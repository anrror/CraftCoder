//! 会话级追踪器 —— 创建 Span 并随会话元数据发射事件。
//!
//! 【领域含义】Tracer 是用户与可观测性系统交互的主要 API。它封装了 `tracing`
//! crate，为每个 Span 和事件自动附加 `session_id` 和其他元数据，并在
//! `observability` 特性禁用时提供零成本的空操作模式。
//!
//! # 使用方式
//!
//! ```rust,ignore
//! use code_agent_core::observability::Tracer;
//! use code_agent_protocol::SessionId;
//!
//! let tracer = Tracer::new(SessionId::from("sess-1"));
//!
//! // 为一次交互创建 Span
//! let turn_span = tracer.start_turn("turn-42");
//! let _enter = turn_span.enter();
//! // ... 执行交互 ...
//! tracer.turn_completed("turn-42", false, 1);
//! drop(_enter);
//! ```
//! Session-scoped tracer — creates spans and emits events with session metadata.
//!
//! The Tracer is the primary API users interact with. It wraps the
//! tracing crate to automatically attach session_id and other metadata to
//! every span and event, and to provide a zero-cost no-op mode when the
//! observability feature is disabled.
//!
//! # Usage
//!
//! ```rust,ignore
//! use code_agent_core::observability::Tracer;
//! use code_agent_protocol::SessionId;
//!
//! let tracer = Tracer::new(SessionId::from("sess-1"));
//!
//! // Create a span for a turn
//! let turn_span = tracer.start_turn("turn-42");
//! let _enter = turn_span.enter();
//! // ... run the turn ...
//! tracer.turn_completed("turn-42", false, 1);
//! drop(_enter);
//! ```

use std::time::Instant;

use code_agent_protocol::SessionId;

use super::events::event_names;

// ---------------------------------------------------------------------------
// Tracer
// ---------------------------------------------------------------------------

/// 会话级追踪器 —— 带 session_id 元数据的 Span 与事件发射器。
///
/// 【领域含义】Tracer 是 Agent 可观测性的核心入口，负责为每个会话创建
/// 带上下文的追踪 Span 和结构化事件。它与 Session 一一对应，确保所有
/// 遥测数据都可以追溯到特定会话。
///
/// # 特性标志行为
///
/// - observability 启用（默认）：创建真实的追踪 Span 并发射结构化事件。
///   需要先调用 super::logger::setup_logging。
/// - observability 禁用：所有方法为空操作。类型仍然存在，调用者无需
///   #[cfg] 守卫。
///
/// A session-scoped tracer that emits spans and events with session_id metadata.
///
/// # Feature flag behavior
///
/// - observability enabled (default): Creates real tracing spans and
///   emits structured events. Call super::logger::setup_logging first.
/// - observability disabled: All methods are no-ops. The type still
///   exists so callers don't need #[cfg] guards.
#[derive(Clone, Debug)]
pub struct Tracer {
    session_id: SessionId,
}

impl Tracer {
    /// 为指定会话创建新的追踪器。
    ///
    /// Create a new tracer for the given session.
    pub fn new(session_id: SessionId) -> Self {
        Self { session_id }
    }

    /// 返回此追踪器绑定的会话 ID。
    ///
    /// Returns the session ID this tracer is bound to.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    // ── Turn lifecycle ────────────────────────────────────────────────────

    /// 创建表示一次交互的 Span。
    ///
    /// 在交互处理期间进入此 Span。自动记录 session_id 和 turn_id。
    /// 在交互开始时发射 TurnStarted 事件。
    ///
    /// Create a span representing one turn.
    ///
    /// The span is entered for the duration of turn processing. It
    /// automatically records session_id and turn_id.
    #[cfg(feature = "observability")]
    pub fn start_turn(&self, turn_id: &str) -> tracing::Span {
        let span = tracing::info_span!(
            "turn",
            session_id = %self.session_id,
            turn_id = %turn_id,
        );
        tracing::event!(
            parent: &span,
            tracing::Level::INFO,
            session_id = %self.session_id,
            turn_id = %turn_id,
            event = event_names::TURN_STARTED,
        );
        span
    }

    #[cfg(not(feature = "observability"))]
    pub fn start_turn(&self, _turn_id: &str) -> tracing::Span {
        tracing::Span::none()
    }

    /// 记录一次交互已完成。
    ///
    /// Record that a turn has completed.
    #[cfg(feature = "observability")]
    pub fn turn_completed(&self, turn_id: &str, interrupted: bool, iterations: usize) {
        tracing::event!(
            tracing::Level::INFO,
            session_id = %self.session_id,
            turn_id = %turn_id,
            interrupted = interrupted,
            iterations = iterations,
            event = event_names::TURN_COMPLETED,
        );
    }

