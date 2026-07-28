//! Anthropic (Claude) model client via the Messages API.
//!
//! Implements [ModelClient] for Anthropic's native Messages API with SSE
//! streaming support. Unlike OpenAI-compatible providers, Anthropic uses a
//! different request/response format with content blocks and SSE event types.
//!
//! # API Reference
//!
//! - Endpoint: `POST https://api.anthropic.com/v1/messages`
//! - Documentation: <https://docs.anthropic.com/en/api/messages>
//! - Headers: `x-api-key`, `anthropic-version: 2023-06-01`,
//!   `anthropic-beta: tools-2024-04-04`
//!
//! # Features
//!
//! - **Streaming**: SSE events (content_block_start/delta/stop, message_delta,
//!   message_stop) are parsed in real time and emitted as ResponseEvents.
//! - **Retry**: Transient errors (429, 5xx) are retried with exponential
//!   backoff.
//! - **Rate limiting**: Configurable RPM via semaphore-based concurrency
//!   enforcement.
//! - **Token tracking**: Reports input/output tokens from message_start and
//!   message_delta events.

use async_trait::async_trait;
use bytes::Bytes;
use code_agent_protocol::{Message, ResponseEvent, ToolCall, TurnId};
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
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
// Anthropic API types (serialization)
// ---------------------------------------------------------------------------

/// A single content block in an Anthropic message.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type")]
enum AnthropicContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

/// An Anthropic message (user or assistant).
#[derive(Clone, Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicMessageContent,
}

/// Content can be a string (simple text) or an array of content blocks.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
enum AnthropicMessageContent {
    Text(String),
    Blocks(Vec<AnthropicContent>),
}

/// Request body for POST /v1/messages.
#[derive(Clone, Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicToolDef>>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

/// Tool definition for Anthropic.
#[derive(Clone, Debug, Serialize)]
struct AnthropicToolDef {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Anthropic SSE event types (deserialization)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type")]
#[allow(dead_code)]
enum AnthropicEvent {
    #[serde(rename = "message_start")]
    MessageStart {
        message: AnthropicMessageStart,
    },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: u32,
        content_block: AnthropicContentBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        index: u32,
        delta: AnthropicDelta,
    },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: u32 },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: AnthropicMessageDeltaData,
        usage: AnthropicUsage,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    ErrorEvent { error: AnthropicErrorBody },
}

