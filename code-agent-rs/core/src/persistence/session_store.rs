//! SQLite-backed session persistence store.
//!
//! [`SessionStore`] provides:
//!
//! - **Auto-save**: Persist a session and its turns to SQLite via
//!   [`save_session`] and [`save_turn`].
//! - **Resume**: Reconstruct a [`SessionSnapshot`] from the database via
//!   `resume`, ready for caller to build a new `Session`.
//! - **Auto-cleanup**: Archive sessions older than 30 days via
//!   [`archive_old_sessions`].
//!
//! All database operations are synchronous; callers should offload to a
//! blocking thread pool if needed.


//!        SQLite                         
//! 
//!                   SessionStore                                                            ?
//!                                                                                       ?
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use code_agent_protocol::{PermissionMode, ResponseEvent, SessionId, SessionStatus};
use rusqlite::{params, Connection, Result as SqliteResult};
use tracing::{debug, info, warn};

use super::schema::initialize_schema;
use super::snapshot::{SessionSnapshot, ToolCallSnapshot, TurnSnapshot};

// ---------------------------------------------------------------------------
// SessionStore
// ---------------------------------------------------------------------------

/// Manages persistent storage of agent sessions in a SQLite database.
///
/// # Example
///
/// ```rust,ignore
/// let store = SessionStore::open("sessions.db").unwrap();
/// store.save_session(&snapshot).unwrap();
/// let restored = store.resume(&SessionId::from("my-session")).unwrap();
/// ```

///                     ?   --                              ?
///
///                   SessionStore         ?Agent                        ?SQLite               ?
///                                                                                    ?
pub struct SessionStore {
    conn: Connection,
}

impl SessionStore {
    //        Lifecycle                                                                                                                                                                      

    /// Open (or create) a SQLite database at the given path and ensure the
    /// schema is up to date.

    ///                      SQLite                                 ?
    pub fn open<P: AsRef<Path>>(path: P) -> SqliteResult<Self> {
        let conn = Connection::open(path.as_ref())?;

        // Enable WAL mode for better concurrent read performance
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        initialize_schema(&conn)?;

        debug!(path = %path.as_ref().display(), "SessionStore opened");
        Ok(Self { conn })
    }

