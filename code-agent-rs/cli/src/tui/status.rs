//! Status bar renderer.
//!
//! Displays session info, connection status, and mode indicator at the bottom
//! of the TUI.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

// ---------------------------------------------------------------------------
// ConnectionStatus
// ---------------------------------------------------------------------------

/// 连接状态
///
/// 【领域含义】表示 TUI 与 Agent 引擎之间的连接状态枚举。
/// 【核心职责】区分已连接、连接中和已断开三种状态，供状态栏显示。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// 已连接 — 连接就绪。
    Connected,
    /// 连接中 — 正在连接到 Agent 引擎。
    Connecting,
    /// 已断开 — 连接丢失或失败。
    Disconnected,
}

impl ConnectionStatus {
    /// 获取状态颜色
    ///
    /// 【领域含义】返回渲染此连接状态时使用的颜色。
    /// 【核心职责】供状态栏渲染时设置颜色。
    pub fn color(self) -> Color {
        match self {
            ConnectionStatus::Connected => Color::Green,
            ConnectionStatus::Connecting => Color::Yellow,
            ConnectionStatus::Disconnected => Color::Red,
        }
    }

    /// 获取状态标签
    ///
    /// 【领域含义】返回人类可读的连接状态标签。
    /// 【核心职责】供状态栏显示文本。
    pub fn label(self) -> &'static str {
        match self {
            ConnectionStatus::Connected => "● Connected",
            ConnectionStatus::Connecting => "◌ Connecting",
            ConnectionStatus::Disconnected => "○ Disconnected",
        }
    }
}

// ---------------------------------------------------------------------------
// InputMode
// ---------------------------------------------------------------------------

/// 输入模式
///
/// 【领域含义】表示 TUI 输入模式的枚举，在状态栏中显示。
/// 【核心职责】区分普通文本输入模式和命令模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    /// 插入模式 — 普通文本输入。
    Insert,
    /// 普通模式 — 命令模式（输入 `/` 后）。
    Normal,
}

impl InputMode {
    /// 获取模式颜色
    ///
    /// 【领域含义】返回渲染此输入模式时使用的颜色。
    /// 【核心职责】供状态栏渲染时设置颜色。
    pub fn color(self) -> Color {
        match self {
            InputMode::Insert => Color::Green,
            InputMode::Normal => Color::Cyan,
        }
    }

    /// 获取模式标签
    ///
    /// 【领域含义】返回人类可读的输入模式标签。
    /// 【核心职责】供状态栏显示文本。
    pub fn label(self) -> &'static str {
        match self {
            InputMode::Insert => "INSERT",
            InputMode::Normal => "NORMAL",
        }
    }
}

// ---------------------------------------------------------------------------
// StatusBar
// ---------------------------------------------------------------------------

/// 状态栏
///
/// 【领域含义】显示会话上下文概览的状态栏组件，位于 TUI 底部。
/// 【核心职责】展示输入模式、会话 ID、模型名称、Token 用量和轮次计数。
///
/// 布局（从左到右）：
/// ```text
/// [mode] │ session: <id> │ model: <name> │ tokens: <p>/<c> │ turns: <n> ┃ <status>
/// ```
pub struct StatusBar {
    /// Connection state to the agent engine.
    pub connection: ConnectionStatus,

    /// Current input mode.
    pub mode: InputMode,

    /// Session identifier.
    pub session_id: String,

    /// Model name in use.
    pub model_name: String,

    /// Prompt tokens consumed so far.
    pub prompt_tokens: u32,

    /// Completion tokens consumed so far.
    pub completion_tokens: u32,

    /// Number of turns completed.
    pub turn_count: u64,
}

impl Default for StatusBar {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusBar {
    /// 创建状态栏
    ///
    /// 【领域含义】创建默认状态栏（已断开、插入模式、空会话）。
    /// 【核心职责】初始化所有字段为默认值。
    pub fn new() -> Self {
        Self {
            connection: ConnectionStatus::Disconnected,
            mode: InputMode::Insert,
            session_id: String::new(),
            model_name: String::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
            turn_count: 0,
        }
    }

    /// 更新 Token 用量
    ///
    /// 【领域含义】从 Agent 事件更新 Token 消耗计数。
    /// 【核心职责】累加 prompt 和 completion Token 数。
    pub fn update_tokens(&mut self, prompt: u32, completion: u32) {
        self.prompt_tokens += prompt;
        self.completion_tokens += completion;
    }

