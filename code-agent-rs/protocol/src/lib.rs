//! 共享协议类型与核心数据模型 (Shared Protocol Types & Core Data Models)
//!
//! 本 crate 定义了 `code-agent-rs` 工作空间中所有其他 crate 共同依赖的基础类型。
//! 所有公开类型均实现 [`Serialize`]、[`Deserialize`]、[`Clone`] 和 [`Debug`]。
//!
//! # DDD 包结构 (DDD Package Organization)
//!
//! 类型按限界上下文 (Bounded Context) 组织为四层：
//!
//! | 层级 | 模块 | 核心类型 | 职责 |
//! |------|------|---------|------|
//! | **标识层** | [`identity`] | [`SessionId`], [`ThreadId`], [`TurnId`] | 领域标识值对象 |
//! | **消息层** | [`message`] | [`Message`], [`ToolCall`], [`ToolResultMessage`] | 对话领域事件 |
//! | **执行层** | [`execution`] | [`TurnInput`], [`ResponseEvent`] | 轮次生命周期事件流 |
//! | **配置层** | [`config`] | [`SessionStatus`], [`PermissionMode`], [`CapabilityLevel`] | 状态与安全策略值对象 |
//!
//! # 向后兼容 (Backward Compatibility)
//!
//! 所有公开类型从 crate 根层级重导出 (re-export)。
//! 外部 crate 可继续使用 `use code_agent_protocol::SessionId;` 导入，
//! 也可使用完整路径 `use code_agent_protocol::identity::SessionId;`。
//!
//! # 示例
//!
//! ```rust
//! use code_agent_protocol::{SessionId, Message, ResponseEvent};
//!
//! let sid = SessionId::new("sess-abc");
//! let msg = Message::UserMessage { content: "hello".into() };
//! ```

// ── 子模块声明 (Sub-module Declarations) ──────────────────────────────

/// 标识层 — 领域标识值对象
///
/// 定义 [`SessionId`]、[`ThreadId`]、[`TurnId`] 三个不可变值对象，
/// 作为系统中最底层的标识抽象。
pub mod identity;

/// 消息层 — 对话领域事件
///
/// 定义 [`Message`] 枚举及其关联结构体 [`ToolCall`] 和 [`ToolResultMessage`]，
/// 覆盖 Agent 对话中用户、AI、工具三者之间的所有交互形式。
pub mod message;

/// 执行层 — 轮次生命周期与事件流
///
/// 定义 [`TurnInput`]（轮次输入命令）和 [`ResponseEvent`]（执行事件流），
/// 以事件溯源模式描述 Agent 的完整执行过程。
pub mod execution;

/// 配置层 — 会话状态与安全策略值对象
///
/// 定义 [`SessionStatus`]、[`PermissionMode`]、[`CapabilityLevel`] 三个枚举，
/// 控制 Agent 的生命周期阶段、权限交互模式和操作能力边界。
pub mod config;

// ── 根层级重导出 (Root-level Re-exports for backward compatibility) ──

pub use config::{CapabilityLevel, PermissionMode, SessionStatus};
pub use execution::{ResponseEvent, TurnInput};
pub use identity::{SessionId, ThreadId, TurnId, UserId};
pub use message::{Message, ToolCall, ToolResultMessage};