    #[cfg(not(feature = "observability"))]
    pub fn turn_completed(&self, _turn_id: &str, _interrupted: bool, _iterations: usize) {}

    // ── Tool call lifecycle ──────────────────────────────────────────────

    /// 为工具调用创建 Span。
    ///
    /// 应在工具执行前进入此 Span，在获取结果后丢弃。
    /// 创建时发射 ToolCallBegin 事件。
    ///
    /// Create a span for a tool call.
    ///
    /// The span should be entered before tool execution and dropped after
    /// the result is obtained. Emits ToolCallBegin on creation.
    #[cfg(feature = "observability")]
    pub fn tool_call_begin(
        &self,
        tool_name: &str,
        tool_call_id: &str,
    ) -> ToolCallSpan {
        let span = tracing::info_span!(
            "tool_call",
            session_id = %self.session_id,
            tool_name = %tool_name,
            tool_call_id = %tool_call_id,
        );
        tracing::event!(
            parent: &span,
            tracing::Level::INFO,
            session_id = %self.session_id,
            tool_name = %tool_name,
            tool_call_id = %tool_call_id,
            event = event_names::TOOL_CALL_BEGIN,
        );
        ToolCallSpan {
            span,
            tool_name: tool_name.to_string(),
            tool_call_id: tool_call_id.to_string(),
            started_at: Instant::now(),
        }
    }

    #[cfg(not(feature = "observability"))]
    pub fn tool_call_begin(
        &self,
        _tool_name: &str,
        _tool_call_id: &str,
    ) -> ToolCallSpan {
        ToolCallSpan {
            span: tracing::Span::none(),
            tool_name: String::new(),
            tool_call_id: String::new(),
            started_at: Instant::now(),
        }
    }

    /// 记录工具调用已完成。
    ///
    /// Record that a tool call has completed.
    #[cfg(feature = "observability")]
    pub fn tool_call_end(
        &self,
        tool_name: &str,
        tool_call_id: &str,
        success: bool,
        duration_us: u64,
    ) {
        tracing::event!(
            tracing::Level::INFO,
            session_id = %self.session_id,
            tool_name = %tool_name,
            tool_call_id = %tool_call_id,
            success = success,
            duration_us = duration_us,
            event = event_names::TOOL_CALL_END,
        );
    }

    #[cfg(not(feature = "observability"))]
    pub fn tool_call_end(
        &self,
        _tool_name: &str,
        _tool_call_id: &str,
        _success: bool,
        _duration_us: u64,
    ) {
    }

    // ── Model request lifecycle ──────────────────────────────────────────

    /// 为模型 API 请求创建 Span。
    ///
    /// 应在 HTTP 请求进行中时进入此 Span。
    ///
    /// Create a span for a model API request.
    ///
    /// The span should be entered while the HTTP request is in flight.
    #[cfg(feature = "observability")]
    pub fn model_request_begin(
        &self,
        model_name: &str,
        message_count: usize,
        tool_count: usize,
    ) -> ModelRequestSpan {
        let span = tracing::info_span!(
            "model_request",
            session_id = %self.session_id,
            model_name = %model_name,
        );
        tracing::event!(
            parent: &span,
            tracing::Level::INFO,
            session_id = %self.session_id,
            model_name = %model_name,
            message_count = message_count,
            tool_count = tool_count,
            event = event_names::MODEL_REQUEST_BEGIN,
        );
        ModelRequestSpan {
            span,
            model_name: model_name.to_string(),
            started_at: Instant::now(),
        }
    }

    #[cfg(not(feature = "observability"))]
    pub fn model_request_begin(
        &self,
        _model_name: &str,
        _message_count: usize,
        _tool_count: usize,
    ) -> ModelRequestSpan {
        ModelRequestSpan {
            span: tracing::Span::none(),
            model_name: String::new(),
            started_at: Instant::now(),
        }
    }

    /// 记录模型 API 请求已完成。
    ///
    /// Record that a model API request has completed.
    #[cfg(feature = "observability")]
    pub fn model_request_end(
        &self,
        model_name: &str,
        success: bool,
        duration_us: u64,
    ) {
        tracing::event!(
            tracing::Level::INFO,
            session_id = %self.session_id,
            model_name = %model_name,
            success = success,
            duration_us = duration_us,
            event = event_names::MODEL_REQUEST_END,
        );
    }

    #[cfg(not(feature = "observability"))]
    pub fn model_request_end(
        &self,
        _model_name: &str,
        _success: bool,
        _duration_us: u64,
    ) {
    }

