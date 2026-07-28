//! Feedback collector -->stores, retrieves, and exports user feedback.
//!
//! [`FeedbackCollector`] provides:
//!
//! - **Opt-in**: Feedback collection is disabled by default; call
//!   `enable` to activate.
//! - **Store**: Persist a [`FeedbackEvent`] to SQLite.
//! - **Retrieve**: Query feedback by session, rating, or date range.
//! - **Export**: Dump all feedback to JSONL format.
//! - **Opt-out**: Call [`disable`](FeedbackCollector::disable) to stop
//!   collection; existing data is preserved.
//!   FeedbackCollector stores feedback in SQLite with opt-in/opt-out and JSONL export.
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, Result as SqliteResult};
use tracing::debug;

use super::schema::initialize_feedback_schema;
use super::{FeedbackEvent, FeedbackSink, Rating};

// ---------------------------------------------------------------------------
// FeedbackCollector
// ---------------------------------------------------------------------------

/// Manages persistent storage of user feedback events in a SQLite database.
///
/// # Example
///
/// ```rust,ignore
/// let collector = FeedbackCollector::open("feedback.db").unwrap();
/// collector.enable();
///
/// collector.store_feedback(&FeedbackEvent {
///     session_id: "sess-1".into(),
///     turn_id: "turn-1".into(),
///     rating: Rating::ThumbsUp,
///     comment: "Great!".into(),
///     tags: vec!["helpful".into()],
///     context_snapshot: serde_json::json!({}),
///     created_at: Utc::now(),
///     id: 0,
/// }).unwrap();
/// ```
/// FeedbackCollector manages the full lifecycle of user feedback events
/// including opt-in/opt-out control, data persistence, query by session/rating, and JSONL export.
/// Disabled by default; call enable to activate.
pub struct FeedbackCollector {
    conn: Connection,
    enabled: bool,
}

impl FeedbackCollector {
    /// Open (or create) a feedback database at the given path and ensure the
    /// schema is up to date.
    ///
    /// Feedback collection starts **disabled** -- call `enable` to activate.
    /// Feedback collection starts disabled -- call enable to activate.
    pub fn open<P: AsRef<Path>>(path: P) -> SqliteResult<Self> {
        let conn = Connection::open(&path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        initialize_feedback_schema(&conn)?;

        debug!(path = %path.as_ref().display(), "FeedbackCollector opened");
        Ok(Self {
            conn,
            enabled: false,
        })
    }

    /// Open an in-memory database (for testing).
    #[cfg(test)]
    pub fn open_in_memory() -> SqliteResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        initialize_feedback_schema(&conn)?;
        Ok(Self {
            conn,
            enabled: false,
        })
    }

    /// Enable feedback collection.
    ///
    /// When enabled, `store_feedback` will persist events. When disabled,
    /// `store_feedback` is a no-op.
    pub fn enable(&mut self) {
        self.enabled = true;
        debug!("Feedback collection enabled");
    }

    /// Disable feedback collection.
    ///
    /// Existing data is preserved. New `store_feedback` calls become no-ops.
    pub fn disable(&mut self) {
        self.enabled = false;
        debug!("Feedback collection disabled");
    }

    /// Returns `true` if feedback collection is currently enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    //        Store                                                                                                                                                                                  

    /// Persist a feedback event to the database.
    ///
    /// Returns `Ok(())` if the event was stored (or skipped due to opt-out).
    /// Returns `Err` on database failure.
    pub fn store_feedback(&self, event: &FeedbackEvent) -> SqliteResult<()> {
        if !self.enabled {
            debug!("Feedback collection disabled -- skipping store");
            return Ok(());
        }

        self.conn.execute(
            "INSERT INTO feedback (session_id, turn_id, rating, comment, tags, context_snapshot, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event.session_id,
                event.turn_id,
                event.rating.to_string(),
                event.comment,
                serde_json::to_string(&event.tags).unwrap_or_default(),
                serde_json::to_string(&event.context_snapshot).unwrap_or_default(),
                event.created_at.to_rfc3339(),
            ],
        )?;

