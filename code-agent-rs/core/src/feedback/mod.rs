//! 反馈收集与评分系统
//!
//! 【领域含义】本模块负责收集、存储、查询和导出用户对 Agent 各轮回复的反馈（赞/踩），
//! 为模型评估、质量监控和持续改进提供数据基础。
//!
//! This module provides:
//!
//! - **[`FeedbackEvent`]**: Core data type for user feedback (thumbs up/down,
//!   comment, tags, context snapshot).
//! - **[`Rating`]**: Enum for thumbs up / thumbs down.
//! - **[`FeedbackCollector`]**: SQLite-backed storage with opt-in/opt-out,
//!   retrieval by session/rating, and JSONL export.
//!
//! # Architecture
//!
//! ```text
//! User -> Rating Prompt -> FeedbackEvent -> FeedbackCollector (SQLite)
//!                                              |
//!                                              +-- get_all_feedback()
//!                                              +-- get_feedback_by_session()
//!                                              +-- get_feedback_by_rating()
//!                                              +-- export_jsonl()
//! ```
//!
//! # Opt-in Design
//!
//! Feedback collection is **disabled by default**. Callers must explicitly
//! call [`FeedbackCollector::enable`] to activate. This ensures no data is
//! collected without user consent.
//!
//! # Usage
//!
//! ```rust,ignore
//! use code_agent_core::feedback::{FeedbackCollector, FeedbackEvent, Rating};
//!
//! let mut collector = FeedbackCollector::open("feedback.db")?;
//! collector.enable();
//!
//! let event = FeedbackEvent::new("sess-1", "turn-1", Rating::ThumbsUp)
//!     .with_comment("Great work!")
//!     .with_tags(vec!["helpful", "accurate"]);
//!
//! collector.store_feedback(&event)?;
//! let all = collector.get_all_feedback()?;
//! ```

mod collector;
mod export;
mod schema;

pub use collector::FeedbackCollector;
pub use export::{export_jsonl, export_jsonl_string};

// ---------------------------------------------------------------------------
// FeedbackSink — decouples TUI from concrete feedback storage
// ---------------------------------------------------------------------------

/// 反馈槽 —— 将 TUI 层与具体的反馈存储实现解耦
///
/// 【领域含义】任何可以接收并持久化反馈事件的组件都实现此 trait。
/// TUI 层仅依赖此 trait，无需知晓底层是 SQLite、文件还是 no-op。
///
/// A sink that receives and persists feedback events.
///
/// This trait decouples the TUI layer from the concrete feedback storage
/// implementation. The TUI only depends on `Box<dyn FeedbackSink>`, not
/// on `FeedbackCollector` or any specific backend.
pub trait FeedbackSink: Send {
    /// Persist a feedback event.
    ///
    /// Returns `Ok(())` on success, or an error if persistence fails.
    fn store(&self, event: &FeedbackEvent) -> Result<(), Box<dyn std::error::Error>>;
}

// ---------------------------------------------------------------------------
// NoopFeedbackSink — silently discards all events
// ---------------------------------------------------------------------------

/// 空操作反馈槽 —— 静默丢弃所有反馈事件
///
/// 【用途】当反馈数据库不可用或用户禁用了反馈收集时使用。
/// 保证 TUI 层始终有一个有效的 `FeedbackSink` 实例，无需 `Option` 判断。
///
/// A feedback sink that silently discards all events.
///
/// Used when the feedback database is unavailable or the user has disabled
/// feedback collection. Ensures the TUI always has a valid `FeedbackSink`
/// without needing an `Option` check.
pub struct NoopFeedbackSink;

impl FeedbackSink for NoopFeedbackSink {
    fn store(&self, _event: &FeedbackEvent) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
}

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::fmt;

// ---------------------------------------------------------------------------
// Rating
// ---------------------------------------------------------------------------

/// 用户评分 —— 对 Agent 某轮回复的二元评价
///
/// 【领域含义】Rating 表示用户对 Agent 回复质量的直接反馈，
/// ThumbsUp 表示满意，ThumbsDown 表示不满意，为模型优化提供标注数据。
///
/// User rating for a turn response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rating {
    /// 正面反馈 —— 回应有帮助
    /// Positive feedback -- the response was helpful.
    ThumbsUp,
    /// 负面反馈 —— 回应没有帮助
    /// Negative feedback -- the response was not helpful.
    ThumbsDown,
}

impl Rating {
    /// 从字符串解析评分
    ///
    /// 将 "thumbs_up" 或 "thumbs_down" 字符串解析为对应的 Rating 枚举值
    ///
    /// Parse a rating from its string representation.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "thumbs_up" => Some(Self::ThumbsUp),
            "thumbs_down" => Some(Self::ThumbsDown),
            _ => None,
        }
    }
}

impl fmt::Display for Rating {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ThumbsUp => write!(f, "thumbs_up"),
            Self::ThumbsDown => write!(f, "thumbs_down"),
        }
    }
}

// ---------------------------------------------------------------------------
// FeedbackEvent
// ---------------------------------------------------------------------------

