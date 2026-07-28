//! 单轮交互上下文
//!
//! 【领域含义】`TurnContext` 是单轮对话的执行上下文值对象。
//! 一轮（Turn）代表一次完整的「用户输入 → Agent 处理 → 响应」交互。
//!
//! A turn represents one complete round: user input → agent processing → response.
//! The [`TurnContext`] bundles the turn's identity, thread, and input messages.

use code_agent_protocol::{Message, ThreadId, TurnId};

/// 单轮交互执行上下文
///
/// 【领域含义】捆绑该轮次的唯一标识、所属线程以及发起本轮的消息内容。
/// 是 `Session::run_turn` 方法的输入参数。
///
/// Bundles the turn's unique identity, the parent thread, and the messages
/// that initiated this turn (typically the user's latest input plus any
/// system-level context).
#[derive(Clone, Debug)]
pub struct TurnContext {
    /// 本轮次的唯一标识符
    pub turn_id: TurnId,

    /// 本轮次所属的线程 ID
    pub thread_id: ThreadId,

    /// 发起本轮的消息列表（通常为用户的最新输入和系统级上下文）
    pub messages: Vec<Message>,
}

impl TurnContext {
    /// 创建新的轮次上下文
    pub fn new(turn_id: TurnId, thread_id: ThreadId, messages: Vec<Message>) -> Self {
        Self {
            turn_id,
            thread_id,
            messages,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_context_creation() {
        let ctx = TurnContext::new(
            TurnId::from("turn-1"),
            ThreadId::from("thread-1"),
            vec![Message::UserMessage {
                content: "hello".into(),
            }],
        );
        assert_eq!(ctx.turn_id.0, "turn-1");
        assert_eq!(ctx.thread_id.0, "thread-1");
        assert_eq!(ctx.messages.len(), 1);
    }

    #[test]
    fn turn_context_empty_messages() {
        let ctx = TurnContext::new(
            TurnId::from("turn-0"),
            ThreadId::from("t0"),
            vec![],
        );
        assert!(ctx.messages.is_empty());
    }
}