#[derive(Clone, Debug, Deserialize)]
struct AnthropicMessageStart {
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type")]
#[allow(dead_code)]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

#[derive(Clone, Debug, Deserialize)]
struct AnthropicMessageDeltaData {
    #[serde(default)]
    #[allow(dead_code)]
    stop_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

#[derive(Clone, Debug, Deserialize)]
struct AnthropicErrorBody {
    #[serde(default)]
    message: String,
}

// ---------------------------------------------------------------------------
// Stream types
// ---------------------------------------------------------------------------

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin>>;

/// Partial tool call being accumulated across SSE deltas.
struct PartialTool {
    id: String,
    name: String,
    input_json: String,
}

/// Mutable state for SSE parsing.
struct StreamState {
    byte_stream: ByteStream,
    line_buffer: String,
    /// Current SSE event type being accumulated (from `event:` lines).
    current_event_type: String,
    content_buffer: String,
    /// Tool calls being accumulated, indexed by content block index.
    partial_tools: BTreeMap<u32, PartialTool>,
    pending_events: VecDeque<ResponseEvent>,
    /// Accumulated token usage from input side (message_start) and output side
    /// (message_delta).
    input_tokens: u32,
    output_tokens: u32,
    usage_slot: Arc<Mutex<Option<TokenUsage>>>,
    finished: bool,
}

/// Flush all accumulated tool calls and final events.
fn flush_final_events(state: &mut StreamState) {
    // Flush accumulated tool calls
    let partials: Vec<_> = std::mem::take(&mut state.partial_tools)
        .into_iter()
        .collect();
    for (_, pt) in partials {
        let args: serde_json::Value = serde_json::from_str(&pt.input_json)
            .unwrap_or_else(|_| serde_json::Value::String(pt.input_json.clone()));
        state.pending_events.push_back(ResponseEvent::ToolCallBegin {
            tool_call: ToolCall {
                id: pt.id,
                name: pt.name,
                arguments: args,
            },
        });
    }

    // Token usage (input + output)
    if state.input_tokens > 0 || state.output_tokens > 0 {
        state.pending_events.push_back(ResponseEvent::TokenUsage {
            prompt_tokens: state.input_tokens,
            completion_tokens: state.output_tokens,
        });
        if let Ok(mut slot) = state.usage_slot.lock() {
            *slot = Some(TokenUsage::new(state.input_tokens, state.output_tokens));
        }
    }

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
// Message conversion for Anthropic
// ---------------------------------------------------------------------------

/// Convert protocol Messages to Anthropic format.
///
/// Anthropic requires alternating user/assistant messages. Consecutive messages
/// of the same role are merged. System messages are extracted and returned
/// separately.
fn convert_messages_anthropic(messages: &[Message]) -> (Vec<AnthropicMessage>, Option<String>) {
    let system_prompt: Option<String> = None;
    let mut result: Vec<AnthropicMessage> = Vec::new();
    // Track current role being built (None = no current message)
    let mut current_role: Option<String> = None;
    let mut current_blocks: Vec<AnthropicContent> = Vec::new();

    for msg in messages {
        match msg {
            // ── User message ──
            Message::UserMessage { content } => {
                if current_role.as_deref() == Some("user") {
                    // Merge: append another text block
                    current_blocks.push(AnthropicContent::Text {
                        text: content.clone(),
                    });
                } else {
                    // Flush previous role
                    flush_role(&mut result, &mut current_role, &mut current_blocks);
                    current_role = Some("user".to_string());
                    current_blocks.push(AnthropicContent::Text {
                        text: content.clone(),
                    });
                }
            }

            // ── Assistant text message ──
            Message::AssistantMessage { content } => {
                if current_role.as_deref() == Some("assistant") {
                    current_blocks.push(AnthropicContent::Text {
                        text: content.clone(),
                    });
                } else {
                    flush_role(&mut result, &mut current_role, &mut current_blocks);
                    current_role = Some("assistant".to_string());
                    current_blocks.push(AnthropicContent::Text {
                        text: content.clone(),
                    });
                }
            }

            // ── Tool call (emitted as assistant with tool_use block) ──
            Message::ToolCall(tc) => {
                if current_role.as_deref() == Some("assistant") {
                    // Tool call after assistant text — same assistant turn
                    current_blocks.push(AnthropicContent::ToolUse {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        input: tc.arguments.clone(),
                    });
                } else {
                    flush_role(&mut result, &mut current_role, &mut current_blocks);
                    current_role = Some("assistant".to_string());
                    current_blocks.push(AnthropicContent::ToolUse {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        input: tc.arguments.clone(),
                    });
                }
            }

            // ── Tool result (emitted as user with tool_result block) ──
            Message::ToolResult(tr) => {
                if current_role.as_deref() == Some("user") {
                    current_blocks.push(AnthropicContent::ToolResult {
                        tool_use_id: tr.tool_call_id.clone(),
                        content: tr.output.clone().unwrap_or_default(),
                    });
                } else {
                    flush_role(&mut result, &mut current_role, &mut current_blocks);
                    current_role = Some("user".to_string());
                    current_blocks.push(AnthropicContent::ToolResult {
                        tool_use_id: tr.tool_call_id.clone(),
                        content: tr.output.clone().unwrap_or_default(),
                    });
                }
            }
        }
    }

    // Flush last role
    flush_role(&mut result, &mut current_role, &mut current_blocks);

    // Extract system prompt: the protocol doesn't have a dedicated SystemMessage
    // variant; Qwen3 handles system instructions via SessionConfig, not through
    // messages. For Anthropic, we check if the first user message contains a
    // system-like instruction pattern, but the cleaner approach is to not
    // inject a system prompt here. Instead, the caller sets it separately.
    // We return None for system_prompt; sessions inject their own instructions.
    (result, system_prompt)
}

fn flush_role(
    result: &mut Vec<AnthropicMessage>,
    current_role: &mut Option<String>,
    current_blocks: &mut Vec<AnthropicContent>,
) {
    if let Some(role) = current_role.take() {
        let content = std::mem::take(current_blocks);
        if !content.is_empty() {
            let msg_content = if content.len() == 1 {
                match &content[0] {
                    AnthropicContent::Text { text } => {
                        AnthropicMessageContent::Text(text.clone())
                    }
                    _ => AnthropicMessageContent::Blocks(content),
                }
            } else {
                AnthropicMessageContent::Blocks(content)
            };
            result.push(AnthropicMessage {
                role,
                content: msg_content,
            });
        }
    }
}

/// Convert ToolDefinition to Anthropic tool format.
fn convert_tools_anthropic(tools: &[ToolDefinition]) -> Vec<AnthropicToolDef> {
    tools
        .iter()
        .map(|tool| AnthropicToolDef {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.parameters.clone(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// AnthropicClient
// ---------------------------------------------------------------------------

/// Anthropic (Claude) model client via the Messages API.
///
/// Communicates with Anthropic's native Messages API with SSE streaming.
/// Supports tool calling, token tracking, rate limiting, and retry logic.
pub struct AnthropicClient {
    config: ModelConfig,
    http: reqwest::Client,
    last_usage: Arc<Mutex<Option<TokenUsage>>>,
    rate_limiter: Arc<Semaphore>,
    retry_count: AtomicU32,
}

impl AnthropicClient {
    /// Create a new Anthropic chat client from a [ModelConfig].
    ///
    /// The API key is sent as `x-api-key` header. The `api_base_url` should
    /// point to the Anthropic API root (e.g. `https://api.anthropic.com`).
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

    /// Get the number of retry attempts across all calls.
    pub fn retry_count(&self) -> u32 {
        self.retry_count.load(Ordering::Relaxed)
    }

    /// Full URL for the Messages API endpoint.
    fn messages_url(&self) -> String {
        format!(
            "{}/v1/messages",
            self.config.api_base_url.trim_end_matches('/')
        )
    }

    /// Send a request with retry logic.
    async fn execute_with_retry(
        &self,
        request_body: &AnthropicRequest,
    ) -> ModelResult<reqwest::Response> {
        let url = self.messages_url();
        let mut last_error = None;

        for attempt in 0..=self.config.retry.max_retries {
            if attempt > 0 {
                let delay = self.config.retry.delay_for_attempt(attempt - 1);
                warn!(
                    attempt = attempt,
                    delay_ms = delay.as_millis(),
                    "Retrying Anthropic API call"
                );
                self.retry_count.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(delay).await;
            }

            match self.send_request(&url, request_body).await {
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
                    warn!(status, "Anthropic API returned retryable error");
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

    /// Send a single HTTP request (no retry).
    async fn send_request(
        &self,
        url: &str,
        body: &AnthropicRequest,
    ) -> ModelResult<reqwest::Response> {
        let _permit = self.rate_limiter.acquire().await.map_err(|_| {
            ModelError::Other("Rate limiter semaphore closed".into())
        })?;

        debug!(
            url = %url,
            model = %body.model,
            num_messages = body.messages.len(),
            "Sending Anthropic messages request"
        );

        let response = self
            .http
            .post(url)
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "tools-2024-04-04")
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await?;

        Ok(response)
    }

    /// Parse the Anthropic SSE stream into ResponseEvents.
    ///
    /// Anthropic SSE uses both `event:` and `data:` lines. The `event:` line
    /// tells us the event type, and the `data:` line contains the JSON payload.
    fn parse_stream(
        response: reqwest::Response,
        usage_slot: Arc<Mutex<Option<TokenUsage>>>,
    ) -> impl Stream<Item = ResponseEvent> {
        use futures::stream;

        let byte_stream: ByteStream = Box::pin(response.bytes_stream());

        let state = StreamState {
            byte_stream,
            line_buffer: String::new(),
            current_event_type: String::new(),
            content_buffer: String::new(),
            partial_tools: BTreeMap::new(),
            pending_events: VecDeque::new(),
            input_tokens: 0,
            output_tokens: 0,
            usage_slot,
            finished: false,
        };

        let stream = stream::unfold(state, move |mut state| async move {
            loop {
                // 1) Drain pending events first
                if let Some(event) = state.pending_events.pop_front() {
                    return Some((event, state));
                }

                // 2) If finished, end the stream
                if state.finished {
                    return None;
                }

                // 3) Try to extract a complete line
                if let Some(newline_pos) = state.line_buffer.find('\n') {
                    let line = state.line_buffer[..newline_pos].trim().to_string();
                    state.line_buffer = state.line_buffer[newline_pos + 1..].to_string();

                    if line.is_empty() {
                        // Empty line separates SSE events. Process the accumulated event.
                        // (Anthropic SSE doesn't batch events — each event is one event+data pair
                        // separated by empty lines. We process on data line instead.)
                        continue;
                    }

                    if let Some(event_type) = line.strip_prefix("event: ") {
                        state.current_event_type = event_type.trim().to_string();
                        continue;
                    }

                    if let Some(data) = line.strip_prefix("data: ") {
                        let event_json = data.trim();
                        // Ignore empty data lines
                        if event_json.is_empty() {
                            continue;
                        }

                        match serde_json::from_str::<AnthropicEvent>(event_json) {
                            Ok(event) => {
                                match event {
                                    AnthropicEvent::MessageStart { message } => {
                                        if let Some(usage) = message.usage {
                                            state.input_tokens = usage.input_tokens;
                                            state.output_tokens = usage.output_tokens;
                                        }
                                    }
                                    AnthropicEvent::ContentBlockStart {
                                        index,
                                        content_block,
                                    } => match content_block {
                                        AnthropicContentBlock::Text { text: _ } => {
                                            // Text block started — nothing to emit yet
                                        }
                                        AnthropicContentBlock::ToolUse { id, name, input } => {
                                            state.partial_tools.insert(
                                                index,
                                                PartialTool {
                                                    id,
                                                    name,
                                                    input_json: input.to_string(),
                                                },
                                            );
                                        }
                                    },
                                    AnthropicEvent::ContentBlockDelta { index, delta } => {
                                        match delta {
                                            AnthropicDelta::TextDelta { text } => {
                                                if !text.is_empty() {
                                                    state.content_buffer.push_str(&text);
                                                    return Some((
                                                        ResponseEvent::AgentMessageDelta {
                                                            content: text,
                                                        },
                                                        state,
                                                    ));
                                                }
                                            }
                                            AnthropicDelta::InputJsonDelta { partial_json } => {
                                                if let Some(pt) =
                                                    state.partial_tools.get_mut(&index)
                                                {
                                                    pt.input_json.push_str(&partial_json);
                                                }
                                            }
                                        }
                                    }
                                    AnthropicEvent::ContentBlockStop { index } => {
                                        // Content block finished. If it was a tool_use block,
                                        // emit the tool call now (flush at end of stream for
                                        // consistency with Qwen3's pattern).
                                        let _ = index;
                                    }
                                    AnthropicEvent::MessageDelta {
                                        delta: _,
                                        usage,
                                    } => {
                                        state.output_tokens += usage.output_tokens;
                                    }
                                    AnthropicEvent::MessageStop => {
                                        state.finished = true;
                                        flush_final_events(&mut state);
                                        continue;
                                    }
                                    AnthropicEvent::Ping => {
                                        // Ping — ignore
                                    }
                                    AnthropicEvent::ErrorEvent { error } => {
                                        state.pending_events.push_back(
                                            ResponseEvent::Error {
                                                message: format!(
                                                    "Anthropic API error: {}",
                                                    error.message
                                                ),
                                            },
                                        );
                                        state.finished = true;
                                        flush_final_events(&mut state);
                                        continue;
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    data = %event_json,
                                    "Failed to parse Anthropic SSE event"
                                );
                                // Non-fatal: skip unparseable event
                            }
                        }
                        continue;
                    }

                    // Non-event, non-data, non-empty line — ignore
                    continue;
                }

                // 4) Read more bytes from stream
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
impl ModelClient for AnthropicClient {
    fn model_name(&self) -> &str {
        &self.config.model
    }

    async fn complete_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        temperature: Option<f32>,
    ) -> ModelResult<Box<dyn Stream<Item = ResponseEvent> + Send + Unpin>> {
        let (anthropic_messages, _system) = convert_messages_anthropic(messages);

        let request = AnthropicRequest {
            model: self.config.model.clone(),
            max_tokens: self.config.max_tokens,
            system: None, // System instructions handled at session level
            messages: anthropic_messages,
            tools: if tools.is_empty() {
                None
            } else {
                Some(convert_tools_anthropic(tools))
            },
            stream: true,
            temperature: Some(temperature.unwrap_or(self.config.temperature)),
        };

        let response = self.execute_with_retry(&request).await?;
        let stream = Self::parse_stream(response, Arc::clone(&self.last_usage));

        Ok(Box::new(stream))
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        temperature: Option<f32>,
    ) -> ModelResult<String> {
        let (anthropic_messages, _system) = convert_messages_anthropic(messages);

        let request = AnthropicRequest {
            model: self.config.model.clone(),
            max_tokens: self.config.max_tokens,
            system: None,
            messages: anthropic_messages,
            tools: if tools.is_empty() {
                None
            } else {
                Some(convert_tools_anthropic(tools))
            },
            stream: false,
            temperature: Some(temperature.unwrap_or(self.config.temperature)),
        };

        let response = self.execute_with_retry(&request).await?;

        #[derive(Deserialize)]
        struct AnthropicResponse {
            content: Vec<AnthropicResponseContent>,
            #[serde(default)]
            usage: Option<AnthropicUsage>,
        }

        #[derive(Deserialize)]
        #[serde(tag = "type")]
        enum AnthropicResponseContent {
            #[serde(rename = "text")]
            Text { text: String },
            #[serde(rename = "tool_use")]
            ToolUse {
                #[allow(dead_code)]
                id: Option<String>,
                #[allow(dead_code)]
                name: Option<String>,
                #[allow(dead_code)]
                input: Option<serde_json::Value>,
            },
        }

        let body: AnthropicResponse = response.json().await?;

        // Update token usage
        if let Some(usage) = &body.usage {
            if let Ok(mut lu) = self.last_usage.lock() {
                *lu = Some(TokenUsage::new(usage.input_tokens, usage.output_tokens));
            }
        }

        let content = body
            .content
            .iter()
            .filter_map(|c| match c {
                AnthropicResponseContent::Text { text } => Some(text.as_str()),
                AnthropicResponseContent::ToolUse { .. } => None,
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

/// 内置 Anthropic (Claude) 供应商 —— 通过 [`ProviderRegistry`] 注册后自动可用
///
/// [`ProviderRegistry`]: crate::model::ProviderRegistry
pub struct AnthropicProvider;

impl ModelProvider for AnthropicProvider {
    fn provider_kind(&self) -> ProviderKind {
        ProviderKind::Anthropic
    }

    fn create_client(&self, config: ModelConfig) -> Arc<dyn ModelClient> {
        Arc::new(AnthropicClient::new(config))
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
    fn convert_simple_user_message() {
        let msgs = vec![Message::UserMessage {
            content: "Hello".into(),
        }];
        let (anthropic_msgs, system) = convert_messages_anthropic(&msgs);
        assert!(system.is_none());
        assert_eq!(anthropic_msgs.len(), 1);
        assert_eq!(anthropic_msgs[0].role, "user");
    }

    #[test]
    fn convert_user_assistant_alternation() {
        let msgs = vec![
            Message::UserMessage {
                content: "Hi".into(),
            },
            Message::AssistantMessage {
                content: "Hello!".into(),
            },
            Message::UserMessage {
                content: "How are you?".into(),
            },
        ];
        let (anthropic_msgs, _) = convert_messages_anthropic(&msgs);
        assert_eq!(anthropic_msgs.len(), 3);
        assert_eq!(anthropic_msgs[0].role, "user");
        assert_eq!(anthropic_msgs[1].role, "assistant");
        assert_eq!(anthropic_msgs[2].role, "user");
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
        let (anthropic_msgs, _) = convert_messages_anthropic(&msgs);
        // Consecutive same-role messages should be merged
        assert_eq!(anthropic_msgs.len(), 1);
        assert_eq!(anthropic_msgs[0].role, "user");
    }

    #[test]
    fn convert_tool_call_to_assistant_with_tool_use() {
        let tc = code_agent_protocol::ToolCall {
            id: "toolu_001".into(),
            name: "read_file".into(),
            arguments: json!({"path": "src/main.rs"}),
        };
        let msgs = vec![Message::ToolCall(tc)];
        let (anthropic_msgs, _) = convert_messages_anthropic(&msgs);
        assert_eq!(anthropic_msgs.len(), 1);
        assert_eq!(anthropic_msgs[0].role, "assistant");
    }

    #[test]
    fn convert_tool_result_to_user_with_tool_result() {
        let tr = code_agent_protocol::ToolResultMessage {
            tool_call_id: "toolu_001".into(),
            output: Some("file contents".into()),
            error: None,
        };
        let msgs = vec![Message::ToolResult(tr)];
        let (anthropic_msgs, _) = convert_messages_anthropic(&msgs);
        assert_eq!(anthropic_msgs.len(), 1);
        assert_eq!(anthropic_msgs[0].role, "user");
    }

    // ── Tool conversion ──

    #[test]
    fn convert_tools_to_anthropic_format() {
        let tools = vec![ToolDefinition::new(
            "search",
            "Search the codebase",
            json!({"type": "object", "properties": {"query": {"type": "string"}}}),
        )];
        let anthropic_tools = convert_tools_anthropic(&tools);
        assert_eq!(anthropic_tools.len(), 1);
        assert_eq!(anthropic_tools[0].name, "search");
        assert_eq!(anthropic_tools[0].description, "Search the codebase");
        assert!(anthropic_tools[0]
            .input_schema
            .to_string()
            .contains("query"));
    }

    #[test]
    fn convert_tools_empty() {
        let tools: Vec<ToolDefinition> = vec![];
        let anthropic_tools = convert_tools_anthropic(&tools);
        assert!(anthropic_tools.is_empty());
    }

    // ── AnthropicEvent deserialization ──

    #[test]
    fn deserialize_text_delta_event() {
        let json = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let event: AnthropicEvent =
            serde_json::from_str(json).expect("deserialize");
        match event {
            AnthropicEvent::ContentBlockDelta { index, delta } => {
                assert_eq!(index, 0);
                match delta {
                    AnthropicDelta::TextDelta { text } => assert_eq!(text, "Hello"),
                    _ => panic!("expected TextDelta"),
                }
            }
            _ => panic!("expected ContentBlockDelta"),
        }
    }

    #[test]
    fn deserialize_tool_use_block() {
        let json = r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_xyz","name":"read_file","input":{}}}"#;
        let event: AnthropicEvent =
            serde_json::from_str(json).expect("deserialize");
        match event {
            AnthropicEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                assert_eq!(index, 1);
                match content_block {
                    AnthropicContentBlock::ToolUse { id, name, .. } => {
                        assert_eq!(id, "toolu_xyz");
                        assert_eq!(name, "read_file");
                    }
                    _ => panic!("expected ToolUse"),
                }
            }
            _ => panic!("expected ContentBlockStart"),
        }
    }

    #[test]
    fn deserialize_message_stop() {
        let json = r#"{"type":"message_stop"}"#;
        let event: AnthropicEvent =
            serde_json::from_str(json).expect("deserialize");
        assert!(matches!(event, AnthropicEvent::MessageStop));
    }

    #[test]
    fn deserialize_error_event() {
        let json = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Invalid model"}}"#;
        let event: AnthropicEvent =
            serde_json::from_str(json).expect("deserialize");
        match event {
            AnthropicEvent::ErrorEvent { error } => {
                assert!(error.message.contains("Invalid model"));
            }
            _ => panic!("expected ErrorEvent"),
        }
    }

    // ── Client construction ──

    #[test]
    fn client_construction() {
        let config = ModelConfig::builder()
            .api_key("sk-ant-test".into())
            .model("claude-sonnet-4-20250514".into())
            .api_base_url("https://api.anthropic.com".into())
            .build()
            .expect("config");
        let client = AnthropicClient::new(config);
        assert_eq!(client.model_name(), "claude-sonnet-4-20250514");
        assert_eq!(client.retry_count(), 0);
        assert!(client.last_token_usage().is_none());
    }

    // ── Model name ──

    #[test]
    fn model_name_returns_configured_model() {
        let config = ModelConfig::builder()
            .api_key("sk-ant-test".into())
            .model("claude-opus-4-20250514".into())
            .build()
            .expect("config");
        let client = AnthropicClient::new(config);
        assert_eq!(client.model_name(), "claude-opus-4-20250514");
    }
}