// ── 测试 (Tests) ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    // --- Identity types ---

    #[test]
    fn session_id_round_trip() {
        let id = SessionId("sess-abc-123".into());
        let json = serde_json::to_string(&id).expect("serialize");
        let parsed: SessionId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, parsed);
    }

    #[test]
    fn thread_id_round_trip() {
        let id = ThreadId("thread-xyz-456".into());
        let json = serde_json::to_string(&id).expect("serialize");
        let parsed: ThreadId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, parsed);
    }

    #[test]
    fn turn_id_round_trip() {
        let id = TurnId("turn-001".into());
        let json = serde_json::to_string(&id).expect("serialize");
        let parsed: TurnId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, parsed);
    }

    #[test]
    fn session_id_from_string() {
        let id: SessionId = String::from("my-session").into();
        assert_eq!(id.0, "my-session");
    }

    #[test]
    fn session_id_from_str() {
        let id: SessionId = "my-session".into();
        assert_eq!(id.0, "my-session");
    }

    #[test]
    fn identity_display() {
        let sid = SessionId("s1".into());
        let tid = ThreadId("t1".into());
        let turn = TurnId("u1".into());
        assert_eq!(sid.to_string(), "s1");
        assert_eq!(tid.to_string(), "t1");
        assert_eq!(turn.to_string(), "u1");
    }

    // --- Message type ---

    #[test]
    fn message_user_round_trip() {
        let msg = Message::UserMessage {
            content: "Hello, agent!".into(),
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        let parsed: Message = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(msg, parsed);
    }

    #[test]
    fn message_assistant_round_trip() {
        let msg = Message::AssistantMessage {
            content: "I'll help with that.".into(),
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        let parsed: Message = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(msg, parsed);
    }

    #[test]
    fn message_tool_call_round_trip() {
        let tc = ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "src/main.rs"}),
        };
        let msg = Message::ToolCall(tc);
        let json = serde_json::to_string(&msg).expect("serialize");
        let parsed: Message = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(msg, parsed);
    }

    #[test]
    fn message_tool_result_round_trip() {
        let tr = ToolResultMessage {
            tool_call_id: "call-1".into(),
            output: Some("fn main() {\n    println!(\"hello\");\n}\n".into()),
            error: None,
        };
        let msg = Message::ToolResult(tr);
        let json = serde_json::to_string(&msg).expect("serialize");
        let parsed: Message = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(msg, parsed);
    }

    #[test]
    fn message_user_json_structure() {
        let msg = Message::UserMessage {
            content: "hi".into(),
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(value["user_message"]["content"], "hi");
    }

    // --- ToolCall ---

    #[test]
    fn tool_call_round_trip() {
        let tc = ToolCall {
            id: "call-2".into(),
            name: "bash".into(),
            arguments: serde_json::json!({"cmd": "cargo test"}),
        };
        let json = serde_json::to_string(&tc).expect("serialize");
        let parsed: ToolCall = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(tc, parsed);
    }

    #[test]
    fn tool_call_json_keys() {
        let tc = ToolCall {
            id: "call-3".into(),
            name: "grep".into(),
            arguments: serde_json::json!({"pattern": "TODO"}),
        };
        let json = serde_json::to_string(&tc).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(value["id"], "call-3");
        assert_eq!(value["name"], "grep");
        assert!(value["arguments"]["pattern"] == "TODO");
    }

    // --- ToolResultMessage ---

    #[test]
    fn tool_result_success_round_trip() {
        let tr = ToolResultMessage {
            tool_call_id: "call-4".into(),
            output: Some("build succeeded".into()),
            error: None,
        };
        let json = serde_json::to_string(&tr).expect("serialize");
        let parsed: ToolResultMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(tr, parsed);
        assert!(parsed.is_success());
        assert!(!parsed.is_error());
    }

    #[test]
    fn tool_result_error_round_trip() {
        let tr = ToolResultMessage {
            tool_call_id: "call-5".into(),
            output: None,
            error: Some("permission denied".into()),
        };
        let json = serde_json::to_string(&tr).expect("serialize");
        let parsed: ToolResultMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(tr, parsed);
        assert!(parsed.is_error());
        assert!(!parsed.is_success());
    }

    #[test]
    fn tool_result_empty_round_trip() {
        // Both output and error are None (tool produced no output and no error)
        let tr = ToolResultMessage {
            tool_call_id: "call-6".into(),
            output: None,
            error: None,
        };
        let json = serde_json::to_string(&tr).expect("serialize");
        let parsed: ToolResultMessage = serde_json::from_str(&json).expect("deserialize");

        // After round-trip, both fields should be None
        assert_eq!(parsed.output, None);
        assert_eq!(parsed.error, None);

        // Check JSON does not include null fields due to skip_serializing_if
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        // output and error should be absent (not null)
        assert!(!value.as_object().unwrap().contains_key("output"));
        assert!(!value.as_object().unwrap().contains_key("error"));
    }

    #[test]
    fn tool_result_deserialize_json_null_as_none() {
        // JSON with explicit null should deserialize to None
        let json = r#"{"tool_call_id":"call-x","output":null,"error":null}"#;
        let parsed: ToolResultMessage = serde_json::from_str(json).expect("deserialize");
        assert_eq!(parsed.output, None);
        assert_eq!(parsed.error, None);
    }

    // --- TurnInput ---

    #[test]
    fn turn_input_round_trip() {
        let input = TurnInput {
            thread_id: ThreadId("th-1".into()),
            messages: vec![
                Message::UserMessage {
                    content: "fix the bug".into(),
                },
            ],
        };
        let json = serde_json::to_string(&input).expect("serialize");
        let parsed: TurnInput = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(input, parsed);
    }

    #[test]
    fn turn_input_empty_messages() {
        let input = TurnInput {
            thread_id: ThreadId("th-empty".into()),
            messages: vec![],
        };
        let json = serde_json::to_string(&input).expect("serialize");
        let parsed: TurnInput = serde_json::from_str(&json).expect("deserialize");
        assert!(parsed.messages.is_empty());
    }

    // --- ResponseEvent ---

    #[test]
    fn response_event_turn_started_round_trip() {
        let ev = ResponseEvent::TurnStarted {
            turn_id: TurnId("turn-1".into()),
        };
        let json = serde_json::to_string(&ev).expect("serialize");
        let parsed: ResponseEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ev, parsed);
    }

    #[test]
    fn response_event_agent_message_delta_round_trip() {
        let ev = ResponseEvent::AgentMessageDelta {
            content: "I think".into(),
        };
        let json = serde_json::to_string(&ev).expect("serialize");
        let parsed: ResponseEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ev, parsed);
    }

    #[test]
    fn response_event_tool_call_begin_round_trip() {
        let tc = ToolCall {
            id: "tc-1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "src/lib.rs"}),
        };
        let ev = ResponseEvent::ToolCallBegin {
            tool_call: tc.clone(),
        };
        let json = serde_json::to_string(&ev).expect("serialize");
        let parsed: ResponseEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ev, parsed);
    }

    #[test]
    fn response_event_tool_call_end_round_trip() {
        let result = ToolResultMessage {
            tool_call_id: "tc-2".into(),
            output: Some("ok".into()),
            error: None,
        };
        let ev = ResponseEvent::ToolCallEnd {
            tool_call_id: "tc-2".into(),
            result: result.clone(),
        };
        let json = serde_json::to_string(&ev).expect("serialize");
        let parsed: ResponseEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ev, parsed);
    }

    #[test]
    fn response_event_turn_complete_round_trip() {
        let final_message = Message::AssistantMessage {
            content: "Done!".into(),
        };
        let ev = ResponseEvent::TurnComplete {
            turn_id: TurnId("turn-1".into()),
            final_message: final_message.clone(),
        };
        let json = serde_json::to_string(&ev).expect("serialize");
        let parsed: ResponseEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ev, parsed);
    }

    #[test]
    fn response_event_error_round_trip() {
        let ev = ResponseEvent::Error {
            message: "something went wrong".into(),
        };
        let json = serde_json::to_string(&ev).expect("serialize");
        let parsed: ResponseEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ev, parsed);
    }

    #[test]
    fn response_event_token_usage_round_trip() {
        let ev = ResponseEvent::TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
        };
        let json = serde_json::to_string(&ev).expect("serialize");
        let parsed: ResponseEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ev, parsed);
    }

    // --- SessionStatus ---

    #[test]
    fn session_status_round_trip() {
        for status in [
            SessionStatus::Active,
            SessionStatus::Paused,
            SessionStatus::Completed,
            SessionStatus::Archived,
        ] {
            let json = serde_json::to_string(&status).expect("serialize");
            let parsed: SessionStatus = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(status, parsed);
        }
    }

    #[test]
    fn session_status_json_values() {
        let active = serde_json::to_string(&SessionStatus::Active).expect("serialize");
        assert_eq!(active, "\"active\"");

        let archived = serde_json::to_string(&SessionStatus::Archived).expect("serialize");
        assert_eq!(archived, "\"archived\"");
    }

    // --- PermissionMode ---

    #[test]
    fn permission_mode_round_trip() {
        for mode in [
            PermissionMode::Auto,
            PermissionMode::Permit,
            PermissionMode::Block,
        ] {
            let json = serde_json::to_string(&mode).expect("serialize");
            let parsed: PermissionMode = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(mode, parsed);
        }
    }

    #[test]
    fn permission_mode_json_values() {
        let auto = serde_json::to_string(&PermissionMode::Auto).expect("serialize");
        assert_eq!(auto, "\"auto\"");
        let block = serde_json::to_string(&PermissionMode::Block).expect("serialize");
        assert_eq!(block, "\"block\"");
    }

    // --- CapabilityLevel ---

    #[test]
    fn capability_level_round_trip() {
        for level in [
            CapabilityLevel::Read,
            CapabilityLevel::Edit,
            CapabilityLevel::Exec,
        ] {
            let json = serde_json::to_string(&level).expect("serialize");
            let parsed: CapabilityLevel = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(level, parsed);
        }
    }

    #[test]
    fn capability_level_json_values() {
        let read = serde_json::to_string(&CapabilityLevel::Read).expect("serialize");
        assert_eq!(read, "\"read\"");
        let exec = serde_json::to_string(&CapabilityLevel::Exec).expect("serialize");
        assert_eq!(exec, "\"exec\"");
    }

    #[test]
    fn capability_level_ordering() {
        assert!(CapabilityLevel::Read < CapabilityLevel::Edit);
        assert!(CapabilityLevel::Edit < CapabilityLevel::Exec);
        assert!(CapabilityLevel::Read < CapabilityLevel::Exec);
    }

    // --- Integration tests ---

    #[test]
    fn full_turn_lifecycle_serialization() {
        // Simulate a complete turn lifecycle through events
        let events = vec![
            ResponseEvent::TurnStarted {
                turn_id: TurnId("turn-full".into()),
            },
            ResponseEvent::AgentMessageDelta {
                content: "Let me".into(),
            },
            ResponseEvent::AgentMessageDelta {
                content: " check that.".into(),
            },
            ResponseEvent::ToolCallBegin {
                tool_call: ToolCall {
                    id: "tc-full".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "src/main.rs"}),
                },
            },
            ResponseEvent::ToolCallEnd {
                tool_call_id: "tc-full".into(),
                result: ToolResultMessage {
                    tool_call_id: "tc-full".into(),
                    output: Some("file contents".into()),
                    error: None,
                },
            },
            ResponseEvent::TokenUsage {
                prompt_tokens: 200,
                completion_tokens: 30,
            },
            ResponseEvent::TurnComplete {
                turn_id: TurnId("turn-full".into()),
                final_message: Message::AssistantMessage {
                    content: "The file looks good.".into(),
                },
            },
        ];

        // Serialize and deserialize every event
        for event in &events {
            let json = serde_json::to_string(event).expect("serialize");
            let parsed: ResponseEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(event, &parsed, "round-trip failed for event");
        }

        // Full list round-trip
        let json = serde_json::to_string(&events).expect("serialize");
        let parsed: Vec<ResponseEvent> = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(events, parsed);
    }

    #[test]
    fn malformed_json_returns_error() {
        // Truly malformed
        assert!(serde_json::from_str::<Message>("not json").is_err());
        // Wrong shape
        assert!(serde_json::from_str::<ToolCall>(r#"{"id": "x"}"#).is_err());
        // Wrong variant tag
        let result = serde_json::from_str::<Message>(r#"{"unknown_variant": "test"}"#);
        assert!(result.is_err());
    }
}
