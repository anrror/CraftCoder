//! TUI feedback prompt — rating widget shown after each agent response.
//!
//! Displays an interactive prompt asking the user to rate the last response
//! with thumbs up/down, an optional comment, and tags.
//!
//! # Key bindings
//!
//! | Key        | Action                     |
//! |-----------|----------------------------|
//! | `u`       | Thumbs up                  |
//! | `d`       | Thumbs down                |
//! | `c`       | Add/edit comment           |
//! | `t`       | Add/edit tags              |
//! | `Enter`   | Submit feedback            |
//! | `s`       | Skip (no feedback)         |
//! | `Esc`     | Cancel / dismiss           |

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
};

use code_agent_core::feedback::{FeedbackEvent, Rating};

// ---------------------------------------------------------------------------
// FeedbackPrompt
// ---------------------------------------------------------------------------

/// 反馈提示状态
///
/// 【领域含义】TUI 反馈提示的状态机枚举。
/// 【核心职责】区分隐藏、激活、已提交和已跳过四种状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedbackPromptState {
    /// 隐藏 — 提示默认隐藏。
    Hidden,
    /// 激活 — 提示可见，等待用户输入。
    Active,
    /// 已提交 — 反馈已提交。
    Submitted,
    /// 已跳过 — 用户选择跳过。
    Skipped,
}

/// 反馈提示组件
///
/// 【领域含义】TUI 反馈提示组件，在每次 Agent 响应轮次后显示，收集用户反馈。
/// 【核心职责】提供评分（赞/踩）、评论和标签输入功能，构建 FeedbackEvent。
pub struct FeedbackPrompt {
    /// Current state of the prompt.
    pub state: FeedbackPromptState,
    /// Selected rating (None = not yet selected).
    pub rating: Option<Rating>,
    /// User's comment text.
    pub comment: String,
    /// User's tags (comma-separated input).
    pub tags_input: String,
    /// Parsed tags.
    pub tags: Vec<String>,
    /// Whether the user is currently editing the comment.
    pub editing_comment: bool,
    /// Whether the user is currently editing tags.
    pub editing_tags: bool,
    /// The session ID for the current feedback.
    session_id: String,
    /// The turn ID for the current feedback.
    turn_id: String,
    /// Context snapshot to attach.
    context_snapshot: serde_json::Value,
}

impl Default for FeedbackPrompt {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedbackPrompt {
    /// 创建反馈提示
    ///
    /// 【领域含义】创建隐藏状态的反馈提示组件。
    /// 【核心职责】初始化所有字段为空/默认值。
    pub fn new() -> Self {
        Self {
            state: FeedbackPromptState::Hidden,
            rating: None,
            comment: String::new(),
            tags_input: String::new(),
            tags: Vec::new(),
            editing_comment: false,
            editing_tags: false,
            session_id: String::new(),
            turn_id: String::new(),
            context_snapshot: serde_json::json!({}),
        }
    }

    /// 显示反馈提示
    ///
    /// 【领域含义】为指定的会话/轮次显示反馈提示。
    /// 【核心职责】重置状态为激活，设置会话 ID、轮次 ID 和上下文快照。
    pub fn show(&mut self, session_id: &str, turn_id: &str, context: serde_json::Value) {
        self.state = FeedbackPromptState::Active;
        self.rating = None;
        self.comment.clear();
        self.tags_input.clear();
        self.tags.clear();
        self.editing_comment = false;
        self.editing_tags = false;
        self.session_id = session_id.to_string();
        self.turn_id = turn_id.to_string();
        self.context_snapshot = context;
    }

    /// 隐藏反馈提示
    ///
    /// 【领域含义】隐藏反馈提示。
    /// 【核心职责】设置状态为 Hidden。
    pub fn hide(&mut self) {
        self.state = FeedbackPromptState::Hidden;
    }

    /// 判断提示是否可见
    ///
    /// 【领域含义】返回反馈提示当前是否可见。
    /// 【核心职责】检查状态是否为 Active。
    pub fn is_active(&self) -> bool {
        self.state == FeedbackPromptState::Active
    }

    /// 处理按键事件
    ///
    /// 【领域含义】处理反馈提示的按键输入。
    /// 【核心职责】根据当前编辑状态（评论/标签/主界面）路由按键，返回 true 表示事件已消费。
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;

        if !self.is_active() {
            return false;
        }

