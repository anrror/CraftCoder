//! Google Gemini model client via the generateContent API.
//!
//! Implements [ModelClient] for Google Gemini's native API with SSE streaming
//! support. Gemini uses a different request/response format with `contents`
//! arrays of `parts` and `functionCall`/`functionResponse` part types.
//!
//! # API Reference
//!
//! - Endpoint: `POST /v1beta/models/{model}:streamGenerateContent?alt=sse&key=`
//! - Documentation: <https://ai.google.dev/api/generate-content>
//!
//! # Features
//!
//! - **Streaming**: SSE `data:` lines are parsed in real time and emitted as
//!   ResponseEvents.
//! - **Retry**: Transient errors (429, 5xx) are retried with exponential
//!   backoff.
//! - **Rate limiting**: Configurable RPM via semaphore-based concurrency
//!   enforcement.
//! - **Token tracking**: Reports prompt/completion tokens from usageMetadata.

use async_trait::async_trait;
use bytes::Bytes;
use code_agent_protocol::{Message, ResponseEvent, ToolCall, TurnId};
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use tracing::{debug, warn};

use crate::model::config::ModelConfig;
use crate::model::types::{TokenUsage, ToolDefinition};
use crate::model::{
    build_http_client, is_retryable_status, ModelClient, ModelError, ModelProvider, ModelResult,
    ProviderKind,
};

// ---------------------------------------------------------------------------
// Gemini API types (serialization)
// ---------------------------------------------------------------------------

/// A single part within Gemini content.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
enum GeminiPart {
    Text { text: String },
    #[serde(rename_all = "camelCase")]
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: GeminiFunctionCall,
    },
    #[serde(rename_all = "camelCase")]
    FunctionResponse {
        #[serde(rename = "functionResponse")]
        function_response: GeminiFunctionResponse,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GeminiFunctionCall {
    name: String,
    #[serde(default)]
    args: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GeminiFunctionResponse {
    name: String,
    response: GeminiFunctionResponseContent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GeminiFunctionResponseContent {
    name: String,
    content: String,
}

/// A content block (user or model turn).
#[derive(Clone, Debug, Serialize)]
struct GeminiContent {
    role: String,
    parts: Vec<GeminiPart>,
}

/// System instruction wrapper.
#[derive(Clone, Debug, Serialize)]
struct GeminiSystemInstruction {
    parts: GeminiSystemParts,
}

#[derive(Clone, Debug, Serialize)]
struct GeminiSystemParts {
    text: String,
}

/// Request body for generateContent.
#[derive(Clone, Debug, Serialize)]
struct GeminiRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiSystemInstruction>,
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiToolWrapper>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "generationConfig")]
    generation_config: Option<GeminiGenerationConfig>,
}

#[derive(Clone, Debug, Serialize)]
struct GeminiToolWrapper {
    #[serde(rename = "functionDeclarations")]
    function_declarations: Vec<GeminiFunctionDecl>,
}

#[derive(Clone, Debug, Serialize)]
struct GeminiFunctionDecl {
    name: String,
    description: String,
    #[serde(default)]
    parameters: serde_json::Value,
}

#[derive(Clone, Debug, Serialize)]
struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Gemini SSE response types (deserialization)
// ---------------------------------------------------------------------------

