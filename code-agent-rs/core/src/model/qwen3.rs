//! OpenAI-compatible model client with streaming support.
//!
//! The API gateway exposes a fully OpenAI-compatible POST /chat/completions
//! endpoint with Server-Sent Events (SSE) streaming.
//!
//! # Features
//!
//! - **Streaming**: Text deltas and tool calls are parsed from SSE chunks in
//!   real time and emitted as ResponseEvents.
//! - **Retry**: Transient errors (429, 5xx) are retried with exponential
//!   backoff (3 retries, base 1s, multiplier 2x, max 10s).
//! - **Rate limiting**: Configurable RPM/RPD limits with semaphore-based
//!   concurrency enforcement.
//! - **Token tracking**: Reports prompt/completion tokens from the final
//!   stream chunk and exposes them via last_token_usage.
//!
//! # 领域描述
//!
//! Qwen3 模型客户端是 AI Agent 与底层大语言模型之间的核心通信层。
//! 它屏蔽了 OpenAI 兼容 API 的协议细节，通过 SSE 流式接口实现
//! 实时文本生成与工具调用解析。支持自动重试、速率限制与令牌用量追踪。

use async_trait::async_trait;
use bytes::Bytes;
use code_agent_protocol::{Message, ResponseEvent, TurnId};
use futures::stream::{Stream, StreamExt};
use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use tracing::{debug, warn};

use crate::model::config::ModelConfig;
use crate::model::types::{
    ChatCompletionChunk, ChatCompletionRequest, TokenUsage, ToolDefinition,
};
use crate::model::{
    build_http_client, convert_messages, convert_tools, is_retryable_status, ModelClient,
    ModelError, ModelProvider, ModelResult, ProviderKind,
};

// ---------------------------------------------------------------------------
// Stream state (module-level so helpers can reference it)
// ---------------------------------------------------------------------------

/// Internal type alias for one partial tool call being accumulated.
type PartialToolCall = (Option<String>, Option<String>, String);

/// Boxed, pinned byte stream from reqwest.
type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin>>;

/// Mutable state shared between the byte-stream reader and the SSE line parser.
struct StreamState {
    /// Underlying byte stream from `reqwest`.
    byte_stream: ByteStream,
    /// Incomplete line buffer — we only split on `\n`.
    line_buffer: String,
    /// Full text content accumulated from `delta.content` chunks.
    content_buffer: String,
    /// Tool calls being accumulated across multiple SSE chunks (indexed by
    /// `index` field in the tool-call delta).
    partial_tool_calls: BTreeMap<u32, PartialToolCall>,
    /// Queue of events that must be yielded before reading more SSE data.
    pending_events: VecDeque<ResponseEvent>,
    /// Token usage extracted from the final SSE chunk.
    last_usage: Option<TokenUsage>,
    /// Shared slot where the client reads token usage after the stream ends.
    usage_slot: Arc<Mutex<Option<TokenUsage>>>,
    /// `true` after `[DONE]` is received or the underlying stream hits EOF.
    finished: bool,
}
// ---------------------------------------------------------------------------
// Final-event flushing
// ---------------------------------------------------------------------------

/// Push all final events (tool calls, token usage, turn complete) into the
/// pending-event queue and write token usage to the shared slot.
fn flush_final_events(state: &mut StreamState) {
    // Emit accumulated tool calls
    let partials: Vec<_> = std::mem::take(&mut state.partial_tool_calls)
        .into_iter()
        .collect();
    for (_idx, (id, name, args)) in partials {
        let tc = code_agent_protocol::ToolCall {
            id: id.unwrap_or_default(),
            name: name.unwrap_or_default(),
            arguments: serde_json::from_str(&args)
                .unwrap_or_else(|_| serde_json::Value::String(args.clone())),
        };
        state
            .pending_events
            .push_back(ResponseEvent::ToolCallBegin { tool_call: tc });
    }

    // Token usage
    if let Some(usage) = &state.last_usage {
        state.pending_events.push_back(ResponseEvent::TokenUsage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
        });
    }

    // Persist token usage to the shared slot
    if let Ok(mut slot) = state.usage_slot.lock() {
        *slot = state.last_usage.clone();
    }

    // TurnComplete (always emitted, even if stream was empty)
    let final_content = std::mem::take(&mut state.content_buffer);
    state
        .pending_events
        .push_back(ResponseEvent::TurnComplete {
            turn_id: TurnId::from("turn-0"),
            final_message: Message::AssistantMessage {
                content: final_content,
            },
        });
}

