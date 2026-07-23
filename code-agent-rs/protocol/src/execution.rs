//! 执行层 (Execution Layer) — 轮次生命周期与事件流
//!
//! 本模块定义了 Agent 执行轮次的输入模型和事件流模型：
//! [`TurnInput`] 描述启动一轮对话所需的上下文，
//! [`ResponseEvent`] 用事件溯源模式描述轮次执行过程中产生的所有事件。
//!
//! 【DDD 分层】执行层属于应用层 / 领域事件层。
//! TurnInput 是命令（Command），ResponseEvent 是事件（Event）。
//! 上层（TUI、Web、CLI）通过订阅 ResponseEvent 流来实时展示 Agent 行为。
//!
//! 【事件流时序】
//! ```text
//! TurnStarted → AgentMessageDelta* → ToolCallBegin → ToolCallEnd → AgentMessageDelta* → TurnComplete
//!                                                                                         → Error
//! TokenUsage 可在任意时刻穿插发出
//! ```

use serde::{Deserialize, Serialize};

use crate::identity::TurnId;
use crate::message::{Message, ToolCall, ToolResultMessage};

// ---------------------------------------------------------------------------
// TurnInput — 轮次输入
// ---------------------------------------------------------------------------

/// 轮次输入 (Turn Execution Command)
///
/// 【领域含义】启动一轮 Agent 对话所需的完整上下文。
/// TurnInput 是 Agent 执行循环的入口数据结构，
/// 封装了目标线程和本轮需要处理的消息集合。
///
/// 【使用场景】
/// - 用户在 TUI 中输入指令后，系统构建 TurnInput 并提交给 Agent
/// - Cron 任务触发时，系统自动构建 TurnInput 发起后台处理
/// - SubAgent 模式下，主 Agent 为子 Agent 构建 TurnInput
///
/// 【字段说明】
/// - `thread_id`: 目标线程标识，确保消息追加到正确的对话分支
/// - `messages`: 本轮需要处理的消息列表。通常包含用户最新输入 +
///   系统注入的上下文（如文件内容、项目结构等）
///
/// 【约束】
/// - `thread_id` 必须引用一个已存在的 Thread
/// - `messages` 可以为空（用于纯系统驱动的轮次），但通常至少包含一条消息
///
/// 【与其他类型的关系】
/// - 引用 [`ThreadId`]（归属线程）
/// - 包含 [`Message`] 列表（本轮输入消息）
/// - 被 Agent 循环消费，产出 [`ResponseEvent`] 流
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnInput {
    /// 本轮对话所属的线程标识
    pub thread_id: crate::identity::ThreadId,
    /// 本轮需要处理的消息列表（用户输入 + 系统上下文）
    pub messages: Vec<Message>,
}

// ---------------------------------------------------------------------------
// ResponseEvent — 响应事件枚举
// ---------------------------------------------------------------------------