/// Top-level SSE chunk (wraps the actual response in `data:`).
#[derive(Clone, Debug, Deserialize)]
struct GeminiStreamChunk {
    #[serde(default)]
    candidates: Vec<GeminiCandidate>,
    #[serde(default)]
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Clone, Debug, Deserialize)]
struct GeminiCandidate {
    #[serde(default)]
    content: Option<GeminiResponseContent>,
    #[serde(default)]
    #[serde(rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
struct GeminiResponseContent {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    parts: Vec<GeminiResponsePart>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum GeminiResponsePart {
    Text { text: String },
    #[serde(rename_all = "camelCase")]
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: GeminiFunctionCall,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
struct GeminiUsageMetadata {
    #[serde(default)]
    #[serde(rename = "promptTokenCount")]
    prompt_token_count: u32,
    #[serde(default)]
    #[serde(rename = "candidatesTokenCount")]
    candidates_token_count: u32,
    #[serde(default)]
    #[serde(rename = "totalTokenCount")]
    total_token_count: u32,
}

/// Non-streaming response for `complete()`.
#[derive(Clone, Debug, Deserialize)]
struct GeminiResponse {
    #[serde(default)]
    candidates: Vec<GeminiCandidate>,
    #[serde(default)]
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsageMetadata>,
}

// ---------------------------------------------------------------------------
// Stream types
// ---------------------------------------------------------------------------

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin>>;

struct StreamState {
    byte_stream: ByteStream,
    line_buffer: String,
    content_buffer: String,
    /// Pending tool calls to emit at stream end.
    pending_tool_calls: Vec<ToolCall>,
    pending_events: VecDeque<ResponseEvent>,
    usage_slot: Arc<Mutex<Option<TokenUsage>>>,
    finished: bool,
}

fn flush_final_events(state: &mut StreamState) {
    // Emit accumulated tool calls
    let tool_calls: Vec<_> = std::mem::take(&mut state.pending_tool_calls);
    for tc in tool_calls {
        state
            .pending_events
            .push_back(ResponseEvent::ToolCallBegin { tool_call: tc });
    }

    // We don't emit TokenUsage here because Gemini sends usageMetadata in
    // the final chunk, not separately. But we store whatever we have.
    // Token usage was already emitted inline if present.

    // TurnComplete
    let final_content = std::mem::take(&mut state.content_buffer);
    state.pending_events.push_back(ResponseEvent::TurnComplete {
        turn_id: TurnId::from("turn-0"),
        final_message: Message::AssistantMessage {
            content: final_content,
        },
    });
}

// ---------------------------------------------------------------------------
// Message conversion for Gemini
// ---------------------------------------------------------------------------

/// Convert protocol Messages to Gemini format.
///
/// Gemini uses "user" and "model" roles (not "assistant").
/// System instructions are passed separately.
fn convert_messages_gemini(
    messages: &[Message],
) -> (Vec<GeminiContent>, Option<String>) {
    let system_prompt: Option<String> = None;
    let mut result: Vec<GeminiContent> = Vec::new();
    let mut current_role: Option<String> = None;
    let mut current_parts: Vec<GeminiPart> = Vec::new();

    for msg in messages {
        match msg {
            Message::UserMessage { content } => {
                if current_role.as_deref() == Some("user") {
                    current_parts.push(GeminiPart::Text {
                        text: content.clone(),
                    });
                } else {
                    flush_gemini_role(&mut result, &mut current_role, &mut current_parts);
                    current_role = Some("user".to_string());
                    current_parts.push(GeminiPart::Text {
                        text: content.clone(),
                    });
                }
            }

            Message::AssistantMessage { content } => {
                if current_role.as_deref() == Some("model") {
                    current_parts.push(GeminiPart::Text {
                        text: content.clone(),
                    });
                } else {
                    flush_gemini_role(&mut result, &mut current_role, &mut current_parts);
                    current_role = Some("model".to_string());
                    current_parts.push(GeminiPart::Text {
                        text: content.clone(),
                    });
                }
            }

            Message::ToolCall(tc) => {
                if current_role.as_deref() == Some("model") {
                    current_parts.push(GeminiPart::FunctionCall {
                        function_call: GeminiFunctionCall {
                            name: tc.name.clone(),
                            args: tc.arguments.clone(),
                        },
                    });
                } else {
                    flush_gemini_role(&mut result, &mut current_role, &mut current_parts);
                    current_role = Some("model".to_string());
                    current_parts.push(GeminiPart::FunctionCall {
                        function_call: GeminiFunctionCall {
                            name: tc.name.clone(),
                            args: tc.arguments.clone(),
                        },
                    });
                }
            }

            Message::ToolResult(tr) => {
                let fn_name = "unknown".to_string(); // We don't have tool name in ToolResultMessage
                if current_role.as_deref() == Some("user") {
                    current_parts.push(GeminiPart::FunctionResponse {
                        function_response: GeminiFunctionResponse {
                            name: fn_name.clone(),
                            response: GeminiFunctionResponseContent {
                                name: fn_name,
                                content: tr.output.clone().unwrap_or_default(),
                            },
                        },
                    });
                } else {
                    flush_gemini_role(&mut result, &mut current_role, &mut current_parts);
                    current_role = Some("user".to_string());
                    current_parts.push(GeminiPart::FunctionResponse {
                        function_response: GeminiFunctionResponse {
                            name: fn_name.clone(),
                            response: GeminiFunctionResponseContent {
                                name: fn_name,
                                content: tr.output.clone().unwrap_or_default(),
                            },
                        },
                    });
                }
            }
        }
    }

    flush_gemini_role(&mut result, &mut current_role, &mut current_parts);

    (result, system_prompt)
}

fn flush_gemini_role(
    result: &mut Vec<GeminiContent>,
    current_role: &mut Option<String>,
    current_parts: &mut Vec<GeminiPart>,
) {
    if let Some(role) = current_role.take() {
        let parts = std::mem::take(current_parts);
        if !parts.is_empty() {
            result.push(GeminiContent { role, parts });
        }
    }
}

/// Convert ToolDefinition to Gemini function declarations.
fn convert_tools_gemini(tools: &[ToolDefinition]) -> Vec<GeminiToolWrapper> {
    if tools.is_empty() {
        return vec![];
    }
    let declarations: Vec<GeminiFunctionDecl> = tools
        .iter()
        .map(|tool| GeminiFunctionDecl {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters.clone(),
        })
        .collect();
    vec![GeminiToolWrapper {
        function_declarations: declarations,
    }]
}

// ---------------------------------------------------------------------------
// GeminiClient
// ---------------------------------------------------------------------------

/// Google Gemini model client via the generateContent API.
///
/// Communicates with Google's native Gemini API with SSE streaming.
/// The API key is passed as a query parameter (`?key=`).
pub struct GeminiClient {
    config: ModelConfig,
    http: reqwest::Client,
    last_usage: Arc<Mutex<Option<TokenUsage>>>,
    rate_limiter: Arc<Semaphore>,
    retry_count: AtomicU32,
}

impl GeminiClient {
    /// Create a new Gemini chat client from a [ModelConfig].
    ///
    /// The `api_base_url` should point to the Gemini API base, e.g.
    /// `https://generativelanguage.googleapis.com/v1beta`.
    /// The API key is sent as a query parameter `?key=`.
    pub fn new(config: ModelConfig) -> Self {
        let max_concurrent = config
            .rate_limit
            .requests_per_minute
            .map(|rpm| rpm.max(1))
            .unwrap_or(60);
        Self {
            config,
            http: build_http_client(),
            last_usage: Arc::new(Mutex::new(None)),
            rate_limiter: Arc::new(Semaphore::new(max_concurrent as usize)),
            retry_count: AtomicU32::new(0),
        }
    }