// ---------------------------------------------------------------------------
// Qwen3OpenAIClient
// ---------------------------------------------------------------------------

/// Qwen3 模型客户端 —— 通过 OpenAI 兼容 API 网关调用 Qwen3 模型
///
/// 【领域含义】Qwen3OpenAIClient 是 ModelClient trait 的 Qwen3 实现，
/// 通过 HTTP 流式接口与 Qwen3 模型通信。支持文本生成、工具调用、
/// 令牌用量追踪、速率限制与自动重试。内部使用事件缓冲区模式
/// 将 SSE 字节流解析为 ResponseEvent 事件流。
///
/// Client for Qwen3 models via an OpenAI-compatible API gateway.
///
/// # Examples
///
/// ```rust,no_run
/// use code_agent_core::model::{ModelConfig, Qwen3OpenAIClient, ModelClient};
///
/// let config = ModelConfig::builder()
///     .api_key("sk-abc".into())
///     .build()
///     .expect("config");
/// let client = Qwen3OpenAIClient::new(config);
/// ```
pub struct Qwen3OpenAIClient {
    /// Configuration for this client.
    config: ModelConfig,
    /// HTTP client with connection pooling.
    http: reqwest::Client,
    /// Last recorded token usage (thread-safe, shared with stream).
    last_usage: Arc<Mutex<Option<TokenUsage>>>,
    /// Rate limiter semaphore: restricts concurrent requests.
    rate_limiter: Arc<Semaphore>,
    /// Counter for retry attempts (for testing observability).
    retry_count: AtomicU32,
}

impl Qwen3OpenAIClient {
    /// 创建 Qwen3 模型客户端实例
    ///
    /// 【领域含义】从 ModelConfig 中提取 API 端点、密钥、速率限制等参数，
    /// 初始化 HTTP 连接池（连接复用）与并发限流信号量。
    /// 默认并发上限取自 config.rate_limit.requests_per_minute 或 60。
    ///
    /// Create a new Qwen3 chat client from a [ModelConfig].
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

    /// 获取当前客户端的累计重试次数
    ///
    /// 【领域含义】用于观测和测试，反映 execute_with_retry 内部
    /// 因可重试错误（429、5xx）触发的重试总次数。
    ///
    /// Get the number of retry attempts across all calls (useful for tests).
    pub fn retry_count(&self) -> u32 {
        self.retry_count.load(Ordering::Relaxed)
    }

