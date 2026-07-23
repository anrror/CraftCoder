//! SQLite schema for feedback collection.
//!
//! Defines the `feedback` table and associated indexes for storing
//! user feedback events (thumbs up/down, comments, tags).

use rusqlite::{Connection, Result as SqliteResult};

/// Create the feedback table and indexes if they do not already exist.
///
/// Idempotent -- safe to call on every application startup.
pub fn initialize_feedback_schema(conn: &Connection) -> SqliteResult<()> {
    conn.execute_batch(FEEDBACK_SCHEMA_DDL)
}

/// Drop the feedback table (for testing).
#[cfg(test)]
#[allow(dead_code)]
#[allow(dead_code)]
pub fn drop_feedback_table(conn: &Connection) -> SqliteResult<()> {
    conn.execute_batch("DROP TABLE IF EXISTS feedback;")
}

const FEEDBACK_SCHEMA_DDL: &str = r#"
-- Feedback: one row per user feedback event
CREATE TABLE IF NOT EXISTS feedback (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id      TEXT NOT NULL,
    turn_id         TEXT NOT NULL,
    rating          TEXT NOT NULL CHECK(rating IN ('thumbs_up', 'thumbs_down')),
    comment         TEXT NOT NULL DEFAULT '',
    tags            TEXT NOT NULL DEFAULT '[]',
    context_snapshot TEXT NOT NULL DEFAULT '{}',
    created_at      TEXT NOT NULL
);

-- Index for listing feedback by session, ordered by recency
CREATE INDEX IF NOT EXISTS idx_feedback_session
    ON feedback(session_id, created_at DESC);

-- Index for listing feedback by rating type
CREATE INDEX IF NOT EXISTS idx_feedback_rating
    ON feedback(rating);

-- Index for full export ordering
CREATE INDEX IF NOT EXISTS idx_feedback_created
    ON feedback(created_at);
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
        initialize_feedback_schema(&conn).expect("initialize feedback schema");
        conn
    }

    #[test]
    fn feedback_schema_creates_table() {
        let conn = in_memory_conn();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"feedback".to_string()));
    }

    #[test]
    fn feedback_schema_is_idempotent() {
        let conn = in_memory_conn();
        initialize_feedback_schema(&conn).expect("second call should succeed");
    }

    #[test]
    fn feedback_schema_enforces_rating_check() {
        let conn = in_memory_conn();
        let result = conn.execute(
            "INSERT INTO feedback (session_id, turn_id, rating, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["s1", "t1", "invalid_rating", "2024-01-01T00:00:00Z"],
        );
        assert!(result.is_err(), "invalid rating should be rejected");
    }

    #[test]
    fn feedback_schema_accepts_valid_ratings() {
        let conn = in_memory_conn();

        for rating in &["thumbs_up", "thumbs_down"] {
            conn.execute(
                "INSERT INTO feedback (session_id, turn_id, rating, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params!["s1", "t1", rating, "2024-01-01T00:00:00Z"],
            )
            .expect(&format!("valid rating '{}' should be accepted", rating));
        }
    }
}