    pub fn retry_count(&self) -> u32 {
        self.retry_count.load(Ordering::Relaxed)
    }

    /// Full URL for the streaming generateContent endpoint.
    fn stream_url(&self) -> String {
        let base = self.config.api_base_url.trim_end_matches('/');
        format!(
            "{}/models/{}:streamGenerateContent?alt=sse&key={}",
            base, self.config.model, self.config.api_key
        )
    }

    /// Full URL for the non-streaming generateContent endpoint.
    fn generate_url(&self) -> String {
        let base = self.config.api_base_url.trim_end_matches('/');
        format!(
            "{}/models/{}:generateContent?key={}",
            base, self.config.model, self.config.api_key
        )
    }

    async fn execute_with_retry(
        &self,
        request_body: &GeminiRequest,
        url: &str,
    ) -> ModelResult<reqwest::Response> {
        let mut last_error = None;

        for attempt in 0..=self.config.retry.max_retries {
            if attempt > 0 {
                let delay = self.config.retry.delay_for_attempt(attempt - 1);
                warn!(
                    attempt = attempt,
                    delay_ms = delay.as_millis(),
                    "Retrying Gemini API call"
                );
                self.retry_count.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(delay).await;
            }

            match self.send_request(url, request_body).await {
                Ok(response) => {
                    if response.status().is_success() {
                        return Ok(response);
                    }
                    let status = response.status().as_u16();
                    let body = response.text().await.unwrap_or_default();
                    if !is_retryable_status(status) {
                        return Err(ModelError::Api {
                            status,
                            message: body,
                        });
                    }
                    last_error = Some(ModelError::Api {
                        status,
                        message: body,
                    });
                    warn!(status, "Gemini API returned retryable error");
                }
                Err(err) => {
                    let status = match &err {
                        ModelError::Http(e) => e.status().map(|s| s.as_u16()),
                        ModelError::Api { status, .. } => Some(*status),
                        _ => None,
                    }
                    .unwrap_or(500);
                    if !is_retryable_status(status) {
                        return Err(err);
                    }
                    last_error = Some(err);
                }
            }
        }

        Err(last_error
            .unwrap_or_else(|| ModelError::Other("Retry exhausted with no error".into())))
    }

