//! Serializable session snapshots for persistence and resume.
//!
//! Since `Session` contains non-serializable fields (`Arc<dyn ModelClient>`,
//! `Arc<ToolRegistry>`), this module provides [`SessionSnapshot`] -- a fully
//! serializable representation of the session state that can be persisted to
//! SQLite and later used to reconstruct a `Session` when combined with fresh
//! model and tool dependencies.


//!                           ?   --                            
//! 
//!                        ?SessionSnapshot   TurnSnapshot   ToolCallSnapshot                         
//!         ?Agent                        ?SQLite                       Session  ?
use chrono::{DateTime, Utc};
use code_agent_protocol::{
    Message, PermissionMode, ResponseEvent, SessionId, SessionStatus, ThreadId, TurnId,
};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// SessionSnapshot
// ---------------------------------------------------------------------------

/// A fully serializable snapshot of a session's state.
///
/// Contains everything needed to reconstruct a `Session` except for the
/// model client and tool registry, which must be provided by the caller.
///                 --                                       ?
///
///                   SessionSnapshot   ?Session                         
///                                           ID                                                                                ?
///                                                                     ?
///                                                              Session  ?
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSnapshot {
    /// Unique session identifier.
    pub id: SessionId,

    /// Current lifecycle status.
    pub status: SessionStatus,

    /// Permission mode in effect.
    pub permission_mode: PermissionMode,

    /// System instructions prepended to every model prompt.
    pub system_instructions: String,

    /// Maximum number of ReAct loop iterations per turn.
    pub max_iterations: usize,

    /// Total number of turns executed so far.
    pub turn_count: u64,

    /// Conversation history messages (ordered chronologically).
    pub messages: Vec<Message>,

    /// Completed turns in this session.
    pub turns: Vec<TurnSnapshot>,

    /// When the session was first created.
    pub created_at: DateTime<Utc>,

    /// When the session was last updated (new turn, status change, etc.).
    pub updated_at: DateTime<Utc>,
}

impl SessionSnapshot {
    /// Create a new snapshot representing an empty session.

    ///                                 ?
    pub fn new(
        id: SessionId,
        system_instructions: String,
        max_iterations: usize,
        permission_mode: PermissionMode,
    ) -> Self {
        let now = Utc::now();
        Self {
            id,
            status: SessionStatus::Active,
            permission_mode,
            system_instructions,
            max_iterations,
            turn_count: 0,
            messages: Vec::new(),
            turns: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

// ---------------------------------------------------------------------------
// TurnSnapshot
// ---------------------------------------------------------------------------

/// A serializable record of a single completed turn.
///                 --                                           
///
///                   TurnSnapshot                                             ?
///                                                      ?
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnSnapshot {
    /// The turn's unique identifier.
    pub turn_id: TurnId,

    /// The thread this turn belongs to.
    pub thread_id: ThreadId,

    /// Ordinal position within the session (0-based).
    pub turn_number: u64,

    /// All events emitted during this turn.
    pub events: Vec<ResponseEvent>,

    /// Tool calls made during this turn.
    pub tool_calls: Vec<ToolCallSnapshot>,

    /// When this turn was created.
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// ToolCallSnapshot
// ---------------------------------------------------------------------------

/// A serializable record of a single tool invocation.
///                       --                                       ?
///
///                   ToolCallSnapshot                                                            ?
///                                                            ?
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCallSnapshot {
    /// The unique ID of this tool call.
    pub id: String,

    /// The name of the tool invoked.
    pub name: String,

    /// Arguments passed to the tool (serialized as JSON string).
    pub arguments: String,

    /// The tool's output on success.
    pub output: Option<String>,

    /// Error message if the tool failed.
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use code_agent_protocol::{Message, ToolCall, ToolResultMessage};

    #[test]
    fn session_snapshot_new_has_defaults() {
        let snap = SessionSnapshot::new(
            SessionId::from("test"),
            "You are helpful.".into(),
            20,
            PermissionMode::Auto,
        );

        assert_eq!(snap.id.0, "test");
        assert_eq!(snap.status, SessionStatus::Active);
        assert_eq!(snap.permission_mode, PermissionMode::Auto);
        assert_eq!(snap.system_instructions, "You are helpful.");
        assert_eq!(snap.max_iterations, 20);
        assert_eq!(snap.turn_count, 0);
        assert!(snap.messages.is_empty());
        assert!(snap.turns.is_empty());
        assert!(snap.created_at <= Utc::now());
        assert_eq!(snap.created_at, snap.updated_at);
    }

    #[test]
    fn snapshot_round_trip_json() {
        let snap = SessionSnapshot {
            id: SessionId::from("s1"),
            status: SessionStatus::Active,
            permission_mode: PermissionMode::Auto,
            system_instructions: "sys".into(),
            max_iterations: 10,
            turn_count: 1,
            messages: vec![Message::UserMessage {
                content: "hello".into(),
            }],
            turns: vec![TurnSnapshot {
                turn_id: TurnId::from("turn-0"),
                thread_id: ThreadId::from("thread-1"),
                turn_number: 0,
                events: vec![ResponseEvent::TurnComplete {
                    turn_id: TurnId::from("turn-0"),
                    final_message: Message::AssistantMessage {
                        content: "hi!".into(),
                    },
                }],
                tool_calls: vec![],
                created_at: Utc::now(),
            }],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let json = serde_json::to_string_pretty(&snap).expect("serialize");
        let parsed: SessionSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(snap, parsed);
    }

    #[test]
    fn turn_snapshot_with_tool_calls() {
        let snap = TurnSnapshot {
            turn_id: TurnId::from("t1"),
            thread_id: ThreadId::from("th1"),
            turn_number: 0,
            events: vec![ResponseEvent::ToolCallBegin {
                tool_call: ToolCall {
                    id: "tc-1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "main.rs"}),
                },
            }],
            tool_calls: vec![ToolCallSnapshot {
                id: "tc-1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"main.rs"}"#.into(),
                output: Some("fn main() {}".into()),
                error: None,
            }],
            created_at: Utc::now(),
        };

        let json = serde_json::to_string(&snap).expect("serialize");
        let parsed: TurnSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(snap, parsed);
    }

    #[test]
    fn tool_call_snapshot_error_case() {
        let tc = ToolCallSnapshot {
            id: "tc-err".into(),
            name: "bash".into(),
            arguments: r#"{"cmd":"rm -rf /"}"#.into(),
            output: None,
            error: Some("permission denied".into()),
        };

        assert_eq!(tc.output, None);
        assert_eq!(tc.error, Some("permission denied".into()));
    }
}
