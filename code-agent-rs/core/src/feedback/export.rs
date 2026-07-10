//! JSONL export for feedback data.
//!
//! Provides streaming export of feedback events to newline-delimited JSON
//! (JSONL) format, suitable for analysis, dashboards, or ML training pipelines.


//! JSONL export for ML training pipelines and analysis.
use std::io::{BufWriter, Write};
use std::path::Path;

use serde::Serialize;

use super::FeedbackEvent;

/// Export feedback events to a JSONL file (one JSON object per line).
///
/// # Arguments
///
/// * `path` -- destination file path
/// * `events` -- feedback events to export
///
/// # Errors
///
/// Returns an I/O error if the file cannot be created or written to.

pub fn export_jsonl<P: AsRef<Path>>(
    path: P,
    events: &[FeedbackEvent],
) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut writer = BufWriter::new(file);

    for event in events {
        let line = serde_json::to_string(&ExportRow::from(event))
            .unwrap_or_default();
        writeln!(writer, "{}", line)?;
    }

    writer.flush()?;
    Ok(())
}

/// Export feedback events to a JSONL string (in-memory).

pub fn export_jsonl_string(events: &[FeedbackEvent]) -> String {
    let mut lines = Vec::with_capacity(events.len());
    for event in events {
        if let Ok(line) = serde_json::to_string(&ExportRow::from(event)) {
            lines.push(line);
        }
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Export row -- flattened representation for analysis
// ---------------------------------------------------------------------------

/// A flattened, analysis-friendly representation of a feedback event.
#[derive(Clone, Debug, Serialize)]

struct ExportRow {
    /// Session identifier.
    session_id: String,
    /// Turn identifier within the session.
    turn_id: String,
    /// Rating: "thumbs_up" or "thumbs_down".
    rating: String,
    /// Optional free-text comment.
    comment: String,
    /// Tags associated with this feedback.
    tags: Vec<String>,
    /// When the feedback was submitted.
    created_at: String,
}

impl From<&FeedbackEvent> for ExportRow {
    fn from(event: &FeedbackEvent) -> Self {
        Self {
            session_id: event.session_id.clone(),
            turn_id: event.turn_id.clone(),
            rating: event.rating.to_string(),
            comment: event.comment.clone(),
            tags: event.tags.clone(),
            created_at: event.created_at.to_rfc3339(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::Rating;
    use chrono::{DateTime, Utc};
    use tempfile::tempdir;

    fn sample_events() -> Vec<FeedbackEvent> {
        vec![
            FeedbackEvent {
                id: 1,
                session_id: "sess-1".into(),
                turn_id: "turn-1".into(),
                rating: Rating::ThumbsUp,
                comment: "Great response!".into(),
                tags: vec!["helpful".into(), "accurate".into()],
                context_snapshot: serde_json::json!({"model": "gpt-4"}),
                created_at: DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
            FeedbackEvent {
                id: 2,
                session_id: "sess-1".into(),
                turn_id: "turn-2".into(),
                rating: Rating::ThumbsDown,
                comment: "Wrong answer".into(),
                tags: vec!["incorrect".into()],
                context_snapshot: serde_json::json!({"model": "gpt-4"}),
                created_at: DateTime::parse_from_rfc3339("2024-01-01T00:01:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
        ]
    }

    #[test]
    fn export_jsonl_to_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("feedback.jsonl");
        let events = sample_events();

        export_jsonl(&path, &events).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        // Each line should be valid JSON
        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(parsed.get("session_id").is_some());
            assert!(parsed.get("rating").is_some());
            assert!(parsed.get("created_at").is_some());
        }
    }

    #[test]
    fn export_jsonl_string_output() {
        let events = sample_events();
        let output = export_jsonl_string(&events);

        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2);

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["session_id"], "sess-1");
        assert_eq!(first["rating"], "thumbs_up");
        assert_eq!(first["tags"], serde_json::json!(["helpful", "accurate"]));
    }

    #[test]
    fn export_empty_events() {
        let output = export_jsonl_string(&[]);
        assert_eq!(output, "");
    }

    #[test]
    fn export_jsonl_round_trip() {
        let events = sample_events();
        let output = export_jsonl_string(&events);

        let parsed: Vec<serde_json::Value> = output
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["session_id"], "sess-1");
        assert_eq!(parsed[0]["rating"], "thumbs_up");
        assert_eq!(parsed[1]["rating"], "thumbs_down");
    }
}
