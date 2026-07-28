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
         DROP TABLE IF EXISTS team_members;
         DROP TABLE IF EXISTS teams;
         DROP TABLE IF EXISTS sessions;
         DROP TABLE IF EXISTS audit_log;",
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

-- Audit log: records team collaboration events for security tracking
CREATE TABLE IF NOT EXISTS audit_log (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    event_type      TEXT NOT NULL,
    actor           TEXT NOT NULL DEFAULT '',
    thread_id       TEXT NOT NULL DEFAULT '',
    target_user     TEXT NOT NULL DEFAULT '',
    permission      TEXT NOT NULL DEFAULT '',
    data            TEXT NOT NULL DEFAULT '{}',
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Index for listing audit events by thread, ordered by most recent first
CREATE INDEX IF NOT EXISTS idx_audit_log_thread
    ON audit_log(thread_id, created_at DESC);

-- Teams: one row per team
CREATE TABLE IF NOT EXISTS teams (
    id              TEXT PRIMARY KEY NOT NULL,
    name            TEXT NOT NULL,
    created_by      TEXT NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Team members: many-to-many relationship between teams and users
CREATE TABLE IF NOT EXISTS team_members (
    team_id         TEXT NOT NULL,
    user_id         TEXT NOT NULL,
    role            TEXT NOT NULL DEFAULT 'member',
    joined_at       TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (team_id, user_id),
    FOREIGN KEY (team_id) REFERENCES teams(id) ON DELETE CASCADE
);

-- Index for listing all teams a user belongs to
CREATE INDEX IF NOT EXISTS idx_team_members_user
    ON team_members(user_id);
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
        assert!(tables.contains(&"audit_log".to_string()));
        assert!(tables.contains(&"teams".to_string()));
        assert!(tables.contains(&"team_members".to_string()));
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
        assert!(indexes.iter().any(|i| i.contains("audit_log_thread")));
        assert!(indexes.iter().any(|i| i.contains("team_members_user")));
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

    #[test]
    fn foreign_key_enforcement_team_members() {
        let conn = in_memory_conn();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();

        // Insert a team_member referencing a nonexistent team should fail
        let result = conn.execute(
            "INSERT INTO team_members (team_id, user_id, role) VALUES (?1, ?2, ?3)",
            ["nonexistent-team", "user-1", "member"],
        );
        assert!(result.is_err());
    }
}
