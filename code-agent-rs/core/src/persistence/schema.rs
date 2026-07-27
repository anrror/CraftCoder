//! SQLite schema for session persistence.
//!
//! Defines table creation, indexes, and migration helpers. The schema uses
//! four tables to capture the full session lifecycle:
//!
//! - `sessions`: one row per agent session
//! - `turns`: one row per turn within a session
//! - `tool_calls`: one row per tool invocation within a turn
//! - `events`: all response events emitted during a turn (stored as JSON)

use rusqlite::{Connection, Result as SqliteResult};

/// Create all tables and indexes if they do not already exist.
///
/// Idempotent -- safe to call on every application startup.
///
///                                 ?   --                            
///
///                                                                        ?
///        sessions   turns   tool_calls   events                           ?
pub fn initialize_schema(conn: &Connection) -> SqliteResult<()> {
    conn.execute_batch(SCHEMA_DDL)
}

/// Drop all persistence tables (for testing).
#[cfg(test)]
///                                              
///                                              
pub fn drop_all_tables(conn: &Connection) -> SqliteResult<()> {
    conn.execute_batch(
        "DROP TABLE IF EXISTS events;
         DROP TABLE IF EXISTS tool_calls;
         DROP TABLE IF EXISTS turns;
         DROP TABLE IF EXISTS sessions;",
    )
}

const SCHEMA_DDL: &str = r#"
-- Sessions: one row per agent session
CREATE TABLE IF NOT EXISTS sessions (
    id              TEXT PRIMARY KEY NOT NULL,
    status          TEXT NOT NULL DEFAULT 'active',
    permission_mode TEXT NOT NULL DEFAULT 'auto',
    system_instructions TEXT NOT NULL DEFAULT '',
    max_iterations  INTEGER NOT NULL DEFAULT 20,
    turn_count      INTEGER NOT NULL DEFAULT 0,
    user_id         TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

-- Index for listing active sessions ordered by most-recent update
CREATE INDEX IF NOT EXISTS idx_sessions_updated
    ON sessions(updated_at DESC);

-- Index for fetching sessions by status (e.g., list all archived)
CREATE INDEX IF NOT EXISTS idx_sessions_status
    ON sessions(status);

-- Turns: one row per turn within a session
CREATE TABLE IF NOT EXISTS turns (
    id              TEXT PRIMARY KEY NOT NULL,
    session_id      TEXT NOT NULL,
    turn_number     INTEGER NOT NULL,
    thread_id       TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

-- Index for listing turns by session, ordered by turn number
CREATE INDEX IF NOT EXISTS idx_turns_session
    ON turns(session_id, turn_number);

-- Tool calls: one row per tool invocation within a turn
CREATE TABLE IF NOT EXISTS tool_calls (
    id              TEXT PRIMARY KEY NOT NULL,
    turn_id         TEXT NOT NULL,
    tool_name       TEXT NOT NULL,
    arguments       TEXT NOT NULL DEFAULT '{}',
    output          TEXT,
    error           TEXT,
    created_at      TEXT NOT NULL,
    FOREIGN KEY (turn_id) REFERENCES turns(id) ON DELETE CASCADE
);

-- Index for listing tool calls by turn
CREATE INDEX IF NOT EXISTS idx_tool_calls_turn
    ON tool_calls(turn_id);

-- Events: all response events emitted during a turn
CREATE TABLE IF NOT EXISTS events (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id      TEXT NOT NULL,
    turn_id         TEXT NOT NULL,
    event_type      TEXT NOT NULL,
    event_data      TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    FOREIGN KEY (turn_id) REFERENCES turns(id) ON DELETE CASCADE
);

-- Index for listing events by session + turn, ordered by insertion order
CREATE INDEX IF NOT EXISTS idx_events_session_turn
    ON events(session_id, turn_id, id);

-- Index for looking up events by type (e.g., all errors)
CREATE INDEX IF NOT EXISTS idx_events_type
    ON events(session_id, event_type);
"#;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn in_memory_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        initialize_schema(&conn).expect("initialize schema");
        conn
    }

    #[test]
    fn schema_creates_tables() {
        let conn = in_memory_conn();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"sessions".to_string()));
        assert!(tables.contains(&"turns".to_string()));
        assert!(tables.contains(&"tool_calls".to_string()));
        assert!(tables.contains(&"events".to_string()));
    }

    #[test]
    fn schema_is_idempotent() {
        let conn = in_memory_conn();
        // Running initialize_schema again should not error
        initialize_schema(&conn).expect("second call should succeed");
    }

    #[test]
    fn indexes_created() {
        let conn = in_memory_conn();

        let indexes: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(indexes.iter().any(|i| i.contains("sessions_updated")));
        assert!(indexes.iter().any(|i| i.contains("sessions_status")));
        assert!(indexes.iter().any(|i| i.contains("turns_session")));
        assert!(indexes.iter().any(|i| i.contains("tool_calls_turn")));
        assert!(indexes.iter().any(|i| i.contains("events_session_turn")));
        assert!(indexes.iter().any(|i| i.contains("events_type")));
    }

    #[test]
    fn foreign_key_enforcement() {
        let conn = in_memory_conn();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();

        // Insert a turn referencing a nonexistent session should fail
        let result = conn.execute(
            "INSERT INTO turns (id, session_id, turn_number, created_at) VALUES (?1, ?2, ?3, ?4)",
            ["turn-1", "nonexistent-session", "1", "2024-01-01T00:00:00Z"],
        );
        assert!(result.is_err());
    }
}
