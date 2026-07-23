//! 命名事件 —— Agent 执行期间由追踪器发射的标准化事件。
//!
//! 【领域含义】本模块定义了 Agent 执行过程中发射的所有标准化事件类型。
//! 每个事件是一个零大小的类型标记，映射到带有已知字段名的 tracing 事件。
//! 实际的事件发射发生在 super::tracer::Tracer 内部，Tracer 会自动附加
//! 会话级元数据。
//!
//! # 事件分类
//!
//! | 事件               | 发射时机                          |
//! |--------------------|----------------------------------|
//! | TurnStarted        | 新的用户→Agent→响应交互开始        |
//! | TurnCompleted      | 交互完成（成功或错误）             |
//! | ToolCallBegin      | 模型请求调用工具                   |
//! | ToolCallEnd        | 工具执行完成                      |
//! | ModelRequestBegin  | 向模型 API 发送 HTTP 请求          |
//! | ModelRequestEnd    | 从模型 API 收到响应                |
//! | TokenUsage         | Token 消耗报告                    |
//! | CompactionTriggered| 上下文压缩已应用                   |
//! Named events emitted by the tracer during agent execution.
//!
//! Each event is a zero-sized type that maps to a tracing event with
//! well-known field names. The actual event emission happens inside
//! super::tracer::Tracer, which adds session-level metadata automatically.
//!
//! # Event Categories
//!
//! | Event               | When emitted                                           |
//! |--------------------|-------------------------------------------------------|
//! | TurnStarted        | Beginning of a new user→agent→response turn            |
//! | TurnCompleted      | Turn finished (success or error)                       |
//! | ToolCallBegin      | Model requested a tool invocation                      |
//! | ToolCallEnd        | Tool execution completed                               |
//! | ModelRequestBegin  | HTTP request sent to the model API                     |
//! | ModelRequestEnd    | Response received from the model API                   |
//! | TokenUsage         | Token consumption reported                             |
//! | CompactionTriggered | Context compaction was applied                      |

use code_agent_protocol::{SessionId, TurnId};

// ---------------------------------------------------------------------------
// Event types (zero-sized markers; the real data is in field parameters)
// ---------------------------------------------------------------------------

/// 命名可观测性事件及其关联字段。
///
/// 【领域含义】ObservabilityEvent 是事件在被发射为 tracing 事件之前的
/// 内部表示。Tracer 将每个枚举变体映射到对应的 tracing::event! 宏调用。
/// 这是领域事件（Domain Event）模式在可观测性层的应用 —— 每个变体代表
/// Agent 生命周期中的一个有意义的业务事件。
///
/// A named observability event with its associated fields.
///
/// This is the internal representation of an event before it is emitted as a
/// tracing event. The Tracer maps each variant to the corresponding
/// tracing::event! macro invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservabilityEvent {
    /// 一次交互已开始。
    ///
    /// A turn has started.
    TurnStarted {
        /// 正在开始的交互 ID。
        ///
        /// The turn being started.
        turn_id: TurnId,
    },
    /// 一次交互已完成（成功或失败）。
    ///
    /// A turn has completed (successfully or with error).
    TurnCompleted {
        /// 已完成的交互 ID。
        ///
        /// The turn that finished.
        turn_id: TurnId,
        /// 交互是否被中断。
        ///
        /// Whether the turn was interrupted.
        interrupted: bool,
        /// 执行的 ReAct 循环迭代次数。
        ///
        /// Number of ReAct loop iterations executed.
        iterations: usize,
    },
    /// 模型发起了工具调用。
    ///
    /// A tool call was initiated by the model.
    ToolCallBegin {
        /// 被调用的工具名称（如 "read_file"）。
        ///
        /// The tool being called (e.g., "read_file").
        tool_name: String,
        /// 模型分配的唯一工具调用 ID。
        ///
        /// The unique tool call ID assigned by the model.
        tool_call_id: String,
    },
    /// 工具调用执行完成。
    ///
    /// A tool call completed execution.
    ToolCallEnd {
        /// 被调用的工具。
        ///
        /// The tool that was called.
        tool_name: String,
        /// 唯一的工具调用 ID。
        ///
        /// The unique tool call ID.
        tool_call_id: String,
        /// 工具调用是否成功。
        ///
        /// Whether the tool call succeeded.
        success: bool,
        /// 工具执行的耗时（微秒），如果已测量。
        ///
        /// Duration of the tool execution in microseconds, if measured.
        duration_us: Option<u64>,
    },
    /// 即将向模型 API 发送流式请求。
    ///
    /// A streaming request to the model API is about to be sent.
    ModelRequestBegin {
        /// 模型名称（如 "my-chat-model"）。
        ///
        /// The model name (e.g., "my-chat-model").
        model_name: String,
        /// 请求中的消息数量。
        ///
        /// Number of messages in the request.
        message_count: usize,
        /// 模型可用的工具数量。
        ///
        /// Number of tools available to the model.
        tool_count: usize,
    },
    /// 模型 API 请求已完成（或失败）。
    ///
    /// A model API request completed (or failed).
    ModelRequestEnd {
        /// 模型名称。
        ///
        /// The model name.
        model_name: String,
        /// 请求是否成功。
        ///
        /// Whether the request succeeded.
        success: bool,
        /// API 调用的耗时（微秒）。
        ///
        /// Duration of the API call in microseconds.
        duration_us: u64,
    },
    /// Token 使用量统计已报告。
    ///
    /// Token usage statistics were reported.
    TokenUsage {
        /// 提示（输入）Token 数量。
        ///
        /// Prompt (input) tokens.
        prompt_tokens: u32,
        /// 补全（输出）Token 数量。
        ///
        /// Completion (output) tokens.
        completion_tokens: u32,
    },
    /// 上下文压缩已触发。
    ///
    /// Context compaction was triggered.
    CompactionTriggered {
        /// 压缩前的 Token 数量。
        ///
        /// Token count before compaction.
        tokens_before: usize,
        /// 压缩后的 Token 数量。
        ///
        /// Token count after compaction.
        tokens_after: usize,
        /// 被移除或截断的消息数量。
        ///
        /// Number of messages removed or truncated.
        messages_removed: usize,
        /// 触发的压缩层。
        ///
        /// The compaction layer(s) that fired.
        layers: String,
    },
}