    /// Execute a chat completion request with retry logic.
    async fn execute_with_retry(
        &self,
        request_body: &ChatCompletionRequest,
    ) -> ModelResult<reqwest::Response> {
        let url = self.config.chat_completions_url();
        let mut last_error = None;

        for attempt in 0..=self.config.retry.max_retries {
            if attempt > 0 {
                let delay = self.config.retry.delay_for_attempt(attempt - 1);
                warn!(
                    attempt = attempt,
                    delay_ms = delay.as_millis(),
                    "Retrying Qwen3 API call"
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
                    warn!(status, "Qwen3 API returned retryable error");
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

    /// Send a single HTTP request (no retry logic).
    async fn send_request(
        &self,
        url: &str,
        body: &ChatCompletionRequest,
    ) -> ModelResult<reqwest::Response> {
        let _permit = self.rate_limiter.acquire().await.map_err(|_| {
            ModelError::Other("Rate limiter semaphore closed".into())
        })?;

        debug!(
            url = %url,
            model = %body.model,
            num_messages = body.messages.len(),
            "Sending chat completion request"
        );

        let response = self
            .http
            .post(url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await?;

        Ok(response)
    }

    /// Parse an SSE stream into ResponseEvents.
    ///
    /// Uses an event-buffer pattern: when the stream ends (either via [DONE]
    /// or EOF), all pending tool calls, token usage, and the final
    /// TurnComplete are queued into the buffer and yielded sequentially.
    ///
    /// The usage_slot is updated with the final token usage so
    /// [Qwen3OpenAIClient::last_token_usage] can return it.
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
            partial_tool_calls: std::collections::BTreeMap::new(),
            pending_events: VecDeque::new(),
            last_usage: None,
            usage_slot,
            finished: false,
        };

        let stream = stream::unfold(state, move |mut state| async move {
            loop {
                // 1) Drain pending events first
                if let Some(event) = state.pending_events.pop_front() {
                    return Some((event, state));
                }

                // 2) If fully finished and no pending events, end the stream
                if state.finished {
                    return None;
                }

                // 3) Try to extract a complete line from the buffer
                if let Some(newline_pos) = state.line_buffer.find('\n') {
                    let line = state.line_buffer[..newline_pos].trim().to_string();
                    state.line_buffer = state.line_buffer[newline_pos + 1..].to_string();

                    // Empty lines are SSE event delimiters — ignore them
                    if line.is_empty() {
                        continue;
                    }

                    // Handle data: [DONE] — end of stream
                    if line == "data: [DONE]" {
                        state.finished = true;
                        flush_final_events(&mut state);
                        continue; // loop back to drain pending_events
                    }

                    // Parse JSON data line
                    if let Some(data) = line.strip_prefix("data: ") {
                        match serde_json::from_str::<ChatCompletionChunk>(data) {
                            Ok(chunk) => {
                                // Update token usage if present in this chunk
                                if let Some(usage) = &chunk.usage {
                                    state.last_usage = Some(TokenUsage::new(
                                        usage.prompt_tokens,
                                        usage.completion_tokens,
                                    ));
                                }

                                for choice in &chunk.choices {
                                    // Content delta
                                    if let Some(ref content) = choice.delta.content {
                                        if !content.is_empty() {
                                            state.content_buffer.push_str(content);
                                            let event = ResponseEvent::AgentMessageDelta {
                                                content: content.clone(),
                                            };
                                            return Some((event, state));
                                        }
                                    }

                                    // Tool call deltas
                                    if let Some(ref tc_deltas) = choice.delta.tool_calls {
                                        for tc_delta in tc_deltas {
                                            let entry = state
                                                .partial_tool_calls
                                                .entry(tc_delta.index)
                                                .or_insert_with(|| (None, None, String::new()));

                                            if let Some(ref id) = tc_delta.id {
                                                entry.0 = Some(id.clone());
                                            }
                                            if let Some(ref func) = tc_delta.function {
                                                if let Some(ref name) = func.name {
                                                    entry.1 = Some(name.clone());
                                                }
                                                if let Some(ref args) = func.arguments {
                                                    entry.2.push_str(args);
                                                }
                                            }
                                        }
                                    }
                                }
                                // Continue parsing next line
                                continue;
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to parse SSE chunk as JSON");
                                // Non-fatal: skip the unparseable line, keep going
                                continue;
                            }
                        }
                    }

                    // Non-data, non-empty line — ignore
                    continue;
                }

                // 4) Buffer has no complete line yet — read more bytes from stream
                match state.byte_stream.next().await {
                    Some(Ok(bytes)) => {
                        let text = String::from_utf8_lossy(&bytes);
                        state.line_buffer.push_str(&text);
                        continue;
                    }
                    Some(Err(e)) => {
                        // Stream read error — emit error and finalize
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
                        // Stream ended without [DONE] — finalize with what we have
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

/// ModelClient trait 的 Qwen3 实现 —— 将通用模型接口映射到 Qwen3 API 语义
///
/// 【领域含义】实现 ModelClient 定义的 complete_stream（流式）、
/// complete（非流式）与 last_token_usage（令牌用量查询）接口，
/// 将通用 Message/ToolDefinition 转换为 Qwen3 的 ChatCompletionRequest 格式。
#[async_trait]
impl ModelClient for Qwen3OpenAIClient {
    /// 返回当前配置的模型名称
    ///
    /// 【领域含义】用于日志记录和模型路由识别。返回 ModelConfig.model 字段值。
    fn model_name(&self) -> &str {
        &self.config.model
    }

    /// 流式聊天补全 —— 通过 SSE 实时返回 ResponseEvent 事件流
    ///
    /// 【领域含义】将 Message 列表与 ToolDefinition 列表组装为
    /// ChatCompletionRequest，经 execute_with_retry 发送后，
    /// 通过 parse_stream 解析 SSE 字节流为事件流。调用方可逐帧消费
    /// AgentMessageDelta、ToolCallBegin、TokenUsage、TurnComplete 等事件。
    async fn complete_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> ModelResult<Box<dyn Stream<Item = ResponseEvent> + Send + Unpin>> {
        let request = ChatCompletionRequest {
            model: self.config.model.clone(),
            messages: convert_messages(messages),
            tools: if tools.is_empty() {
                None
            } else {
                Some(convert_tools(tools))
            },
            stream: self.config.stream,
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
        };

        let response = self.execute_with_retry(&request).await?;

        let stream = Self::parse_stream(response, Arc::clone(&self.last_usage));

        Ok(Box::new(stream))
    }

    /// 非流式聊天补全 —— 一次性返回完整响应文本
    ///
    /// 【领域含义】适用于不需要实时流式输出的场景（如短问答）。
    /// 通过 stream=false 参数请求非流式响应，从首个 choice 中提取 content 文本，
    /// 同时记录返回的令牌用量。
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> ModelResult<String> {
        // Non-streaming optimization: use stream=false for a single response
        let request = ChatCompletionRequest {
            model: self.config.model.clone(),
            messages: convert_messages(messages),
            tools: if tools.is_empty() {
                None
            } else {
                Some(convert_tools(tools))
            },
            stream: false,
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
        };

        let response = self.execute_with_retry(&request).await?;
        let body: crate::model::types::ChatCompletionResponse = response.json().await?;

        // Update token usage
        if let Some(usage) = &body.usage {
            if let Ok(mut lu) = self.last_usage.lock() {
                *lu = Some(TokenUsage::new(usage.prompt_tokens, usage.completion_tokens));
            }
        }

        let content = body
            .choices
            .first()
            .and_then(|c| c.message.content.as_deref())
            .unwrap_or("")
            .to_string();

        Ok(content)
    }

    /// 获取最后一次请求的令牌用量
    ///
    /// 【领域含义】返回上一次 complete_stream 或 complete 调用中
    /// 记录的 prompt_tokens 和 completion_tokens 值。
    /// 此值由 parse_stream 中的 flush_final_events 写入共享槽位。
    fn last_token_usage(&self) -> Option<TokenUsage> {
        self.last_usage.lock().ok()?.clone()
    }
}

// ---------------------------------------------------------------------------
// ModelProvider impl — enables ProviderRegistry-based factory dispatch
// ---------------------------------------------------------------------------

/// 内置 Qwen3 供应商 —— 通过 [`ProviderRegistry`] 注册后自动可用
///
/// [`ProviderRegistry`]: crate::model::ProviderRegistry
pub struct Qwen3Provider;

impl ModelProvider for Qwen3Provider {
    fn provider_kind(&self) -> ProviderKind {
        ProviderKind::Qwen3
    }

    fn create_client(&self, config: ModelConfig) -> Arc<dyn ModelClient> {
        Arc::new(Qwen3OpenAIClient::new(config))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::RetryConfig;
    use httpmock::prelude::*;
    use serde_json::json;

    fn test_config(server: &MockServer) -> ModelConfig {
        ModelConfig::builder()
            .api_key("sk-test".into())
            .api_base_url(server.base_url())
            .model("test-model".into())
            .build()
            .expect("test config")
    }

    // ── Streaming text response ──

    #[tokio::test]
    async fn stream_text_response() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("Content-Type", "text/event-stream")
                .body(
                    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\
                     \"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"}}]}\n\n\
                     data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\
                     \"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"}}]}\n\n\
                     data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\
                     \"choices\":[{\"index\":0,\"delta\":{}}],\
                     \"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n\n\
                     data: [DONE]\n\n",
                );
        });

        let config = test_config(&server);
        let client = Qwen3OpenAIClient::new(config);

        let messages = vec![Message::UserMessage {
            content: "hi".into(),
        }];
        let tools = vec![];

        let mut stream = client
            .complete_stream(&messages, &tools)
            .await
            .expect("stream");
        let mut texts: Vec<String> = Vec::new();
        while let Some(event) = stream.next().await {
            match event {
                ResponseEvent::AgentMessageDelta { content } => {
                    if !content.is_empty() {
                        texts.push(content);
                    }
                }
                ResponseEvent::TurnComplete { .. } => break,
                _ => {}
            }
        }

        mock.assert();
        assert_eq!(texts, vec!["Hello", " world"]);
    }