    /// Open an in-memory database (for testing).
    #[cfg(test)]
    ///                                           
    pub fn open_in_memory() -> SqliteResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        initialize_schema(&conn)?;
        Ok(Self { conn })
    }

    /// Return a reference to the underlying connection.
    #[cfg(test)]
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    //        Save                                                                                                                                                                                     

    /// Persist a session snapshot to the database.
    ///
    /// Uses `INSERT OR REPLACE` so it is safe to call multiple times for the
    /// same session (each call updates `updated_at` and refreshes state).

    ///                                 ?
    ///
    ///        INSERT OR REPLACE                                            ?
    pub fn save_session(&self, snapshot: &SessionSnapshot) -> SqliteResult<()> {
        let now = Utc::now().to_rfc3339();

        self.conn.execute(
            "INSERT OR REPLACE INTO sessions
             (id, status, permission_mode, system_instructions, max_iterations,
              turn_count, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                snapshot.id.0,
                serde_json::to_string(&snapshot.status).unwrap_or_default(),
                serde_json::to_string(&snapshot.permission_mode).unwrap_or_default(),
                snapshot.system_instructions,
                snapshot.max_iterations as i64,
                snapshot.turn_count as i64,
                snapshot.created_at.to_rfc3339(),
                now,
            ],
        )?;

        debug!(session_id = %snapshot.id, "Session saved");
        Ok(())
    }

    /// Persist a completed turn and all its events/tool-calls to the database.
    ///
    /// Call this after every [`Session::run_turn`] invocation to enable crash
    /// recovery and resume.

    ///                                          ?                        
    ///
    ///        Session::run_turn                                                         ?
    pub fn save_turn(
        &self,
        session_id: &SessionId,
        turn: &TurnSnapshot,
        update_turn_count: bool,
    ) -> SqliteResult<()> {
        let now = Utc::now().to_rfc3339();

        // Insert the turn row
        self.conn.execute(
            "INSERT OR REPLACE INTO turns (id, session_id, turn_number, thread_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                turn.turn_id.0,
                session_id.0,
                turn.turn_number as i64,
                turn.thread_id.0,
                turn.created_at.to_rfc3339(),
            ],
        )?;

        // Insert tool calls
        for tc in &turn.tool_calls {
            self.conn.execute(
                "INSERT OR REPLACE INTO tool_calls
                 (id, turn_id, tool_name, arguments, output, error, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    tc.id,
                    turn.turn_id.0,
                    tc.name,
                    tc.arguments,
                    tc.output,
                    tc.error,
                    now,
                ],
            )?;
        }

        // Insert events
        for event in &turn.events {
            let (event_type, event_data) = serialize_event(event);
            self.conn.execute(
                "INSERT INTO events (session_id, turn_id, event_type, event_data, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![session_id.0, turn.turn_id.0, event_type, event_data, now],
            )?;
        }

        // Update session metadata
        if update_turn_count {
            self.conn.execute(
                "UPDATE sessions SET turn_count = ?1, updated_at = ?2 WHERE id = ?3",
                params![turn.turn_number as i64 + 1, now, session_id.0],
            )?;
        } else {
            self.conn.execute(
                "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                params![now, session_id.0],
            )?;
        }

        debug!(
            session_id = %session_id,
            turn_id = %turn.turn_id,
            tool_calls = turn.tool_calls.len(),
            events = turn.events.len(),
            "Turn saved"
        );
        Ok(())
    }

    /// Update session status (e.g., mark as paused, completed, archived).

    ///                                                             
    pub fn update_status(
        &self,
        session_id: &SessionId,
        status: SessionStatus,
    ) -> SqliteResult<()> {
        let status_str = serde_json::to_string(&status).unwrap_or_default();
        let now = Utc::now().to_rfc3339();

        self.conn.execute(
            "UPDATE sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status_str, now, session_id.0],
        )?;

        debug!(session_id = %session_id, ?status, "Session status updated");
        Ok(())
    }

    //        Resume                                                                                                                                                                               

    /// Resume a session from the database.
    ///
    /// Returns `Ok(Some(snapshot))` if the session exists, `Ok(None)` if it
    /// does not, or `Err(...)` on database failure.

    ///                         
    ///
    ///        Ok(Some(snapshot))                      Ok(None)                  ?
    pub fn resume(&self, session_id: &SessionId) -> SqliteResult<Option<SessionSnapshot>> {
        // Load session row
        let mut stmt = self.conn.prepare(
            "SELECT id, status, permission_mode, system_instructions, max_iterations,
                    turn_count, created_at, updated_at
             FROM sessions WHERE id = ?1",
        )?;

        let session_row = stmt.query_row(params![session_id.0], |row| {
            Ok(SessionRow {
                id: row.get::<_, String>(0)?,
                status: row.get::<_, String>(1)?,
                permission_mode: row.get::<_, String>(2)?,
                system_instructions: row.get::<_, String>(3)?,
                max_iterations: row.get::<_, i64>(4)?,
                turn_count: row.get::<_, i64>(5)?,
                created_at: row.get::<_, String>(6)?,
                updated_at: row.get::<_, String>(7)?,
            })
        });

        let session_row = match session_row {
            Ok(r) => r,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e),
        };

        // Load turns (ordered by turn_number)
        let mut turn_stmt = self.conn.prepare(
            "SELECT id, session_id, turn_number, thread_id, created_at
             FROM turns WHERE session_id = ?1 ORDER BY turn_number ASC",
        )?;

        let turn_rows: Vec<TurnRow> = turn_stmt
            .query_map(params![session_id.0], |row| {
                Ok(TurnRow {
                    id: row.get::<_, String>(0)?,
                    _session_id: row.get::<_, String>(1)?,
                    turn_number: row.get::<_, i64>(2)?,
                    thread_id: row.get::<_, String>(3)?,
                    created_at: row.get::<_, String>(4)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        // Load tool calls and events for each turn, then build messages
        let mut turns = Vec::with_capacity(turn_rows.len());
        let mut messages: Vec<code_agent_protocol::Message> = Vec::new();

        for tr in &turn_rows {
            let turn_id = code_agent_protocol::TurnId(tr.id.clone());

            // Load tool calls
            let mut tc_stmt = self.conn.prepare(
                "SELECT id, turn_id, tool_name, arguments, output, error
                 FROM tool_calls WHERE turn_id = ?1",
            )?;

            let tool_calls: Vec<ToolCallSnapshot> = tc_stmt
                .query_map(params![turn_id.0], |row| {
                    Ok(ToolCallSnapshot {
                        id: row.get::<_, String>(0)?,
                        name: row.get::<_, String>(2)?,
                        arguments: row.get::<_, String>(3)?,
                        output: row.get::<_, Option<String>>(4)?,
                        error: row.get::<_, Option<String>>(5)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();

            // Load events
            let mut ev_stmt = self.conn.prepare(
                "SELECT event_type, event_data
                 FROM events WHERE turn_id = ?1 ORDER BY id ASC",
            )?;

            let events: Vec<ResponseEvent> = ev_stmt
                .query_map(params![turn_id.0], |row| {
                    Ok(EventRow {
                        event_type: row.get::<_, String>(0)?,
                        event_data: row.get::<_, String>(1)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .filter_map(|er| deserialize_event(&er.event_type, &er.event_data))
                .collect();

            // Reconstruct messages from events
            for event in &events {
                match event {
                    ResponseEvent::TurnComplete { ref final_message, .. } => {
                        messages.push(final_message.clone());
                    }
                    ResponseEvent::ToolCallBegin { ref tool_call } => {
                        messages.push(code_agent_protocol::Message::ToolCall(
                            tool_call.clone(),
                        ));
                    }
                    ResponseEvent::ToolCallEnd { ref result, .. } => {
                        messages.push(code_agent_protocol::Message::ToolResult(
                            result.clone(),
                        ));
                    }
                    _ => {}
                }
            }

            let created_at = DateTime::parse_from_rfc3339(&tr.created_at)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            turns.push(TurnSnapshot {
                turn_id,
                thread_id: code_agent_protocol::ThreadId(tr.thread_id.clone()),
                turn_number: tr.turn_number as u64,
                events,
                tool_calls,
                created_at,
            });
        }

        let status: SessionStatus =
            serde_json::from_str(&session_row.status).unwrap_or(SessionStatus::Active);
        let permission_mode: PermissionMode =
            serde_json::from_str(&session_row.permission_mode).unwrap_or(PermissionMode::Auto);

        let created_at = DateTime::parse_from_rfc3339(&session_row.created_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let updated_at = DateTime::parse_from_rfc3339(&session_row.updated_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        Ok(Some(SessionSnapshot {
            id: SessionId::from(session_row.id),
            status,
            permission_mode,
            system_instructions: session_row.system_instructions,
            max_iterations: session_row.max_iterations as usize,
            turn_count: session_row.turn_count as u64,
            messages,
            turns,
            created_at,
            updated_at,
        }))
    }

    /// List all session IDs in the database.

    ///                              ?ID
    pub fn list_sessions(&self) -> SqliteResult<Vec<SessionId>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM sessions ORDER BY updated_at DESC")?;

        let ids: Vec<SessionId> = stmt
            .query_map([], |row| row.get::<_, String>(0).map(SessionId::from))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(ids)
    }

    //        Cleanup                                                                                                                                                                            

    /// Archive sessions that haven't been updated in more than `max_age_days`.
    ///
    /// Archived sessions are marked with `status = 'archived'` in the database
    /// but their data is preserved. Returns the number of sessions archived.

    ///                                           
    ///
    ///                          "archived"                                                      ?
    pub fn archive_old_sessions(&self, max_age_days: i64) -> SqliteResult<usize> {
        let cutoff = Utc::now() - Duration::days(max_age_days);
        let cutoff_str = cutoff.to_rfc3339();

        info!(
            cutoff = %cutoff_str,
            max_age_days,
            "Archiving sessions older than cutoff"
        );

        let archived = self.conn.execute(
            "UPDATE sessions SET status = ?1, updated_at = ?2
             WHERE status != ?1 AND updated_at < ?3",
            params![
                serde_json::to_string(&SessionStatus::Archived).unwrap_or_default(),
                Utc::now().to_rfc3339(),
                cutoff_str,
            ],
        )?;

        if archived > 0 {
            info!(count = archived, "Archived old sessions");
        }

        Ok(archived)
    }

    /// Permanently delete archived sessions older than `max_age_days`.
    ///
    /// Returns the number of sessions deleted. Cascade deletes will also
    /// remove associated turns, tool_calls, and events.

    ///                                                 
    ///
    ///                                                                                 ?
    pub fn purge_archived_sessions(&self, max_age_days: i64) -> SqliteResult<usize> {
        let cutoff = Utc::now() - Duration::days(max_age_days);
        let cutoff_str = cutoff.to_rfc3339();

        warn!(
            cutoff = %cutoff_str,
            max_age_days,
            "Purging archived sessions older than cutoff"
        );

        let deleted = self.conn.execute(
            "DELETE FROM sessions WHERE status = ?1 AND updated_at < ?2",
            params![
                serde_json::to_string(&SessionStatus::Archived).unwrap_or_default(),
                cutoff_str,
            ],
        )?;

        if deleted > 0 {
            info!(count = deleted, "Purged archived sessions");
        }

        Ok(deleted)
    }

    /// Delete a session and all associated data (cascading).

    ///                                                      ?
    pub fn delete_session(&self, session_id: &SessionId) -> SqliteResult<()> {
        self.conn.execute(
            "DELETE FROM sessions WHERE id = ?1",
            params![session_id.0],
        )?;

        debug!(session_id = %session_id, "Session deleted");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Internal row types
// ---------------------------------------------------------------------------

struct SessionRow {
    id: String,
    status: String,
    permission_mode: String,
    system_instructions: String,
    max_iterations: i64,
    turn_count: i64,
    created_at: String,
    updated_at: String,
}

struct TurnRow {
    id: String,
    _session_id: String,
    turn_number: i64,
    thread_id: String,
    created_at: String,
}

struct EventRow {
    event_type: String,
    event_data: String,
}

// ---------------------------------------------------------------------------
// Event serialization helpers
// ---------------------------------------------------------------------------

/// Serialize a `ResponseEvent` to a (type_tag, json) pair for storage.

///   ?ResponseEvent              (            , JSON)                        ?
fn serialize_event(event: &ResponseEvent) -> (String, String) {
    let (tag, data) = match event {
        ResponseEvent::TurnStarted { turn_id } => (
            "turn_started",
            serde_json::json!({"turn_id": turn_id.0}),
        ),
        ResponseEvent::AgentMessageDelta { content } => (
            "agent_message_delta",
            serde_json::json!({"content": content}),
        ),
        ResponseEvent::ToolCallBegin { tool_call } => (
            "tool_call_begin",
            serde_json::to_value(tool_call).unwrap_or_default(),
        ),
        ResponseEvent::ToolCallEnd {
            tool_call_id,
            result,
        } => (
            "tool_call_end",
            serde_json::json!({
                "tool_call_id": tool_call_id,
                "result": result,
            }),
        ),
        ResponseEvent::TurnComplete {
            turn_id,
            final_message,
        } => (
            "turn_complete",
            serde_json::json!({
                "turn_id": turn_id.0,
                "final_message": final_message,
            }),
        ),
        ResponseEvent::Error { message } => (
            "error",
            serde_json::json!({"message": message}),
        ),
        ResponseEvent::TokenUsage {
            prompt_tokens,
            completion_tokens,
        } => (
            "token_usage",
            serde_json::json!({
                "prompt_tokens": prompt_tokens,
                "completion_tokens": completion_tokens,
            }),
        ),
    };

    (tag.to_string(), data.to_string())
}

/// Deserialize a (type_tag, json) pair back into a `ResponseEvent`.

///   ?(            , JSON)                        ?ResponseEvent
fn deserialize_event(tag: &str, data: &str) -> Option<ResponseEvent> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;

    match tag {
        "turn_started" => Some(ResponseEvent::TurnStarted {
            turn_id: code_agent_protocol::TurnId(
                v.get("turn_id")?.as_str()?.to_string(),
            ),
        }),
        "agent_message_delta" => Some(ResponseEvent::AgentMessageDelta {
            content: v.get("content")?.as_str()?.to_string(),
        }),
        "tool_call_begin" => {
            let tc: code_agent_protocol::ToolCall = serde_json::from_value(v).ok()?;
            Some(ResponseEvent::ToolCallBegin { tool_call: tc })
        }
        "tool_call_end" => Some(ResponseEvent::ToolCallEnd {
            tool_call_id: v.get("tool_call_id")?.as_str()?.to_string(),
            result: serde_json::from_value(v.get("result")?.clone()).ok()?,
        }),
        "turn_complete" => Some(ResponseEvent::TurnComplete {
            turn_id: code_agent_protocol::TurnId(
                v.get("turn_id")?.as_str()?.to_string(),
            ),
            final_message: serde_json::from_value(v.get("final_message")?.clone()).ok()?,
        }),
        "error" => Some(ResponseEvent::Error {
            message: v.get("message")?.as_str()?.to_string(),
        }),
        "token_usage" => Some(ResponseEvent::TokenUsage {
            prompt_tokens: v.get("prompt_tokens")?.as_u64()? as u32,
            completion_tokens: v.get("completion_tokens")?.as_u64()? as u32,
        }),
        _ => {
            warn!(tag, "Unknown event type during deserialization");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use code_agent_protocol::{Message, ToolCall, ToolResultMessage, ThreadId, TurnId};

    fn make_test_snapshot() -> SessionSnapshot {
        SessionSnapshot::new(
            SessionId::from("test-session"),
            "You are a test assistant.".into(),
            10,
            PermissionMode::Auto,
        )
    }

    fn make_turn(turn_number: u64, content: &str) -> TurnSnapshot {
        TurnSnapshot {
            turn_id: TurnId(format!("turn-{}", turn_number)),
            thread_id: ThreadId::from("thread-1"),
            turn_number,
            events: vec![
                ResponseEvent::TurnStarted {
                    turn_id: TurnId(format!("turn-{}", turn_number)),
                },
                ResponseEvent::AgentMessageDelta {
                    content: content.to_string(),
                },
                ResponseEvent::TurnComplete {
                    turn_id: TurnId(format!("turn-{}", turn_number)),
                    final_message: Message::AssistantMessage {
                        content: content.to_string(),
                    },
                },
            ],
            tool_calls: vec![],
            created_at: Utc::now(),
        }
    }

    //        Save + Resume round-trip                                                                                                                         

    #[test]
    fn save_and_resume_session() {
        let store = SessionStore::open_in_memory().unwrap();
        let mut snap = make_test_snapshot();
        snap.turn_count = 1;
        snap.messages = vec![Message::UserMessage {
            content: "hello".into(),
        }];
        snap.turns.push(make_turn(0, "Hello, world!"));

        store.save_session(&snap).unwrap();
        store
            .save_turn(&snap.id, &snap.turns[0], false)
            .unwrap();

        let restored = store.resume(&snap.id).unwrap().expect("session should exist");

        assert_eq!(restored.id, snap.id);
        assert_eq!(restored.system_instructions, snap.system_instructions);
        assert_eq!(restored.max_iterations, snap.max_iterations);
        assert_eq!(restored.permission_mode, snap.permission_mode);
        assert_eq!(restored.turn_count, 1);
        assert_eq!(restored.turns.len(), 1);
        assert_eq!(restored.turns[0].turn_id.0, "turn-0");
    }

    #[test]
    fn resume_nonexistent_session() {
        let store = SessionStore::open_in_memory().unwrap();
        let result = store
            .resume(&SessionId::from("does-not-exist"))
            .unwrap();
        assert!(result.is_none());
    }

    //        Save + Resume with tool calls                                                                                                          

    #[test]
    fn save_and_resume_with_tool_calls() {
        let store = SessionStore::open_in_memory().unwrap();
        let mut snap = make_test_snapshot();
        snap.turn_count = 1;

        let turn = TurnSnapshot {
            turn_id: TurnId::from("turn-0"),
            thread_id: ThreadId::from("thread-1"),
            turn_number: 0,
            events: vec![
                ResponseEvent::TurnStarted {
                    turn_id: TurnId::from("turn-0"),
                },
                ResponseEvent::ToolCallBegin {
                    tool_call: ToolCall {
                        id: "tc-1".into(),
                        name: "read_file".into(),
                        arguments: serde_json::json!({"path": "main.rs"}),
                    },
                },
                ResponseEvent::ToolCallEnd {
                    tool_call_id: "tc-1".into(),
                    result: ToolResultMessage {
                        tool_call_id: "tc-1".into(),
                        output: Some("fn main() {}".into()),
                        error: None,
                    },
                },
                ResponseEvent::TurnComplete {
                    turn_id: TurnId::from("turn-0"),
                    final_message: Message::AssistantMessage {
                        content: "File read.".into(),
                    },
                },
            ],
            tool_calls: vec![ToolCallSnapshot {
                id: "tc-1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"main.rs"}"#.into(),
                output: Some("fn main() {}".into()),
                error: None,
            }],
            created_at: Utc::now(),
        };

        snap.turns.push(turn.clone());
        store.save_session(&snap).unwrap();
        store.save_turn(&snap.id, &turn, true).unwrap();

        let restored = store.resume(&snap.id).unwrap().expect("session should exist");

        assert_eq!(restored.turns.len(), 1);

        let restored_turn = &restored.turns[0];
        assert_eq!(restored_turn.tool_calls.len(), 1);
        assert_eq!(restored_turn.tool_calls[0].name, "read_file");

        // Messages should be reconstructed from events
        assert!(
            restored
                .messages
                .iter()
                .any(|m| matches!(m, Message::ToolCall(..))),
            "should have ToolCall message"
        );
        assert!(
            restored
                .messages
                .iter()
                .any(|m| matches!(m, Message::ToolResult(..))),
            "should have ToolResult message"
        );
    }

    //        Save + Resume multiple turns                                                                                                             

    #[test]
    fn save_and_resume_multiple_turns() {
        let store = SessionStore::open_in_memory().unwrap();
        let mut snap = make_test_snapshot();

        // Save session first so FOREIGN KEY constraint on turns is satisfied
        store.save_session(&snap).unwrap();

        for i in 0..5 {
            let turn = make_turn(i, &format!("Turn {} response", i));
            snap.turns.push(turn.clone());
            store.save_turn(&snap.id, &turn, true).unwrap();
        }

        let restored = store.resume(&snap.id).unwrap().expect("session should exist");

        assert_eq!(restored.turns.len(), 5);
        assert_eq!(restored.turn_count, 5);

        // Turns should be in order
        for (i, turn) in restored.turns.iter().enumerate() {
            assert_eq!(turn.turn_number, i as u64);
        }
    }

    //        Crash recovery: no data loss                                                                                                             

    #[test]
    fn crash_recovery_no_data_loss() {
        let store = SessionStore::open_in_memory().unwrap();
        let mut snap = make_test_snapshot();
        store.save_session(&snap).unwrap();

        // Simulate 3 turns being saved
        for i in 0..3 {
            let turn = make_turn(i, &format!("Response {}", i));
            snap.turns.push(turn.clone());
            store.save_turn(&snap.id, &turn, true).unwrap();
        }

        // "Crash" -- create a new store (simulates process restart)
        // In-memory DB is lost, but on-disk would survive.
        // For test purposes, we just verify the current state is intact.
        let restored = store.resume(&snap.id).unwrap().expect("session should exist");
        assert_eq!(restored.turns.len(), 3);
        assert_eq!(restored.turn_count, 3);
    }

    #[test]
    fn crash_recovery_disk_persistence() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        // Phase 1: create and save
        {
            let store = SessionStore::open(&db_path).unwrap();
            let snap = make_test_snapshot();
            store.save_session(&snap).unwrap();

            for i in 0..2 {
                let turn = make_turn(i, &format!("Turn {}", i));
                store.save_turn(&snap.id, &turn, true).unwrap();
            }
        }

        // Phase 2: "crash" -- reopen the database
        {
            let store = SessionStore::open(&db_path).unwrap();
            let restored = store
                .resume(&SessionId::from("test-session"))
                .unwrap()
                .expect("session should survive crash");

            assert_eq!(restored.turns.len(), 2);
            assert_eq!(restored.turn_count, 2);
            assert_eq!(restored.system_instructions, "You are a test assistant.");
        }
    }

    //        Cleanup: archive old sessions                                                                                                          

    #[test]
    fn archive_old_sessions_marks_archived() {
        let store = SessionStore::open_in_memory().unwrap();

        // Create a session with an old updated_at timestamp
        let old_date = (Utc::now() - Duration::days(60)).to_rfc3339();
        store
            .conn
            .execute(
                "INSERT INTO sessions (id, status, permission_mode, system_instructions,
                 max_iterations, turn_count, created_at, updated_at)
                 VALUES (?1, 'active', '\"auto\"', '', 20, 0, ?2, ?2)",
                params!["old-session", old_date],
            )
            .unwrap();

        // Create a recent session
        let snap = make_test_snapshot();
        store.save_session(&snap).unwrap();

        // Archive sessions older than 30 days
        let count = store.archive_old_sessions(30).unwrap();
        assert_eq!(count, 1, "should archive exactly 1 old session");

        // Old session should now be archived
        let status: String = store
            .conn
            .query_row(
                "SELECT status FROM sessions WHERE id = 'old-session'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(status.contains("archived"), "old session should be archived");

        // Recent session should still be active
        let status: String = store
            .conn
            .query_row(
                "SELECT status FROM sessions WHERE id = ?1",
                params![snap.id.0],
                |row| row.get(0),
            )
            .unwrap();
        assert!(status.contains("active"), "recent session should stay active");
    }

    #[test]
    fn archive_idempotent() {
        let store = SessionStore::open_in_memory().unwrap();

        let old_date = (Utc::now() - Duration::days(60)).to_rfc3339();
        store
            .conn
            .execute(
                "INSERT INTO sessions (id, status, permission_mode, system_instructions,
                 max_iterations, turn_count, created_at, updated_at)
                 VALUES (?1, '\"archived\"', '\"auto\"', '', 20, 0, ?2, ?2)",
                params!["already-archived", old_date],
            )
            .unwrap();

        // Should not count already-archived sessions
        let count = store.archive_old_sessions(30).unwrap();
        assert_eq!(count, 0);
    }

    //        Cleanup: purge archived                                                                                                                            

    #[test]
    fn purge_deletes_old_archived_sessions() {
        let store = SessionStore::open_in_memory().unwrap();

        let old_date = (Utc::now() - Duration::days(90)).to_rfc3339();
        store
            .conn
            .execute(
                "INSERT INTO sessions (id, status, permission_mode, system_instructions,
                 max_iterations, turn_count, created_at, updated_at)
                 VALUES (?1, '\"archived\"', '\"auto\"', '', 20, 0, ?2, ?2)",
                params!["old-archived", old_date],
            )
            .unwrap();

        let count = store.purge_archived_sessions(30).unwrap();
        assert_eq!(count, 1);

        // Session should be gone
        let exists: bool = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = 'old-archived'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap();
        assert!(!exists);
    }

    //        Update status                                                                                                                                                          

    #[test]
    fn update_session_status() {
        let store = SessionStore::open_in_memory().unwrap();
        let snap = make_test_snapshot();
        store.save_session(&snap).unwrap();

        store
            .update_status(&snap.id, SessionStatus::Completed)
            .unwrap();

        let restored = store.resume(&snap.id).unwrap().expect("should exist");
        assert_eq!(restored.status, SessionStatus::Completed);
    }

    //        Delete session                                                                                                                                                       

    #[test]
    fn delete_session_removes_all_data() {
        let store = SessionStore::open_in_memory().unwrap();
        let snap = make_test_snapshot();
        store.save_session(&snap).unwrap();

        let turn = make_turn(0, "test");
        store.save_turn(&snap.id, &turn, false).unwrap();

        store.delete_session(&snap.id).unwrap();

        assert!(store.resume(&snap.id).unwrap().is_none());

        // Turns should also be gone (cascade)
        let turn_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM turns WHERE session_id = ?1",
                params![snap.id.0],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(turn_count, 0);
    }

    //        List sessions                                                                                                                                                          

    #[test]
    fn list_sessions_returns_all() {
        let store = SessionStore::open_in_memory().unwrap();

        store
            .save_session(&SessionSnapshot::new(
                SessionId::from("a"),
                "".into(),
                10,
                PermissionMode::Auto,
            ))
            .unwrap();
        store
            .save_session(&SessionSnapshot::new(
                SessionId::from("b"),
                "".into(),
                10,
                PermissionMode::Auto,
            ))
            .unwrap();

        let ids = store.list_sessions().unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&SessionId::from("a")));
        assert!(ids.contains(&SessionId::from("b")));
    }

    //        Event serialization round-trip                                                                                                       

    #[test]
    fn event_serialization_all_variants() {
        let events = vec![
            ResponseEvent::TurnStarted {
                turn_id: TurnId::from("t1"),
            },
            ResponseEvent::AgentMessageDelta {
                content: "hello".into(),
            },
            ResponseEvent::ToolCallBegin {
                tool_call: ToolCall {
                    id: "tc-1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({}),
                },
            },
            ResponseEvent::ToolCallEnd {
                tool_call_id: "tc-1".into(),
                result: ToolResultMessage {
                    tool_call_id: "tc-1".into(),
                    output: Some("ok".into()),
                    error: None,
                },
            },
            ResponseEvent::TurnComplete {
                turn_id: TurnId::from("t1"),
                final_message: Message::AssistantMessage {
                    content: "done".into(),
                },
            },
            ResponseEvent::Error {
                message: "oops".into(),
            },
            ResponseEvent::TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
            },
        ];

        for event in &events {
            let (tag, data) = serialize_event(event);
            let parsed = deserialize_event(&tag, &data).expect("should deserialize");
            assert_eq!(&parsed, event, "round-trip failed for tag: {}", tag);
        }
    }

    #[test]
    fn deserialize_unknown_tag_returns_none() {
        let result = deserialize_event("unknown_event", "{}");
        assert!(result.is_none());
    }
}