// ---------------------------------------------------------------------------
// Event name constants (used as tracing::event!(name: ...))
// ---------------------------------------------------------------------------

/// 事件名称字符串 —— 这些是 JSON 日志输出中的 event 字段值。
///
/// 【领域含义】event_names 模块定义了所有标准化事件名称常量。
/// 这些常量在 Tracer 中用作 tracing::event! 的 event 字段值，确保
/// 日志消费者可以通过统一的命名约定来过滤和分析事件。
///
/// Event name strings — these are the event field in JSON log output.
pub mod event_names {
    /// 在交互开始时发射。
    ///
    /// Emitted when a turn begins.
    pub const TURN_STARTED: &str = "turn.started";
    /// 在交互完成时发射。
    ///
    /// Emitted when a turn completes.
    pub const TURN_COMPLETED: &str = "turn.completed";
    /// 在工具调用开始时发射。
    ///
    /// Emitted when a tool call begins.
    pub const TOOL_CALL_BEGIN: &str = "tool_call.begin";
    /// 在工具调用结束时发射。
    ///
    /// Emitted when a tool call ends.
    pub const TOOL_CALL_END: &str = "tool_call.end";
    /// 在模型请求发送时发射。
    ///
    /// Emitted when a model request is sent.
    pub const MODEL_REQUEST_BEGIN: &str = "model_request.begin";
    /// 在模型请求完成时发射。
    ///
    /// Emitted when a model request completes.
    pub const MODEL_REQUEST_END: &str = "model_request.end";
    /// 在 Token 使用量报告时发射。
    ///
    /// Emitted when token usage is reported.
    pub const TOKEN_USAGE: &str = "token.usage";
    /// 在上下文压缩触发时发射。
    ///
    /// Emitted when context compaction is triggered.
    pub const COMPACTION_TRIGGERED: &str = "compaction.triggered";
}

// ---------------------------------------------------------------------------
// Helper: extract field values for tracing
// ---------------------------------------------------------------------------

impl ObservabilityEvent {
    /// 用作 tracing 宏中 name 参数的事件名称字符串。
    ///
    /// The event name string used as the name parameter in tracing macros.
    pub fn event_name(&self) -> &'static str {
        match self {
            Self::TurnStarted { .. } => event_names::TURN_STARTED,
            Self::TurnCompleted { .. } => event_names::TURN_COMPLETED,
            Self::ToolCallBegin { .. } => event_names::TOOL_CALL_BEGIN,
            Self::ToolCallEnd { .. } => event_names::TOOL_CALL_END,
            Self::ModelRequestBegin { .. } => event_names::MODEL_REQUEST_BEGIN,
            Self::ModelRequestEnd { .. } => event_names::MODEL_REQUEST_END,
            Self::TokenUsage { .. } => event_names::TOKEN_USAGE,
            Self::CompactionTriggered { .. } => event_names::COMPACTION_TRIGGERED,
        }
    }
}

// ---------------------------------------------------------------------------
// Session metadata helper
// ---------------------------------------------------------------------------

/// 每个事件携带的会话标识符的轻量级包装。
///
/// 【领域含义】SessionMeta 为每个可观测性事件提供会话级别的上下文，
/// 确保所有遥测数据都可以追溯到特定的 Agent 会话。
///
/// Lightweight wrapper for the session identifier carried on every event.
#[derive(Clone, Debug)]
pub struct SessionMeta {
    pub session_id: SessionId,
}

impl std::fmt::Display for SessionMeta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.session_id)
    }
}