        debug!(
            session_id = %event.session_id,
            turn_id = %event.turn_id,
            rating = %event.rating,
            "Feedback stored"
        );
        Ok(())
    }

    //        Retrieve                                                                                                                                                                         

    /// Retrieve all feedback events for a given session, ordered by recency.
    pub fn get_feedback_by_session(&self, session_id: &str) -> SqliteResult<Vec<FeedbackEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, turn_id, rating, comment, tags, context_snapshot, created_at
             FROM feedback WHERE session_id = ?1 ORDER BY created_at DESC",
        )?;

        let events = stmt
            .query_map(params![session_id], |row| {
                Ok(FeedbackEvent {
                    id: row.get::<_, i64>(0)? as u64,
                    session_id: row.get::<_, String>(1)?,
                    turn_id: row.get::<_, String>(2)?,
                    rating: Rating::from_str(&row.get::<_, String>(3)?)
                        .unwrap_or(Rating::ThumbsUp),
                    comment: row.get::<_, String>(4)?,
                    tags: serde_json::from_str(&row.get::<_, String>(5)?)
                        .unwrap_or_default(),
                    context_snapshot: serde_json::from_str(&row.get::<_, String>(6)?)
                        .unwrap_or_default(),
                    created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(7)?)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(events)
    }

    /// Retrieve all feedback events with a specific rating.
    ///                                       ?
    pub fn get_feedback_by_rating(&self, rating: Rating) -> SqliteResult<Vec<FeedbackEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, turn_id, rating, comment, tags, context_snapshot, created_at
             FROM feedback WHERE rating = ?1 ORDER BY created_at DESC",
        )?;

        let events = stmt
            .query_map(params![rating.to_string()], |row| {
                Ok(FeedbackEvent {
                    id: row.get::<_, i64>(0)? as u64,
                    session_id: row.get::<_, String>(1)?,
                    turn_id: row.get::<_, String>(2)?,
                    rating: Rating::from_str(&row.get::<_, String>(3)?)
                        .unwrap_or(Rating::ThumbsUp),
                    comment: row.get::<_, String>(4)?,
                    tags: serde_json::from_str(&row.get::<_, String>(5)?)
                        .unwrap_or_default(),
                    context_snapshot: serde_json::from_str(&row.get::<_, String>(6)?)
                        .unwrap_or_default(),
                    created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(7)?)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(events)
    }

    /// Retrieve all feedback events, ordered by recency.
    ///                                             ?
    pub fn get_all_feedback(&self) -> SqliteResult<Vec<FeedbackEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, turn_id, rating, comment, tags, context_snapshot, created_at
             FROM feedback ORDER BY created_at DESC",
        )?;

        let events = stmt
            .query_map([], |row| {
                Ok(FeedbackEvent {
                    id: row.get::<_, i64>(0)? as u64,
                    session_id: row.get::<_, String>(1)?,
                    turn_id: row.get::<_, String>(2)?,
                    rating: Rating::from_str(&row.get::<_, String>(3)?)
                        .unwrap_or(Rating::ThumbsUp),
                    comment: row.get::<_, String>(4)?,
                    tags: serde_json::from_str(&row.get::<_, String>(5)?)
                        .unwrap_or_default(),
                    context_snapshot: serde_json::from_str(&row.get::<_, String>(6)?)
                        .unwrap_or_default(),
                    created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(7)?)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(events)
    }

    /// Count total feedback events.
    pub fn count(&self) -> SqliteResult<u64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM feedback", [], |row| row.get::<_, i64>(0))
            .map(|c| c as u64)
    }

    /// Count feedback events by rating.
    ///                                       ?
    pub fn count_by_rating(&self, rating: Rating) -> SqliteResult<u64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM feedback WHERE rating = ?1",
                params![rating.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c as u64)
    }

    /// Delete all feedback for a given session.
    ///                                       ?
    pub fn delete_session_feedback(&self, session_id: &str) -> SqliteResult<usize> {
        let deleted = self
            .conn
            .execute("DELETE FROM feedback WHERE session_id = ?1", params![session_id])?;
        debug!(session_id, deleted, "Deleted session feedback");
        Ok(deleted)
    }
}

// ---------------------------------------------------------------------------
// FeedbackSink impl
// ---------------------------------------------------------------------------

impl FeedbackSink for FeedbackCollector {
    fn store(&self, event: &FeedbackEvent) -> Result<(), Box<dyn std::error::Error>> {
        self.store_feedback(event).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::Rating;
    use tempfile::tempdir;

    fn sample_event(session_id: &str, turn_id: &str, rating: Rating) -> FeedbackEvent {
        FeedbackEvent {
            id: 0,
            session_id: session_id.into(),
            turn_id: turn_id.into(),
            rating,
            comment: String::new(),
            tags: vec![],
            context_snapshot: serde_json::json!({}),
            created_at: Utc::now(),
        }
    }

    //        Opt-in / opt-out                                                                                                                                                 

    #[test]
    fn feedback_starts_disabled() {
        let collector = FeedbackCollector::open_in_memory().unwrap();
        assert!(!collector.is_enabled());
    }

    #[test]
    fn enable_activates_collection() {
        let mut collector = FeedbackCollector::open_in_memory().unwrap();
        collector.enable();
        assert!(collector.is_enabled());
    }

    #[test]
    fn disable_stops_collection() {
        let mut collector = FeedbackCollector::open_in_memory().unwrap();
        collector.enable();
        collector.disable();
        assert!(!collector.is_enabled());
    }

    #[test]
    fn store_when_disabled_is_noop() {
        let collector = FeedbackCollector::open_in_memory().unwrap();
        let event = sample_event("s1", "t1", Rating::ThumbsUp);
        collector.store_feedback(&event).unwrap();
        assert_eq!(collector.count().unwrap(), 0);
    }

    #[test]
    fn store_when_enabled_persists() {
        let mut collector = FeedbackCollector::open_in_memory().unwrap();
        collector.enable();

        let event = sample_event("s1", "t1", Rating::ThumbsUp);
        collector.store_feedback(&event).unwrap();
        assert_eq!(collector.count().unwrap(), 1);
    }

    //        Store and retrieve                                                                                                                                           

    #[test]
    fn store_and_retrieve_by_session() {
        let mut collector = FeedbackCollector::open_in_memory().unwrap();
        collector.enable();

        collector
            .store_feedback(&sample_event("s1", "t1", Rating::ThumbsUp))
            .unwrap();
        collector
            .store_feedback(&sample_event("s1", "t2", Rating::ThumbsDown))
            .unwrap();
        collector
            .store_feedback(&sample_event("s2", "t1", Rating::ThumbsUp))
            .unwrap();

        let s1_events = collector.get_feedback_by_session("s1").unwrap();
        assert_eq!(s1_events.len(), 2);

        let s2_events = collector.get_feedback_by_session("s2").unwrap();
        assert_eq!(s2_events.len(), 1);
    }

    #[test]
    fn store_and_retrieve_by_rating() {
        let mut collector = FeedbackCollector::open_in_memory().unwrap();
        collector.enable();

        collector
            .store_feedback(&sample_event("s1", "t1", Rating::ThumbsUp))
            .unwrap();
        collector
            .store_feedback(&sample_event("s1", "t2", Rating::ThumbsDown))
            .unwrap();
        collector
            .store_feedback(&sample_event("s2", "t1", Rating::ThumbsUp))
            .unwrap();

        let up = collector.get_feedback_by_rating(Rating::ThumbsUp).unwrap();
        assert_eq!(up.len(), 2);

        let down = collector.get_feedback_by_rating(Rating::ThumbsDown).unwrap();
        assert_eq!(down.len(), 1);
    }

    #[test]
    fn store_with_comment_and_tags() {
        let mut collector = FeedbackCollector::open_in_memory().unwrap();
        collector.enable();

        let event = FeedbackEvent {
            id: 0,
            session_id: "s1".into(),
            turn_id: "t1".into(),
            rating: Rating::ThumbsUp,
            comment: "Very helpful!".into(),
            tags: vec!["accurate".into(), "fast".into()],
            context_snapshot: serde_json::json!({"model": "gpt-4"}),
            created_at: Utc::now(),
        };

        collector.store_feedback(&event).unwrap();

        let events = collector.get_all_feedback().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].comment, "Very helpful!");
        assert_eq!(events[0].tags, vec!["accurate", "fast"]);
        assert_eq!(events[0].context_snapshot["model"], "gpt-4");
    }

    //        Count                                                                                                                                                                                  

    #[test]
    fn count_by_rating() {
        let mut collector = FeedbackCollector::open_in_memory().unwrap();
        collector.enable();

        for _ in 0..3 {
            collector
                .store_feedback(&sample_event("s1", "t1", Rating::ThumbsUp))
                .unwrap();
        }
        for _ in 0..2 {
            collector
                .store_feedback(&sample_event("s1", "t2", Rating::ThumbsDown))
                .unwrap();
        }

        assert_eq!(collector.count().unwrap(), 5);
        assert_eq!(collector.count_by_rating(Rating::ThumbsUp).unwrap(), 3);
        assert_eq!(collector.count_by_rating(Rating::ThumbsDown).unwrap(), 2);
    }

    //        Delete                                                                                                                                                                               

    #[test]
    fn delete_session_feedback() {
        let mut collector = FeedbackCollector::open_in_memory().unwrap();
        collector.enable();

        collector
            .store_feedback(&sample_event("s1", "t1", Rating::ThumbsUp))
            .unwrap();
        collector
            .store_feedback(&sample_event("s1", "t2", Rating::ThumbsDown))
            .unwrap();
        collector
            .store_feedback(&sample_event("s2", "t1", Rating::ThumbsUp))
            .unwrap();

        let deleted = collector.delete_session_feedback("s1").unwrap();
        assert_eq!(deleted, 2);

        assert_eq!(collector.count().unwrap(), 1);
    }

    //        Disk persistence                                                                                                                                                 

    #[test]
    fn disk_persistence() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("feedback.db");

        // Phase 1: store
        {
            let mut collector = FeedbackCollector::open(&db_path).unwrap();
            collector.enable();
            collector
                .store_feedback(&sample_event("s1", "t1", Rating::ThumbsUp))
                .unwrap();
            collector
                .store_feedback(&sample_event("s1", "t2", Rating::ThumbsDown))
                .unwrap();
        }

        // Phase 2: reopen and verify
        {
            let collector = FeedbackCollector::open(&db_path).unwrap();
            // Reopening starts disabled -- but data is preserved
            let events = collector.get_all_feedback().unwrap();
            assert_eq!(events.len(), 2);
        }
    }

    //        Empty state                                                                                                                                                                

    #[test]
    fn empty_collector_returns_empty() {
        let collector = FeedbackCollector::open_in_memory().unwrap();
        assert_eq!(collector.count().unwrap(), 0);
        assert!(collector.get_all_feedback().unwrap().is_empty());
        assert!(collector
            .get_feedback_by_session("nonexistent")
            .unwrap()
            .is_empty());
    }
}