    async fn send_request(
        &self,
        url: &str,
        body: &GeminiRequest,
    ) -> ModelResult<reqwest::Response> {
        let _permit = self.rate_limiter.acquire().await.map_err(|_| {
            ModelError::Other("Rate limiter semaphore closed".into())
        })?;

        debug!(
            url = %url,
            num_contents = body.contents.len(),
            "Sending Gemini generateContent request"
        );

        let response = self
            .http
            .post(url)
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await?;

        Ok(response)
    }

    /// Parse the Gemini SSE stream into ResponseEvents.
    ///
    /// Gemini SSE is simpler than Anthropic: each line is `data: {json}\n\n`.
    /// Each JSON object contains candidates with parts (text or functionCall)
    /// and optional usageMetadata.
    fn parse_stream(
        response: reqwest::Response,
        usage_slot: Arc<Mutex<Option<TokenUsage>>>,
    ) -> impl Stream<Item = ResponseEvent> {
        use futures::stream;

        let byte_stream: ByteStream = Box::pin(response.bytes_stream());

        let state = StreamState {
            byte_stream,
            line_buffer: String::new(),
            content_buffer: String::new(),
            pending_tool_calls: Vec::new(),
            pending_events: VecDeque::new(),
            usage_slot,
            finished: false,
        };

        let stream = stream::unfold(state, move |mut state| async move {
            loop {
                // 1) Drain pending events
                if let Some(event) = state.pending_events.pop_front() {
                    return Some((event, state));
                }

                // 2) If finished, end
                if state.finished {
                    return None;
                }

                // 3) Try to extract a complete line
                if let Some(newline_pos) = state.line_buffer.find('\n') {
                    let line = state.line_buffer[..newline_pos].trim().to_string();
                    state.line_buffer = state.line_buffer[newline_pos + 1..].to_string();

                    // Empty lines are SSE event delimiters
                    if line.is_empty() {
                        continue;
                    }

                    // Parse data: line
                    if let Some(data) = line.strip_prefix("data: ") {
                        let data = data.trim();
                        if data.is_empty() || data == "[DONE]" {
                            state.finished = true;
                            flush_final_events(&mut state);
                            continue;
                        }

                        match serde_json::from_str::<GeminiStreamChunk>(data) {
                            Ok(chunk) => {
                                // Process usage metadata
                                if let Some(usage) = &chunk.usage_metadata {
                                    let token_usage = TokenUsage::new(
                                        usage.prompt_token_count,
                                        usage.candidates_token_count,
                                    );
                                    state.pending_events.push_back(
                                        ResponseEvent::TokenUsage {
                                            prompt_tokens: token_usage.prompt_tokens,
                                            completion_tokens: token_usage
                                                .completion_tokens,
                                        },
                                    );
                                    if let Ok(mut slot) = state.usage_slot.lock() {
                                        *slot = Some(token_usage);
                                    }
                                }

                                // Process candidates
                                for candidate in &chunk.candidates {
                                    if let Some(ref content) = candidate.content {
                                        for part in &content.parts {
                                            match part {
                                                GeminiResponsePart::Text { text } => {
                                                    if !text.is_empty() {
                                                        state
                                                            .content_buffer
                                                            .push_str(text);
                                                        return Some((
                                                            ResponseEvent::AgentMessageDelta {
                                                                content: text.clone(),
                                                            },
                                                            state,
                                                        ));
                                                    }
                                                }
                                                GeminiResponsePart::FunctionCall {
                                                    function_call: fc,
                                                } => {
                                                    let tc = ToolCall {
                                                        id: format!(
                                                            "gemini-{}",
                                                            fc.name
                                                        ),
                                                        name: fc.name.clone(),
                                                        arguments: fc.args.clone(),
                                                    };
                                                    // For Gemini, function calls are
                                                    // complete in one chunk (not
                                                    // streaming), so we can emit
                                                    // immediately. But following the
                                                    // Qwen3 pattern, we accumulate
                                                    // and emit at stream end.
                                                    state.pending_tool_calls.push(tc);
                                                }
                                            }
                                        }
                                    }

                                    // Check finish reason
                                    if let Some(ref reason) = candidate.finish_reason {
                                        if reason == "STOP"
                                            || reason == "MAX_TOKENS"
                                            || reason == "SAFETY"
                                        {
                                            state.finished = true;
                                            flush_final_events(&mut state);
                                            continue;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    data = %data,
                                    "Failed to parse Gemini SSE chunk"
                                );
                                // Non-fatal: skip
                            }
                        }
                        continue;
                    }

                    continue;
                }

                // 4) Read more bytes
                match state.byte_stream.next().await {
                    Some(Ok(bytes)) => {
                        let text = String::from_utf8_lossy(&bytes);
                        state.line_buffer.push_str(&text);
                        continue;
                    }
                    Some(Err(e)) => {
                        state
                            .pending_events
                            .push_back(ResponseEvent::Error {
                                message: format!("Stream read error: {}", e),
                            });
                        state.finished = true;
                        flush_final_events(&mut state);
                        continue;
                    }
                    None => {
                        state.finished = true;
                        flush_final_events(&mut state);
                        continue;
                    }
                }
            }
        });

        Box::pin(stream)
    }
}

// ---------------------------------------------------------------------------
// ModelClient implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl ModelClient for GeminiClient {
    fn model_name(&self) -> &str {
        &self.config.model
    }

    async fn complete_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> ModelResult<Box<dyn Stream<Item = ResponseEvent> + Send + Unpin>> {
        let (gemini_contents, _system) = convert_messages_gemini(messages);

        let request = GeminiRequest {
            system_instruction: None,
            contents: gemini_contents,
            tools: if tools.is_empty() {
                None
            } else {
                Some(convert_tools_gemini(tools))
            },
            generation_config: Some(GeminiGenerationConfig {
                temperature: Some(self.config.temperature),
                max_output_tokens: Some(self.config.max_tokens),
            }),
        };

        let url = self.stream_url();
        let response = self.execute_with_retry(&request, &url).await?;
        let stream = Self::parse_stream(response, Arc::clone(&self.last_usage));

        Ok(Box::new(stream))
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> ModelResult<String> {
        let (gemini_contents, _system) = convert_messages_gemini(messages);

        let request = GeminiRequest {
            system_instruction: None,
            contents: gemini_contents,
            tools: if tools.is_empty() {
                None
            } else {
                Some(convert_tools_gemini(tools))
            },
            generation_config: Some(GeminiGenerationConfig {
                temperature: Some(self.config.temperature),
                max_output_tokens: Some(self.config.max_tokens),
            }),
        };

        let url = self.generate_url();
        let response = self.execute_with_retry(&request, &url).await?;
        let body: GeminiResponse = response.json().await?;

        // Update token usage
        if let Some(usage) = &body.usage_metadata {
            if let Ok(mut lu) = self.last_usage.lock() {
                *lu = Some(TokenUsage::new(
                    usage.prompt_token_count,
                    usage.candidates_token_count,
                ));
            }
        }

        let content = body
            .candidates
            .iter()
            .filter_map(|c| c.content.as_ref())
            .flat_map(|c| &c.parts)
            .filter_map(|part| match part {
                GeminiResponsePart::Text { text } => Some(text.as_str()),
                GeminiResponsePart::FunctionCall { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("");

        Ok(content)
    }

    fn last_token_usage(&self) -> Option<TokenUsage> {
        self.last_usage.lock().ok()?.clone()
    }
}

// ---------------------------------------------------------------------------
// ModelProvider impl — enables ProviderRegistry-based factory dispatch
// ---------------------------------------------------------------------------

/// 内置 Gemini 供应商 —— 通过 [`ProviderRegistry`] 注册后自动可用
///
/// [`ProviderRegistry`]: crate::model::ProviderRegistry
pub struct GeminiProvider;

impl ModelProvider for GeminiProvider {
    fn provider_kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn create_client(&self, config: ModelConfig) -> Arc<dyn ModelClient> {
        Arc::new(GeminiClient::new(config))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Message conversion ──

    #[test]
    fn convert_user_message_to_gemini() {
        let msgs = vec![Message::UserMessage {
            content: "Hello".into(),
        }];
        let (contents, system) = convert_messages_gemini(&msgs);
        assert!(system.is_none());
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, "user");
        assert_eq!(contents[0].parts.len(), 1);
    }

    #[test]
    fn convert_assistant_message_to_model_role() {
        let msgs = vec![Message::AssistantMessage {
            content: "Hi!".into(),
        }];
        let (contents, _) = convert_messages_gemini(&msgs);
        assert_eq!(contents.len(), 1);
        // Assistant messages become "model" role in Gemini
        assert_eq!(contents[0].role, "model");
    }

    #[test]
    fn convert_merges_consecutive_user_messages() {
        let msgs = vec![
            Message::UserMessage {
                content: "First".into(),
            },
            Message::UserMessage {
                content: "Second".into(),
            },
        ];
        let (contents, _) = convert_messages_gemini(&msgs);
        // Merged into one message with two parts
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, "user");
        assert_eq!(contents[0].parts.len(), 2);
    }

    #[test]
    fn convert_tool_call_to_function_call() {
        let tc = code_agent_protocol::ToolCall {
            id: "call-1".into(),
            name: "search".into(),
            arguments: json!({"query": "rust"}),
        };
        let msgs = vec![Message::ToolCall(tc)];
        let (contents, _) = convert_messages_gemini(&msgs);
        assert_eq!(contents.len(), 1);
        // Tool calls become "model" role with functionCall part
        assert_eq!(contents[0].role, "model");
    }

    #[test]
    fn convert_tool_result_to_function_response() {
        let tr = code_agent_protocol::ToolResultMessage {
            tool_call_id: "call-1".into(),
            output: Some("result".into()),
            error: None,
        };
        let msgs = vec![Message::ToolResult(tr)];
        let (contents, _) = convert_messages_gemini(&msgs);
        // Tool results become "user" role with functionResponse part
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, "user");
    }

    // ── Tool conversion ──

    #[test]
    fn convert_tools_to_gemini_format() {
        let tools = vec![ToolDefinition::new(
            "search",
            "Search code",
            json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        )];
        let wrappers = convert_tools_gemini(&tools);
        assert_eq!(wrappers.len(), 1);
        let decls = &wrappers[0].function_declarations;
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].name, "search");
    }

    #[test]
    fn convert_tools_empty() {
        let wrappers = convert_tools_gemini(&[]);
        assert!(wrappers.is_empty());
    }

    // ── SSE chunk deserialization ──

    #[test]
    fn deserialize_text_chunk() {
        let json = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello world"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":3,"totalTokenCount":8}}"#;
        let chunk: GeminiStreamChunk =
            serde_json::from_str(json).expect("deserialize");
        assert_eq!(chunk.candidates.len(), 1);
        let usage = chunk.usage_metadata.expect("usage");
        assert_eq!(usage.prompt_token_count, 5);
        assert_eq!(usage.candidates_token_count, 3);
    }