/// 响应事件 (Turn Execution Event Stream)
///
/// 【领域含义】Agent 执行过程中产生的实时事件流。
/// 使用事件溯源（Event Sourcing）模式：
/// 每个事件记录一个不可变的业务事实，客户端通过订阅事件流
/// 重建 Agent 的完整执行过程。
///
/// 【使用场景】
/// - TUI 实时渲染：订阅事件流，动态更新终端界面
/// - Web UI：通过 SSE (Server-Sent Events) 推送到浏览器
/// - 日志/审计：所有事件持久化后可用于回放和分析
/// - 测试：验证 Agent 的执行路径是否符合预期
///
/// 【序列化格式】`#[serde(rename_all = "snake_case")]`
/// 确保 JSON 输出为 `{"turn_started": {"turn_id": "..."}}` 格式。
///
/// 【与其他类型的关系】
/// - `TurnStarted` / `TurnComplete` 携带 [`TurnId`]
/// - `ToolCallBegin` 包装 [`ToolCall`] 结构体
/// - `ToolCallEnd` 包装 [`ToolResultMessage`] 结构体
/// - `TurnComplete` 的 `final_message` 是 [`Message`]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseEvent {
    /// 新轮次已创建并开始执行
    ///
    /// 【触发时机】Agent 接受 TurnInput 并创建新 Turn 后立即发出。
    /// 这是每个轮次的第一个事件，客户端可据此初始化 UI 状态。
    TurnStarted {
        /// 新创建的轮次标识
        turn_id: TurnId,
    },
    /// Agent 流式文本响应的增量片段
    ///
    /// 【触发时机】LLM 每产生一个 token 即发出一个 Delta 事件。
    /// 一个轮次可能产生多条 Delta，客户端需拼接所有 content 得到完整响应。
    /// 【领域含义】这是流式输出的基本单元，用于实现打字机效果。
    AgentMessageDelta {
        /// 助手文本输出的一个片段（增量，非全量）
        content: String,
    },
    /// 工具调用已被 Agent 发起
    ///
    /// 【触发时机】Agent 生成 ToolCall 后、实际执行前发出。
    /// 客户端可据此展示 "正在调用工具..." 的 UI 状态。
    ToolCallBegin {
        /// 被调用的工具详情（ID、名称、参数）
        tool_call: ToolCall,
    },
    /// 工具调用执行完毕
    ///
    /// 【触发时机】工具执行完成后立即发出（无论成功或失败）。
    /// 客户端可据此更新工具调用的 UI 状态（显示结果/错误）。
    ToolCallEnd {
        /// 对应 ToolCall 的 ID，用于关联请求与结果
        tool_call_id: String,
        /// 工具执行结果（输出或错误信息）
        result: ToolResultMessage,
    },
    /// 轮次成功完成
    ///
    /// 【触发时机】Agent 完成所有推理和工具调用后，
    /// 产出最终响应时发出。这是正常轮次的最后一个事件。
    TurnComplete {
        /// 完成轮次的标识
        turn_id: TurnId,
        /// 该轮次的最终助手消息（完整文本）
        final_message: Message,
    },
    /// 轮次执行过程中发生错误
    ///
    /// 【触发时机】任何不可恢复的错误（LLM API 调用失败、
    /// 超时、内部异常等）发生时发出。此事件通常标志轮次异常终止。
    Error {
        /// 人类可读的错误描述
        message: String,
    },
    /// Token 用量统计
    ///
    /// 【触发时机】在轮次执行过程中定期发出（如每次 LLM API 调用后），
    /// 也可在 TurnComplete 之前作为最后一条统计信息发出。
    /// 用于计量和计费。
    TokenUsage {
        /// 提示词（输入）消耗的 Token 数
        prompt_tokens: u32,
        /// 补全（输出）消耗的 Token 数
        completion_tokens: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_input_construction() {
        let input = TurnInput {
            thread_id: crate::identity::ThreadId::new("th-1"),
            messages: vec![Message::UserMessage {
                content: "hello".into(),
            }],
        };
        assert_eq!(input.thread_id.to_string(), "th-1");
        assert_eq!(input.messages.len(), 1);
    }

    #[test]
    fn turn_input_empty_messages() {
        let input = TurnInput {
            thread_id: crate::identity::ThreadId::new("th-empty"),
            messages: vec![],
        };
        assert!(input.messages.is_empty());
    }

    #[test]
    fn response_event_turn_started_has_turn_id() {
        let ev = ResponseEvent::TurnStarted {
            turn_id: TurnId::new("turn-1"),
        };
        match ev {
            ResponseEvent::TurnStarted { turn_id } => {
                assert_eq!(turn_id.to_string(), "turn-1");
            }
            _ => panic!("expected TurnStarted"),
        }
    }

    #[test]
    fn response_event_all_variants_exist() {
        // Ensure all variants compile
        let _ = ResponseEvent::TurnStarted {
            turn_id: TurnId::new("t1"),
        };
        let _ = ResponseEvent::AgentMessageDelta {
            content: "hello".into(),
        };
        let _ = ResponseEvent::ToolCallBegin {
            tool_call: ToolCall {
                id: "tc1".into(),
                name: "test".into(),
                arguments: serde_json::json!({}),
            },
        };
        let _ = ResponseEvent::ToolCallEnd {
            tool_call_id: "tc1".into(),
            result: ToolResultMessage {
                tool_call_id: "tc1".into(),
                output: Some("ok".into()),
                error: None,
            },
        };
        let _ = ResponseEvent::TurnComplete {
            turn_id: TurnId::new("t1"),
            final_message: Message::AssistantMessage {
                content: "done".into(),
            },
        };
        let _ = ResponseEvent::Error {
            message: "oops".into(),
        };
        let _ = ResponseEvent::TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
        };
    }
}