        // If editing comment, route keys to comment input
        if self.editing_comment {
            match key.code {
                KeyCode::Enter => {
                    self.editing_comment = false;
                }
                KeyCode::Esc => {
                    self.editing_comment = false;
                    self.comment.clear();
                }
                KeyCode::Backspace => {
                    self.comment.pop();
                }
                KeyCode::Char(ch) => {
                    self.comment.push(ch);
                }
                _ => {}
            }
            return true;
        }

        // If editing tags, route keys to tags input
        if self.editing_tags {
            match key.code {
                KeyCode::Enter => {
                    self.tags = self
                        .tags_input
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    self.editing_tags = false;
                }
                KeyCode::Esc => {
                    self.editing_tags = false;
                    self.tags_input.clear();
                }
                KeyCode::Backspace => {
                    self.tags_input.pop();
                }
                KeyCode::Char(ch) => {
                    self.tags_input.push(ch);
                }
                _ => {}
            }
            return true;
        }

        match key.code {
            KeyCode::Char('u') | KeyCode::Char('U') => {
                self.rating = Some(Rating::ThumbsUp);
                true
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.rating = Some(Rating::ThumbsDown);
                true
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                self.editing_comment = true;
                true
            }
            KeyCode::Char('t') | KeyCode::Char('T') => {
                self.editing_tags = true;
                self.tags_input = self.tags.join(", ");
                true
            }
            KeyCode::Enter => {
                if self.rating.is_some() {
                    self.state = FeedbackPromptState::Submitted;
                }
                true
            }
            KeyCode::Char('s') | KeyCode::Char('S') => {
                self.state = FeedbackPromptState::Skipped;
                true
            }
            KeyCode::Esc => {
                self.hide();
                true
            }
            _ => false,
        }
    }