    /// 增加轮次计数
    ///
    /// 【领域含义】增加已完成的 Agent 轮次计数。
    /// 【核心职责】turn_count 加 1。
    pub fn increment_turn(&mut self) {
        self.turn_count += 1;
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

impl StatusBar {
    /// Render the status bar into the given frame area (single line).
    pub fn render(&self, f: &mut Frame, area: Rect) {
        let spans = self.build_spans(area.width as usize);
        let line = Line::from(spans);
        let paragraph = Paragraph::new(line);

        f.render_widget(paragraph, area);
    }

    /// Build styled spans for the status line.
    fn build_spans(&self, _width: usize) -> Vec<Span<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();

        // Mode indicator
        spans.push(Span::styled(
            format!(" {} ", self.mode.label()),
            Style::default()
                .fg(Color::Black)
                .bg(self.mode.color())
                .add_modifier(Modifier::BOLD),
        ));

        // Separator
        spans.push(self.separator());

        // Session ID
        let session_text = if self.session_id.is_empty() {
            " session: -- ".to_string()
        } else {
            format!(" session: {} ", self.session_id)
        };
        spans.push(Span::styled(session_text, Style::default().fg(Color::Gray)));

        spans.push(self.separator());

        // Model name
        let model_text = if self.model_name.is_empty() {
            " model: -- ".to_string()
        } else {
            format!(" model: {} ", self.model_name)
        };
        spans.push(Span::styled(model_text, Style::default().fg(Color::Gray)));

        spans.push(self.separator());

        // Token usage
        let tokens_text = format!(
            " tokens: {}/{} ",
            self.prompt_tokens, self.completion_tokens
        );
        spans.push(Span::styled(tokens_text, Style::default().fg(Color::Gray)));

        spans.push(self.separator());

        // Turn count
        let turns_text = format!(" turns: {} ", self.turn_count);
        spans.push(Span::styled(turns_text, Style::default().fg(Color::Gray)));

        // Right-align connection status
        let status_text = format!(" {} ", self.connection.label());
        spans.push(Span::styled(
            status_text,
            Style::default()
                .fg(Color::Black)
                .bg(self.connection.color()),
        ));

        spans
    }

    fn separator(&self) -> Span<'static> {
        Span::styled("│", Style::default().fg(Color::DarkGray))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_status_colors() {
        assert_eq!(ConnectionStatus::Connected.color(), Color::Green);
        assert_eq!(ConnectionStatus::Connecting.color(), Color::Yellow);
        assert_eq!(ConnectionStatus::Disconnected.color(), Color::Red);
    }

    #[test]
    fn connection_status_labels() {
        assert!(ConnectionStatus::Connected.label().contains("Connected"));
        assert!(ConnectionStatus::Disconnected.label().contains("Disconnected"));
    }

    #[test]
    fn input_mode_colors() {
        assert_eq!(InputMode::Insert.color(), Color::Green);
        assert_eq!(InputMode::Normal.color(), Color::Cyan);
    }

    #[test]
    fn input_mode_labels() {
        assert_eq!(InputMode::Insert.label(), "INSERT");
        assert_eq!(InputMode::Normal.label(), "NORMAL");
    }

    #[test]
    fn status_bar_defaults() {
        let bar = StatusBar::new();
        assert_eq!(bar.connection, ConnectionStatus::Disconnected);
        assert_eq!(bar.mode, InputMode::Insert);
        assert_eq!(bar.prompt_tokens, 0);
        assert_eq!(bar.completion_tokens, 0);
        assert_eq!(bar.turn_count, 0);
    }

    #[test]
    fn update_tokens_accumulates() {
        let mut bar = StatusBar::new();
        bar.update_tokens(100, 50);
        assert_eq!(bar.prompt_tokens, 100);
        assert_eq!(bar.completion_tokens, 50);

        bar.update_tokens(200, 30);
        assert_eq!(bar.prompt_tokens, 300);
        assert_eq!(bar.completion_tokens, 80);
    }

    #[test]
    fn increment_turn_adds_one() {
        let mut bar = StatusBar::new();
        bar.increment_turn();
        bar.increment_turn();
        assert_eq!(bar.turn_count, 2);
    }
}