    #[test]
    fn deserialize_function_call_chunk() {
        let json = r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"search","args":{"query":"rust"}}}]},"finishReason":"STOP"}]}"#;
        let chunk: GeminiStreamChunk =
            serde_json::from_str(json).expect("deserialize");
        assert_eq!(chunk.candidates.len(), 1);
        let parts = &chunk.candidates[0]
            .content
            .as_ref()
            .expect("content")
            .parts;
        assert_eq!(parts.len(), 1);
        match &parts[0] {
            GeminiResponsePart::FunctionCall { function_call: fc } => {
                assert_eq!(fc.name, "search");
                assert_eq!(fc.args, json!({"query": "rust"}));
            }
            _ => panic!("expected FunctionCall"),
        }
    }

    // ── Client construction ──

    #[test]
    fn client_construction() {
        let config = ModelConfig::builder()
            .api_key("gemini-api-key".into())
            .model("gemini-2.5-pro".into())
            .api_base_url("https://generativelanguage.googleapis.com/v1beta".into())
            .build()
            .expect("config");
        let client = GeminiClient::new(config);
        assert_eq!(client.model_name(), "gemini-2.5-pro");
        assert_eq!(client.retry_count(), 0);
        assert!(client.last_token_usage().is_none());
    }

    // ── URL construction ──

    #[test]
    fn stream_url_contains_model_and_key() {
        let config = ModelConfig::builder()
            .api_key("test-key".into())
            .model("gemini-pro".into())
            .api_base_url("https://api.example.com/v1beta".into())
            .build()
            .expect("config");
        let client = GeminiClient::new(config);
        let url = client.stream_url();
        assert!(url.contains("gemini-pro"));
        assert!(url.contains("test-key"));
        assert!(url.contains("streamGenerateContent"));
        assert!(url.contains("alt=sse"));
    }

    #[test]
    fn generate_url_no_sse_param() {
        let config = ModelConfig::builder()
            .api_key("test-key".into())
            .model("gemini-pro".into())
            .api_base_url("https://api.example.com/v1beta".into())
            .build()
            .expect("config");
        let client = GeminiClient::new(config);
        let url = client.generate_url();
        assert!(url.contains("generateContent"));
        assert!(!url.contains("streamGenerateContent"));
        assert!(!url.contains("alt=sse"));
    }
}