/// 反馈事件 —— 用户提交的单一反馈记录
///
/// 【领域含义】FeedbackEvent 捕获用户在一次交互中对 Agent 回复的完整评价，
/// 包含评分、评论、标签以及反馈发生时的上下文快照。
/// 是反馈驱动改进（Feedback-Driven Improvement）的核心数据载体。
///
/// A single feedback event submitted by the user.
///
/// Captures the user's rating, optional comment, tags, and a snapshot of the
/// context at the time the feedback was given.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FeedbackEvent {
    /// Auto-incrementing database ID (0 if not yet stored).
    pub id: u64,
    /// The session this feedback pertains to.
    pub session_id: String,
    /// The specific turn within the session.
    pub turn_id: String,
    /// Thumbs up or thumbs down.
    pub rating: Rating,
    /// Optional free-text comment from the user.
    pub comment: String,
    /// Tags categorizing the feedback (e.g., "helpful", "incorrect", "fast").
    pub tags: Vec<String>,
    /// Snapshot of context at feedback time (model, tokens, etc.).
    pub context_snapshot: JsonValue,
    /// When the feedback was submitted.
    pub created_at: DateTime<Utc>,
}

impl FeedbackEvent {
    /// 创建新的反馈事件
    ///
    /// 使用指定的会话 ID、轮次 ID 和评分初始化反馈事件。
    /// 评论和标签默认为空，可通过 with_comment 和 with_tags 设置。
    ///
    /// Create a new feedback event with the given session, turn, and rating.
    ///
    /// Comment and tags are empty by default; use `with_comment` and
    /// `with_tags` to set them.
    pub fn new(session_id: impl Into<String>, turn_id: impl Into<String>, rating: Rating) -> Self {
        Self {
            id: 0,
            session_id: session_id.into(),
            turn_id: turn_id.into(),
            rating,
            comment: String::new(),
            tags: Vec::new(),
            context_snapshot: serde_json::json!({}),
            created_at: Utc::now(),
        }
    }

    /// 设置反馈评论（Builder 模式）
    /// Set the comment on this feedback event.
    pub fn with_comment(mut self, comment: impl Into<String>) -> Self {
        self.comment = comment.into();
        self
    }

    /// 设置反馈标签（Builder 模式）
    /// Set the tags on this feedback event.
    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = tags.into_iter().map(|t| t.into()).collect();
        self
    }

    /// 设置上下文快照（Builder 模式）
    /// Set the context snapshot on this feedback event.
    pub fn with_context_snapshot(mut self, snapshot: JsonValue) -> Self {
        self.context_snapshot = snapshot;
        self
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rating_display() {
        assert_eq!(Rating::ThumbsUp.to_string(), "thumbs_up");
        assert_eq!(Rating::ThumbsDown.to_string(), "thumbs_down");
    }

    #[test]
    fn rating_from_str() {
        assert_eq!(Rating::from_str("thumbs_up"), Some(Rating::ThumbsUp));
        assert_eq!(Rating::from_str("thumbs_down"), Some(Rating::ThumbsDown));
        assert_eq!(Rating::from_str("invalid"), None);
    }

    #[test]
    fn rating_round_trip_json() {
        for rating in &[Rating::ThumbsUp, Rating::ThumbsDown] {
            let json = serde_json::to_string(rating).unwrap();
            let parsed: Rating = serde_json::from_str(&json).unwrap();
            assert_eq!(*rating, parsed);
        }
    }

    #[test]
    fn feedback_event_new() {
        let event = FeedbackEvent::new("sess-1", "turn-1", Rating::ThumbsUp);
        assert_eq!(event.session_id, "sess-1");
        assert_eq!(event.turn_id, "turn-1");
        assert_eq!(event.rating, Rating::ThumbsUp);
        assert!(event.comment.is_empty());
        assert!(event.tags.is_empty());
        assert_eq!(event.context_snapshot, serde_json::json!({}));
    }

    #[test]
    fn feedback_event_with_comment() {
        let event = FeedbackEvent::new("s1", "t1", Rating::ThumbsDown)
            .with_comment("Wrong answer");
        assert_eq!(event.comment, "Wrong answer");
    }

    #[test]
    fn feedback_event_with_tags() {
        let event = FeedbackEvent::new("s1", "t1", Rating::ThumbsUp)
            .with_tags(vec!["helpful", "accurate"]);
        assert_eq!(event.tags, vec!["helpful", "accurate"]);
    }

    #[test]
    fn feedback_event_with_context_snapshot() {
        let snapshot = serde_json::json!({"model": "gpt-4", "tokens": 150});
        let event = FeedbackEvent::new("s1", "t1", Rating::ThumbsUp)
            .with_context_snapshot(snapshot.clone());
        assert_eq!(event.context_snapshot, snapshot);
    }

    #[test]
    fn feedback_event_round_trip_json() {
        let event = FeedbackEvent::new("sess-1", "turn-1", Rating::ThumbsUp)
            .with_comment("Great!")
            .with_tags(vec!["helpful"])
            .with_context_snapshot(serde_json::json!({"model": "gpt-4"}));

        let json = serde_json::to_string(&event).unwrap();
        let parsed: FeedbackEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, parsed);
    }
}