    // ── Token usage ───────────────────────────────────────────────────────

    /// 记录当前模型调用的 Token 使用量。
    ///
    /// Record token usage for the current model call.
    #[cfg(feature = "observability")]
    pub fn record_token_usage(&self, prompt_tokens: u32, completion_tokens: u32) {
        tracing::event!(
            tracing::Level::INFO,
            session_id = %self.session_id,
            prompt_tokens = prompt_tokens,
            completion_tokens = completion_tokens,
            total_tokens = prompt_tokens + completion_tokens,
            event = event_names::TOKEN_USAGE,
        );
    }

    #[cfg(not(feature = "observability"))]
    pub fn record_token_usage(&self, _prompt_tokens: u32, _completion_tokens: u32) {}

    // ── Compaction ───────────────────────────────────────────────────────

    /// 记录上下文压缩被触发。
    ///
    /// Record that context compaction was triggered.
    #[cfg(feature = "observability")]
    pub fn compaction_triggered(
        &self,
        tokens_before: usize,
        tokens_after: usize,
        messages_removed: usize,
        layers: &str,
    ) {
        tracing::event!(
            tracing::Level::INFO,
            session_id = %self.session_id,
            tokens_before = tokens_before,
            tokens_after = tokens_after,
            messages_removed = messages_removed,
            layers = layers,
            reduction_pct = if tokens_before > 0 {
                format!("{:.1}", (1.0 - tokens_after as f64 / tokens_before as f64) * 100.0)
            } else {
                "0.0".to_string()
            },
            event = event_names::COMPACTION_TRIGGERED,
        );
    }

    #[cfg(not(feature = "observability"))]
    pub fn compaction_triggered(
        &self,
        _tokens_before: usize,
        _tokens_after: usize,
        _messages_removed: usize,
        _layers: &str,
    ) {
    }
}

// ---------------------------------------------------------------------------
// Span wrappers — carry timing data for automatic duration calculation
// ---------------------------------------------------------------------------

/// 进行中的工具调用 Span。
///
/// 【领域含义】ToolCallSpan 封装了一个工具调用从开始到结束的完整生命周期。
/// 当丢弃时，底层的 tracing Span 被关闭。使用 finish 方法可以发射结束事件
/// 并计算经过的耗时。
///
/// A span representing an in-progress tool call.
///
/// When dropped, the underlying tracing span is closed. Use
/// finish (ToolCallSpan::finish) to emit the end event and calculate the
/// elapsed duration.
#[must_use = "Tool call spans must be finished or explicitly dropped"]
#[derive(Debug)]
pub struct ToolCallSpan {
    span: tracing::Span,
    tool_name: String,
    tool_call_id: String,
    started_at: Instant,
}

impl ToolCallSpan {
    /// 获取底层 tracing Span 的引用（例如，用于 .enter()）。
    ///
    /// Get a reference to the underlying tracing span (e.g., for .enter()).
    pub fn span(&self) -> &tracing::Span {
        &self.span
    }

    /// 发射 ToolCallEnd 事件并关闭 Span。
    ///
    /// Emit the ToolCallEnd event and close the span.
    #[cfg(feature = "observability")]
    pub fn finish(self, success: bool) {
        let duration_us = self.started_at.elapsed().as_micros() as u64;
        tracing::event!(
            parent: &self.span,
            tracing::Level::INFO,
            tool_name = %self.tool_name,
            tool_call_id = %self.tool_call_id,
            success = success,
            duration_us = duration_us,
            event = event_names::TOOL_CALL_END,
        );
        // Span drops here, recording its duration automatically
    }

    #[cfg(not(feature = "observability"))]
    pub fn finish(self, _success: bool) {
        // Span drops — no-op
    }

    /// 返回自创建此 Span 以来经过的时间，以微秒为单位。
    ///
    /// Return the elapsed time since this span was created, in microseconds.
    pub fn elapsed_us(&self) -> u64 {
        self.started_at.elapsed().as_micros() as u64
    }
}

/// 进行中的模型 API 请求 Span。
///
/// 【领域含义】ModelRequestSpan 封装了一个模型 API 请求从发送到接收响应的
/// 完整生命周期。使用 finish 方法可以发射结束事件并记录耗时。
///
/// A span representing an in-progress model API request.
///
/// When dropped, the underlying tracing span is closed. Use
/// finish (ModelRequestSpan::finish) to emit the end event.
#[must_use = "Model request spans must be finished or explicitly dropped"]
#[derive(Debug)]
pub struct ModelRequestSpan {
    span: tracing::Span,
    model_name: String,
    started_at: Instant,
}