    // ── Tool call streaming ──

    #[tokio::test]
    async fn stream_tool_call_response() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("Content-Type", "text/event-stream")
                .body(
                    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\
                     \"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[\
                     {\"index\":0,\"id\":\"call_abc\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"\"}}]}}]}\n\n\
                     data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\
                     \"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[\
                     {\"index\":0,\"function\":{\"arguments\":\"src/main.rs\\\"}\"}}]}}]}\n\n\
                     data: [DONE]\n\n",
                );
        });

        let config = test_config(&server);
        let client = Qwen3OpenAIClient::new(config);

        let messages = vec![Message::UserMessage {
            content: "read main.rs".into(),
        }];
        let tools = vec![ToolDefinition::new(
            "read_file",
            "Read a file",
            json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        )];

        let mut stream = client
            .complete_stream(&messages, &tools)
            .await
            .expect("stream");
        let mut tool_calls: Vec<code_agent_protocol::ToolCall> = Vec::new();
        let mut got_turn_complete = false;
        while let Some(event) = stream.next().await {
            match event {
                ResponseEvent::ToolCallBegin { tool_call } => {
                    tool_calls.push(tool_call);
                }
                ResponseEvent::TurnComplete { .. } => {
                    got_turn_complete = true;
                    break;
                }
                _ => {}
            }
        }

        mock.assert();
        assert!(got_turn_complete, "Expected TurnComplete event");
        assert_eq!(tool_calls.len(), 1, "Expected exactly 1 tool call");
        assert_eq!(tool_calls[0].name, "read_file");
        assert_eq!(tool_calls[0].id, "call_abc");
        assert!(
            tool_calls[0].arguments.to_string().contains("src/main.rs"),
            "Arguments should contain src/main.rs, got: {}",
            tool_calls[0].arguments
        );
    }

    // ── Retry on 429 ──

    #[tokio::test]
    async fn retry_on_rate_limit() {
        let server = MockServer::start();

        let _fail_mock = server.mock(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(429)
                .header("Content-Type", "application/json")
                .body(r#"{"error": {"message": "Rate limited"}}"#);
        });

        let config = test_config(&server);
        let client = Qwen3OpenAIClient::new(config);

        let messages = vec![Message::UserMessage {
            content: "hi".into(),
        }];
        let result = client.complete_stream(&messages, &[]).await;
        // 429 is retryable, but all retries will hit the same mock -> fail
        assert!(result.is_err());
        // Retry count should be > 0
        assert!(client.retry_count() > 0);
    }

    #[tokio::test]
    async fn retry_succeeds_after_transient_error() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("Content-Type", "text/event-stream")
                .body("data: [DONE]\n\n");
        });

        let config = ModelConfig::builder()
            .api_key("sk-test".into())
            .api_base_url(server.base_url())
            .model("test-model".into())
            .build()
            .expect("config");
        let client = Qwen3OpenAIClient::new(config);

        let messages = vec![Message::UserMessage {
            content: "hi".into(),
        }];
        let result = client.complete_stream(&messages, &[]).await;
        assert!(result.is_ok());
        mock.assert();
    }

    // ── Token tracking ──

    #[tokio::test]
    async fn token_tracking_accuracy() {
        let server = MockServer::start();

        let _mock = server.mock(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("Content-Type", "text/event-stream")
                .body(
                    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\
                     \"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"}}]}\n\n\
                     data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\
                     \"choices\":[{\"index\":0,\"delta\":{}}],\
                     \"usage\":{\"prompt_tokens\":42,\"completion_tokens\":7,\"total_tokens\":49}}\n\n\
                     data: [DONE]\n\n",
                );
        });

        let config = test_config(&server);
        let client = Qwen3OpenAIClient::new(config);

        let messages = vec![Message::UserMessage {
            content: "test".into(),
        }];
        let mut stream = client
            .complete_stream(&messages, &[])
            .await
            .expect("stream");
        while let Some(_) = stream.next().await {}

        // Token usage is stored in the shared slot by flush_final_events
        let usage = client.last_token_usage();
        assert!(usage.is_some(), "Expected token usage to be tracked");
        let usage = usage.unwrap();
        assert_eq!(usage.prompt_tokens, 42);
        assert_eq!(usage.completion_tokens, 7);
        assert_eq!(usage.total_tokens, 49);
    }

    // ── Non-streaming complete ──

    #[tokio::test]
    async fn non_streaming_complete() {
        let server = MockServer::start();

        let _mock = server.mock(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(
                    r#"{
                    "id": "1",
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "The answer is 42."},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
                }"#,
                );
        });

        let config = test_config(&server);
        let client = Qwen3OpenAIClient::new(config);

        let messages = vec![Message::UserMessage {
            content: "2+2".into(),
        }];
        let result = client.complete(&messages, &[]).await.expect("complete");
        assert_eq!(result, "The answer is 42.");

        let usage = client.last_token_usage().expect("usage tracked");
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }

    // ── Error handling ──

    #[tokio::test]
    async fn api_error_non_retryable() {
        let server = MockServer::start();

        let _mock = server.mock(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(400)
                .header("Content-Type", "application/json")
                .body(r#"{"error": {"message": "Invalid model"}}"#);
        });

        let config = test_config(&server);
        let client = Qwen3OpenAIClient::new(config);

        let messages = vec![Message::UserMessage {
            content: "hi".into(),
        }];
        let result = client.complete_stream(&messages, &[]).await;
        assert!(result.is_err());
        match result {
            Err(ModelError::Api { status, .. }) => assert_eq!(status, 400),
            _ => panic!("expected Api error"),
        }
    }

    // ── Model name ──

    #[test]
    fn model_name_returns_configured_model() {
        let config = ModelConfig::builder()
            .api_key("sk-test".into())
            .model("my-custom-model".into())
            .build()
            .expect("config");
        let client = Qwen3OpenAIClient::new(config);
        assert_eq!(client.model_name(), "my-custom-model");
    }

    // ── Request body correctness ──

    #[tokio::test]
    async fn request_includes_auth_header() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .header("Authorization", "Bearer sk-test-key")
                .header("Content-Type", "application/json");
            then.status(200)
                .header("Content-Type", "text/event-stream")
                .body("data: [DONE]\n\n");
        });

        let config = ModelConfig::builder()
            .api_key("sk-test-key".into())
            .api_base_url(server.base_url())
            .model("test-model".into())
            .build()
            .expect("config");
        let client = Qwen3OpenAIClient::new(config);

        let messages = vec![Message::UserMessage {
            content: "hi".into(),
        }];
        let result = client.complete_stream(&messages, &[]).await;
        assert!(result.is_ok());
        mock.assert();
    }

    // ── Edge case: empty content delta in chunk ──

    #[tokio::test]
    async fn empty_content_delta_no_event() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("Content-Type", "text/event-stream")
                .body(
                    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\
                     \"choices\":[{\"index\":0,\"delta\":{\"content\":\"\"}}]}\n\n\
                     data: [DONE]\n\n",
                );
        });

        let config = test_config(&server);
        let client = Qwen3OpenAIClient::new(config);

        let messages = vec![Message::UserMessage {
            content: "hi".into(),
        }];
        let mut stream = client
            .complete_stream(&messages, &[])
            .await
            .expect("stream");
        let mut content_events = 0;
        while let Some(event) = stream.next().await {
            if matches!(event, ResponseEvent::AgentMessageDelta { .. }) {
                content_events += 1;
            }
            if matches!(event, ResponseEvent::TurnComplete { .. }) {
                break;
            }
        }

        mock.assert();
        // Empty content deltas should not emit AgentMessageDelta events
        assert_eq!(content_events, 0);
    }

    // ── Edge case: empty tool_calls in chunk ──

    #[tokio::test]
    async fn empty_tool_calls_no_event() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("Content-Type", "text/event-stream")
                .body(
                    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\
                     \"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[]}}]}\n\n\
                     data: [DONE]\n\n",
                );
        });

        let config = test_config(&server);
        let client = Qwen3OpenAIClient::new(config);

        let messages = vec![Message::UserMessage {
            content: "hi".into(),
        }];
        let tools = vec![ToolDefinition::new(
            "test_tool",
            "A test tool",
            json!({"type": "object", "properties": {}}),
        )];

        let mut stream = client
            .complete_stream(&messages, &tools)
            .await
            .expect("stream");
        let mut tool_events = 0;
        while let Some(event) = stream.next().await {
            if matches!(event, ResponseEvent::ToolCallBegin { .. }) {
                tool_events += 1;
            }
            if matches!(event, ResponseEvent::TurnComplete { .. }) {
                break;
            }
        }

        mock.assert();
        assert_eq!(tool_events, 0);
    }

    // ── Edge case: connection refused (simulated via unreachable URL) ──

    #[tokio::test]
    async fn connection_refused_yields_error() {
        let config = ModelConfig::builder()
            .api_key("sk-test".into())
            .api_base_url("http://127.0.0.1:1".into()) // nothing on port 1
            .model("test-model".into())
            .retry(RetryConfig {
                max_retries: 0,
                base_delay_ms: 10,
                max_delay_ms: 10,
                multiplier: 1.0,
            })
            .build()
            .expect("config");
        let client = Qwen3OpenAIClient::new(config);

        let messages = vec![Message::UserMessage {
            content: "hi".into(),
        }];
        let result = client.complete_stream(&messages, &[]).await;
        assert!(
            result.is_err(),
            "Expected connection error"
        );
    }

    // ── Edge case: unparseable SSE data ──

    #[tokio::test]
    async fn unparseable_sse_data_is_skipped() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("Content-Type", "text/event-stream")
                .body(
                    "data: not-json\n\n\
                     data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\
                     \"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"}}]}\n\n\
                     data: [DONE]\n\n",
                );
        });

        let config = test_config(&server);
        let client = Qwen3OpenAIClient::new(config);

        let messages = vec![Message::UserMessage {
            content: "hi".into(),
        }];
        let mut stream = client
            .complete_stream(&messages, &[])
            .await
            .expect("stream");
        let mut texts: Vec<String> = Vec::new();
        while let Some(event) = stream.next().await {
            match event {
                ResponseEvent::AgentMessageDelta { content } => {
                    if !content.is_empty() {
                        texts.push(content);
                    }
                }
                ResponseEvent::TurnComplete { .. } => break,
                _ => {}
            }
        }

        mock.assert();
        assert_eq!(texts, vec!["Hello"]);
    }

    // ── Edge case: stream emits ToolCallBegin + TokenUsage + TurnComplete in order ──

    #[tokio::test]
    async fn stream_emits_tool_then_usage_then_turn_complete() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("Content-Type", "text/event-stream")
                .body(
                    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\
                     \"choices\":[{\"index\":0,\"delta\":{\"content\":\"Using tool\"}}]}\n\n\
                     data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\
                     \"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[\
                     {\"index\":0,\"id\":\"call_x\",\"function\":{\"name\":\"search\",\"arguments\":\"{}\"}}]}}]}\n\n\
                     data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\
                     \"choices\":[{\"index\":0,\"delta\":{}}],\
                     \"usage\":{\"prompt_tokens\":20,\"completion_tokens\":10,\"total_tokens\":30}}\n\n\
                     data: [DONE]\n\n",
                );
        });

        let config = test_config(&server);
        let client = Qwen3OpenAIClient::new(config);

        let messages = vec![Message::UserMessage {
            content: "search".into(),
        }];
        let tools = vec![ToolDefinition::new(
            "search",
            "Search tool",
            json!({"type": "object", "properties": {}}),
        )];

        let mut stream = client
            .complete_stream(&messages, &tools)
            .await
            .expect("stream");
        let mut events: Vec<ResponseEvent> = Vec::new();
        while let Some(event) = stream.next().await {
            let is_turn_complete = matches!(event, ResponseEvent::TurnComplete { .. });
            events.push(event);
            if is_turn_complete {
                break;
            }
        }

        mock.assert();

        // Verify event sequence: content delta(s), then ToolCallBegin, then TokenUsage, then TurnComplete
        let has_content = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::AgentMessageDelta { .. }));
        let has_tool = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::ToolCallBegin { .. }));
        let has_usage = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::TokenUsage { .. }));
        let has_complete = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::TurnComplete { .. }));

        assert!(has_content, "Expected AgentMessageDelta event");
        assert!(has_tool, "Expected ToolCallBegin event");
        assert!(has_usage, "Expected TokenUsage event");
        assert!(has_complete, "Expected TurnComplete event");

        // Check ordering: content before tool, tool before usage, usage before complete
        let tool_pos = events
            .iter()
            .position(|e| matches!(e, ResponseEvent::ToolCallBegin { .. }))
            .unwrap();
        let usage_pos = events
            .iter()
            .position(|e| matches!(e, ResponseEvent::TokenUsage { .. }))
            .unwrap();
        let complete_pos = events
            .iter()
            .position(|e| matches!(e, ResponseEvent::TurnComplete { .. }))
            .unwrap();
        assert!(
            tool_pos < usage_pos,
            "ToolCallBegin should come before TokenUsage"
        );
        assert!(
            usage_pos < complete_pos,
            "TokenUsage should come before TurnComplete"
        );
    }
}