    /// 构建反馈事件
    ///
    /// 【领域含义】从当前提示状态构建 FeedbackEvent。
    /// 【核心职责】如果未选择评分则返回 None，否则构建包含评分、评论和标签的完整事件。
    pub fn build_event(&self) -> Option<FeedbackEvent> {
        self.rating.map(|rating| {
            FeedbackEvent::new(&self.session_id, &self.turn_id, rating)
                .with_comment(&self.comment)
                .with_tags(self.tags.clone())
                .with_context_snapshot(self.context_snapshot.clone())
        })
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

impl FeedbackPrompt {
    /// Render the feedback prompt into the given frame area.
    pub fn render(&self, f: &mut ratatui::Frame, area: Rect) {
        if !self.is_active() {
            return;
        }

        let lines = self.build_lines(area.width as usize);
        let paragraph = Paragraph::new(lines)
            .block(
                Block::bordered()
                    .title(" Rate this response ")
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, area);
    }

    fn build_lines(&self, _width: usize) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();

        // Rating selection
        let rating_text = match self.rating {
            Some(Rating::ThumbsUp) => " 👍 Thumbs Up (selected)".to_string(),
            Some(Rating::ThumbsDown) => " 👎 Thumbs Down (selected)".to_string(),
            None => " [u] Thumbs Up  [d] Thumbs Down".to_string(),
        };
        lines.push(Line::from(Span::styled(
            rating_text,
            Style::default().fg(Color::White),
        )));

        // Comment
        if self.editing_comment {
            lines.push(Line::from(Span::styled(
                format!(" Comment: {}█", self.comment),
                Style::default().fg(Color::Yellow),
            )));
        } else if !self.comment.is_empty() {
            lines.push(Line::from(Span::styled(
                format!(" Comment: {}", self.comment),
                Style::default().fg(Color::Green),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                " [c] Add comment",
                Style::default().fg(Color::DarkGray),
            )));
        }

        // Tags
        if self.editing_tags {
            lines.push(Line::from(Span::styled(
                format!(" Tags (comma-sep): {}█", self.tags_input),
                Style::default().fg(Color::Yellow),
            )));
        } else if !self.tags.is_empty() {
            lines.push(Line::from(Span::styled(
                format!(" Tags: {}", self.tags.join(", ")),
                Style::default().fg(Color::Green),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                " [t] Add tags",
                Style::default().fg(Color::DarkGray),
            )));
        }

        // Actions
        let can_submit = self.rating.is_some();
        let submit_style = if can_submit {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        lines.push(Line::from(vec![
            Span::styled("[Enter] Submit", submit_style),
            Span::raw("  "),
            Span::styled("[s] Skip", Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled("[Esc] Cancel", Style::default().fg(Color::DarkGray)),
        ]));

        lines
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn prompt_starts_hidden() {
        let prompt = FeedbackPrompt::new();
        assert_eq!(prompt.state, FeedbackPromptState::Hidden);
    }

    #[test]
    fn show_activates_prompt() {
        let mut prompt = FeedbackPrompt::new();
        prompt.show("s1", "t1", serde_json::json!({}));
        assert!(prompt.is_active());
    }

    #[test]
    fn hide_deactivates() {
        let mut prompt = FeedbackPrompt::new();
        prompt.show("s1", "t1", serde_json::json!({}));
        prompt.hide();
        assert!(!prompt.is_active());
    }

    #[test]
    fn key_u_selects_thumbs_up() {
        let mut prompt = FeedbackPrompt::new();
        prompt.show("s1", "t1", serde_json::json!({}));

        let key = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        assert!(prompt.handle_key(key));
        assert_eq!(prompt.rating, Some(Rating::ThumbsUp));
    }

    #[test]
    fn key_d_selects_thumbs_down() {
        let mut prompt = FeedbackPrompt::new();
        prompt.show("s1", "t1", serde_json::json!({}));

        let key = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(prompt.handle_key(key));
        assert_eq!(prompt.rating, Some(Rating::ThumbsDown));
    }

    #[test]
    fn enter_submits_when_rating_selected() {
        let mut prompt = FeedbackPrompt::new();
        prompt.show("s1", "t1", serde_json::json!({}));
        prompt.rating = Some(Rating::ThumbsUp);

        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(prompt.handle_key(key));
        assert_eq!(prompt.state, FeedbackPromptState::Submitted);
    }

    #[test]
    fn enter_does_not_submit_without_rating() {
        let mut prompt = FeedbackPrompt::new();
        prompt.show("s1", "t1", serde_json::json!({}));

        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(prompt.handle_key(key));
        assert_eq!(prompt.state, FeedbackPromptState::Active);
    }

    #[test]
    fn key_s_skips() {
        let mut prompt = FeedbackPrompt::new();
        prompt.show("s1", "t1", serde_json::json!({}));

        let key = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(prompt.handle_key(key));
        assert_eq!(prompt.state, FeedbackPromptState::Skipped);
    }

    #[test]
    fn key_esc_hides() {
        let mut prompt = FeedbackPrompt::new();
        prompt.show("s1", "t1", serde_json::json!({}));

        let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert!(prompt.handle_key(key));
        assert_eq!(prompt.state, FeedbackPromptState::Hidden);
    }

    #[test]
    fn build_event_returns_event() {
        let mut prompt = FeedbackPrompt::new();
        prompt.show("s1", "t1", serde_json::json!({"model": "test"}));
        prompt.rating = Some(Rating::ThumbsUp);
        prompt.comment = "Great!".into();
        prompt.tags = vec!["helpful".into()];

        let event = prompt.build_event().expect("should build event");
        assert_eq!(event.session_id, "s1");
        assert_eq!(event.turn_id, "t1");
        assert_eq!(event.rating, Rating::ThumbsUp);
        assert_eq!(event.comment, "Great!");
        assert_eq!(event.tags, vec!["helpful"]);
    }

    #[test]
    fn build_event_returns_none_without_rating() {
        let prompt = FeedbackPrompt::new();
        assert!(prompt.build_event().is_none());
    }

    #[test]
    fn comment_editing() {
        let mut prompt = FeedbackPrompt::new();
        prompt.show("s1", "t1", serde_json::json!({}));

        // Start editing comment
        let c_key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(prompt.handle_key(c_key));
        assert!(prompt.editing_comment);

        // Type characters
        let h_key = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
        let i_key = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE);
        prompt.handle_key(h_key);
        prompt.handle_key(i_key);
        assert_eq!(prompt.comment, "hi");

        // Enter to finish
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        prompt.handle_key(enter);
        assert!(!prompt.editing_comment);
        assert_eq!(prompt.comment, "hi");
    }

    #[test]
    fn tags_editing() {
        let mut prompt = FeedbackPrompt::new();
        prompt.show("s1", "t1", serde_json::json!({}));

        // Start editing tags
        let t_key = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE);
        assert!(prompt.handle_key(t_key));
        assert!(prompt.editing_tags);

        // Type tags
        for ch in "helpful, accurate".chars() {
            prompt.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        // Enter to finish
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        prompt.handle_key(enter);
        assert!(!prompt.editing_tags);
        assert_eq!(prompt.tags, vec!["helpful", "accurate"]);
    }

    #[test]
    fn keys_ignored_when_hidden() {
        let mut prompt = FeedbackPrompt::new();
        let key = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        assert!(!prompt.handle_key(key));
    }
}