impl ModelRequestSpan {
    /// 获取底层 tracing Span 的引用。
    ///
    /// Get a reference to the underlying tracing span.
    pub fn span(&self) -> &tracing::Span {
        &self.span
    }

    /// 发射 ModelRequestEnd 事件并关闭 Span。
    ///
    /// Emit the ModelRequestEnd event and close the span.
    #[cfg(feature = "observability")]
    pub fn finish(self, success: bool) {
        let duration_us = self.started_at.elapsed().as_micros() as u64;
        tracing::event!(
            parent: &self.span,
            tracing::Level::INFO,
            model_name = %self.model_name,
            success = success,
            duration_us = duration_us,
            event = event_names::MODEL_REQUEST_END,
        );
    }

    #[cfg(not(feature = "observability"))]
    pub fn finish(self, _success: bool) {
        // Span drops — no-op
    }

    /// 返回自创建此 Span 以来经过的时间，以微秒为单位。
    ///
    /// Return the elapsed time since this span was created, in microseconds.
    pub fn elapsed_us(&self) -> u64 {
        self.started_at.elapsed().as_micros() as u64
    }
}
// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::logger;

    #[test]
    fn tracer_creation() {
        let tracer = Tracer::new(SessionId::from("test-session"));
        assert_eq!(tracer.session_id().0, "test-session");
    }

    #[test]
    fn tracer_clone() {
        let tracer = Tracer::new(SessionId::from("s1"));
        let cloned = tracer.clone();
        assert_eq!(cloned.session_id().0, "s1");
    }

    #[test]
    fn tracer_debug() {
        let tracer = Tracer::new(SessionId::from("dbg"));
        let debug = format!("{tracer:?}");
        assert!(debug.contains("dbg"));
    }

    #[test]
    fn start_turn_returns_span() {
        logger::setup_for_testing("info");
        let tracer = Tracer::new(SessionId::from("test"));
        let span = tracer.start_turn("turn-1");
        // The span should exist (not panic)
        let _ = format!("{span:?}");
    }

    #[test]
    fn turn_completed_emits() {
        logger::setup_for_testing("info");
        let tracer = Tracer::new(SessionId::from("test"));
        tracer.turn_completed("turn-1", false, 1);
    }

    #[test]
    fn tool_call_lifecycle() {
        logger::setup_for_testing("info");
        let tracer = Tracer::new(SessionId::from("test"));
        let span = tracer.tool_call_begin("read_file", "call-1");
        let _ = span.elapsed_us(); // just verify it doesn'"'"'t panic
        span.finish(true);
    }

    #[test]
    fn tool_call_end_standalone() {
        logger::setup_for_testing("info");
        let tracer = Tracer::new(SessionId::from("test"));
        tracer.tool_call_end("read_file", "call-1", true, 5000);
    }

    #[test]
    fn model_request_lifecycle() {
        logger::setup_for_testing("info");
        let tracer = Tracer::new(SessionId::from("test"));
        let span = tracer.model_request_begin("qwen", 10, 5);
        let _ = span.elapsed_us(); // just verify it doesn'"'"'t panic
        span.finish(true);
    }

    #[test]
    fn model_request_end_standalone() {
        logger::setup_for_testing("info");
        let tracer = Tracer::new(SessionId::from("test"));
        tracer.model_request_end("qwen", true, 150_000);
    }

    #[test]
    fn record_token_usage_emits() {
        logger::setup_for_testing("info");
        let tracer = Tracer::new(SessionId::from("test"));
        tracer.record_token_usage(100, 50);
    }

    #[test]
    fn compaction_triggered_emits() {
        logger::setup_for_testing("info");
        let tracer = Tracer::new(SessionId::from("test"));
        tracer.compaction_triggered(10000, 8000, 5, "Snip,BudgetReduction");
    }

    #[test]
    fn tracer_methods_dont_panic_when_disabled() {
        // This test always passes because even without the observability
        // feature, methods return no-op spans and don'"'"'t panic.
        let tracer = Tracer::new(SessionId::from("test"));

        let turn_span = tracer.start_turn("t1");
        drop(turn_span);

        tracer.turn_completed("t1", false, 1);

        let tool_span = tracer.tool_call_begin("read", "call-1");
        let _ = tool_span.elapsed_us();
        tool_span.finish(true);

        tracer.tool_call_end("read", "call-1", true, 5000);

        let model_span = tracer.model_request_begin("qwen", 10, 5);
        drop(model_span);

        tracer.model_request_end("qwen", true, 100_000);
        tracer.record_token_usage(100, 50);
        tracer.compaction_triggered(10000, 8000, 2, "Snip");
    }
}