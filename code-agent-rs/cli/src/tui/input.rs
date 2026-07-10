//! Input bar with history and command palette.
//!
//! Provides a multi-line text input widget with:
//! - Line editing (cursor movement, insertion, deletion)
//! - History navigation (up/down arrows)
//! - Command palette detection: `/edit`, `/debug`, `/review`, `/search`, `/help`
//! - Ctrl+C interrupt handling (signaled via [`InputAction`])

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};

// ---------------------------------------------------------------------------
// InputAction — what the main loop should do
// ---------------------------------------------------------------------------

/// 输入动作
///
/// 【领域含义】输入栏产生的动作枚举，主循环必须处理这些动作。
/// 【核心职责】区分提交消息、中断 Agent、清空输入和无操作四种动作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputAction {
    /// 提交 — 将当前输入作为用户消息发送给 Agent。
    Submit(String),

    /// 中断 — 用户在输入为空时按 Ctrl+C，中断 Agent。
    Interrupt,

    /// 清空 — 用户在有文本时按 Ctrl+C，清空输入。
    Clear,

    /// 无操作 — 本次无动作。
    None,
}

// ---------------------------------------------------------------------------
// InputBar
// ---------------------------------------------------------------------------

/// 输入栏组件
///
/// 【领域含义】多行文本编辑输入栏，支持历史记录和命令面板检测。
/// 【核心职责】管理文本编辑、光标移动、历史导航和命令检测。
///
/// 支持命令面板检测：以 `/` 开头后跟识别命令名的消息会特殊高亮。
///
/// # 按键绑定
/// | 键           | 动作              |
/// |-------------|-------------------|
/// | Enter       | 提交消息           |
/// | Up/Ctrl+P   | 上一条历史         |
/// | Down/Ctrl+N | 下一条历史         |
/// | Ctrl+C      | 中断 / 清空       |
/// | Backspace   | 删除前一个字符     |
/// | Delete      | 删除后一个字符     |
/// | Home/Ctrl+A | 跳到行首          |
/// | End/Ctrl+E  | 跳到行尾          |
/// | Left/Ctrl+B | 左移光标          |
/// | Right/Ctrl+F| 右移光标          |
/*
 * 注意：按键绑定处理由 App 循环通过 crossterm KeyCode 匹配外部完成；此结构仅管理状态。
 */
pub struct InputBar {
    /// The current input text.
    text: String,

    /// Cursor position (byte index into `text`).
    cursor: usize,

    /// History of submitted inputs.
    history: Vec<String>,

    /// Current position in history navigation (-1 = typing new text).
    history_index: isize,

    /// The draft text that was being typed before navigating into history.
    draft: String,
}

impl Default for InputBar {
    fn default() -> Self {
        Self::new()
    }
}

impl InputBar {
    /// 创建输入栏
    ///
    /// 【领域含义】创建空的输入栏实例。
    /// 【核心职责】初始化文本、光标、历史和草稿状态。
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_index: -1,
            draft: String::new(),
        }
    }

    // ── Text editing ────────────────────────────────────────────────────

    /// 在光标位置插入字符
    ///
    /// 【领域含义】在光标当前位置插入一个字符。
    /// 【核心职责】更新文本并移动光标。
    pub fn insert_char(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    /// 删除光标前的字符（退格键）
    ///
    /// 【领域含义】删除光标位置前的一个字符。
    /// 【核心职责】更新文本和光标位置。
    pub fn delete_backward(&mut self) {
        if self.cursor > 0 {
            if let Some(prev) = self.text[..self.cursor].chars().last() {
                self.cursor -= prev.len_utf8();
                self.text.remove(self.cursor);
            }
        }
    }

    /// 删除光标后的字符（Delete 键）
    ///
    /// 【领域含义】删除光标位置后的一个字符。
    /// 【核心职责】更新文本。
    pub fn delete_forward(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    /// 光标左移一个字符
    ///
    /// 【领域含义】将光标向左移动一个字符位置。
    /// 【核心职责】更新光标位置。
    pub fn cursor_left(&mut self) {
        if self.cursor > 0 {
            if let Some(prev) = self.text[..self.cursor].chars().last() {
                self.cursor -= prev.len_utf8();
            }
        }
    }

    /// 光标右移一个字符
    ///
    /// 【领域含义】将光标向右移动一个字符位置。
    /// 【核心职责】更新光标位置。
    pub fn cursor_right(&mut self) {
        if self.cursor < self.text.len() {
            if let Some(next) = self.text[self.cursor..].chars().next() {
                self.cursor += next.len_utf8();
            }
        }
    }

    /// 光标跳到行首
    ///
    /// 【领域含义】将光标移动到文本起始位置。
    /// 【核心职责】设置光标为 0。
    pub fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    /// 光标跳到行尾
    ///
    /// 【领域含义】将光标移动到文本末尾位置。
    /// 【核心职责】设置光标为文本长度。
    pub fn cursor_end(&mut self) {
        self.cursor = self.text.len();
    }

    /// 删除光标到词尾
    ///
    /// 【领域含义】从光标位置删除到当前词尾（仅非空白字符）。
    /// 【核心职责】删除光标后的连续非空白字符。
    pub fn delete_word_forward(&mut self) {
        let rest = &self.text[self.cursor..];
        let bytes: usize = rest
            .chars()
            .take_while(|c| !c.is_whitespace())
            .map(|c| c.len_utf8())
            .sum();

        if bytes > 0 {
            self.text.drain(self.cursor..self.cursor + bytes);
        }
    }

    /// 删除光标前的词
    ///
    /// 【领域含义】删除光标位置前的一个词。
    /// 【核心职责】跳过空白后删除连续非空白字符。
    pub fn delete_word_backward(&mut self) {
        let before = &self.text[..self.cursor];
        let chars: Vec<char> = before.chars().rev().collect();
        let mut remove = 0;
        // Skip whitespace, then skip word chars
        let mut saw_non_space = false;
        for &ch in &chars {
            if ch.is_whitespace() {
                if saw_non_space {
                    break;
                }
                remove += ch.len_utf8();
            } else {
                saw_non_space = true;
                remove += ch.len_utf8();
            }
        }
        if remove > 0 {
            let start = self.cursor - remove;
            self.text.drain(start..self.cursor);
            self.cursor = start;
        }
    }

    // ── History ─────────────────────────────────────────────────────────

    /// 导航到上一条历史记录
    ///
    /// 【领域含义】在输入历史中向上导航。
    /// 【核心职责】如果在"新输入"位置，保存当前文本为草稿并显示最近的历史条目。
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }

        if self.history_index == -1 {
            // Save current draft before entering history
            self.draft = self.text.clone();
            self.history_index = self.history.len() as isize - 1;
        } else if self.history_index > 0 {
            self.history_index -= 1;
        }

        let idx = self.history_index as usize;
        self.text = self.history[idx].clone();
        self.cursor = self.text.len();
    }

    /// 导航到下一条历史记录
    ///
    /// 【领域含义】在输入历史中向下导航。
    /// 【核心职责】如果超过末尾，恢复保存的草稿。
    pub fn history_next(&mut self) {
        if self.history.is_empty() {
            return;
        }

        if self.history_index == -1 {
            return; // already at new input
        }

        if (self.history_index as usize) + 1 >= self.history.len() {
            // Restore draft
            self.text = self.draft.clone();
            self.draft = String::new();
            self.cursor = self.text.len();
            self.history_index = -1;
        } else {
            self.history_index += 1;
            let idx = self.history_index as usize;
            self.text = self.history[idx].clone();
            self.cursor = self.text.len();
        }
    }

    /// 提交当前输入
    ///
    /// 【领域含义】提交当前输入：保存到历史、返回文本、清空输入。
    /// 【核心职责】空提交被忽略，返回 InputAction::None。
    pub fn submit(&mut self) -> InputAction {
        let trimmed = self.text.trim().to_string();
        if trimmed.is_empty() {
            return InputAction::None;
        }

        // Save to history
        self.history.push(trimmed.clone());
        self.history_index = -1;
        self.draft.clear();
        self.text.clear();
        self.cursor = 0;

        InputAction::Submit(trimmed)
    }

    /// 处理 Ctrl+C 中断
    ///
    /// 【领域含义】处理 Ctrl+C 按键：输入为空时返回 Interrupt，有文本时清空并返回 Clear。
    /// 【核心职责】根据输入状态决定中断或清空。
    pub fn handle_interrupt(&mut self) -> InputAction {
        if self.text.is_empty() {
            InputAction::Interrupt
        } else {
            self.text.clear();
            self.cursor = 0;
            InputAction::Clear
        }
    }

    // ── Command detection ────────────────────────────────────────────────

    /// 检测斜杠命令
    ///
    /// 【领域含义】检测输入文本是否以识别的斜杠命令开头。
    /// 【核心职责】识别 /edit、/debug、/review、/search、/help 等命令。
    pub fn detect_command(text: &str) -> Option<&str> {
        let text = text.trim();
        if !text.starts_with('/') {
            return None;
        }

        let command = text.split_whitespace().next()?;

        match command {
            "/edit" | "/debug" | "/review" | "/search" | "/help" => Some(command),
            _ => None, // unrecognized command
        }
    }

    /// 判断是否为命令
    ///
    /// 【领域含义】判断当前文本是否以识别的命令开头。
    /// 【核心职责】委托给 detect_command 进行检测。
    pub fn is_command(&self) -> bool {
        Self::detect_command(&self.text).is_some()
    }

    /// 判断输入是否为空
    ///
    /// 【领域含义】判断当前输入文本是否为空。
    /// 【核心职责】检查 text 字段是否为空。
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// 获取当前文本
    ///
    /// 【领域含义】返回当前输入文本的引用。
    /// 【核心职责】供外部读取输入内容。
    pub fn text(&self) -> &str {
        &self.text
    }

    /// 获取历史记录数
    ///
    /// 【领域含义】返回输入历史中的条目数量。
    /// 【核心职责】供外部查询历史大小。
    pub fn history_len(&self) -> usize {
        self.history.len()
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

impl InputBar {
    /// Render the input bar into the given frame area.
    pub fn render(&self, f: &mut Frame, area: Rect) {
        let command = Self::detect_command(&self.text);
        let style = if command.is_some() {
            Style::default().fg(Color::Rgb(100, 200, 255))
        } else {
            Style::default().fg(Color::White)
        };

        let cursor_style = Style::default()
            .fg(Color::Black)
            .bg(Color::Rgb(180, 180, 180));

        // Build spans with cursor
        let mut spans: Vec<Span> = Vec::new();

        let before_cursor = &self.text[..self.cursor];
        let after_cursor = &self.text[self.cursor..];

        if !before_cursor.is_empty() {
            let styled_before = if let Some(cmd) = command {
                // Color the command prefix differently
                if before_cursor.len() <= cmd.len() {
                    Span::styled(
                        before_cursor.to_string(),
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    )
                } else {
                    // Command prefix + rest
                    let before_vec: Vec<Span> = vec![
                        Span::styled(
                            cmd.to_string(),
                            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            before_cursor[cmd.len()..].to_string(),
                            Style::default().fg(Color::White),
                        ),
                    ];
                    spans.extend(before_vec);
                    Span::raw("") // placeholder, already extended
                }
            } else {
                Span::styled(before_cursor.to_string(), style)
            };

            // If no command, just push the styled before
            if command.is_none() {
                spans.push(styled_before);
            }
        }

        // Cursor
        if self.cursor < self.text.len() {
            let cursor_char = after_cursor.chars().next().unwrap();
            let rest = &after_cursor[cursor_char.len_utf8()..];
            spans.push(Span::styled(cursor_char.to_string(), cursor_style));
            if !rest.is_empty() {
                spans.push(Span::styled(rest.to_string(), style));
            }
        } else {
            spans.push(Span::styled(" ", cursor_style));
        }

        let line = Line::from(spans);

        let paragraph = Paragraph::new(line)
            .block(
                Block::bordered()
                    .title(" Input ")
                    .border_style(Style::default().fg(Color::DarkGray)),
            );

        f.render_widget(paragraph, area);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Basic editing ────────────────────────────────────────────────────

    #[test]
    fn insert_char_adds_at_cursor() {
        let mut bar = InputBar::new();
        bar.insert_char('h');
        bar.insert_char('i');
        assert_eq!(bar.text(), "hi");
        assert_eq!(bar.cursor, 2);
    }

    #[test]
    fn delete_backward_removes_char() {
        let mut bar = InputBar::new();
        bar.insert_char('a');
        bar.insert_char('b');
        bar.delete_backward();
        assert_eq!(bar.text(), "a");
        assert_eq!(bar.cursor, 1);
    }

    #[test]
    fn delete_backward_at_start_is_noop() {
        let mut bar = InputBar::new();
        bar.delete_backward();
        assert_eq!(bar.text(), "");
    }

    #[test]
    fn delete_forward_removes_char() {
        let mut bar = InputBar::new();
        bar.insert_char('x');
        bar.insert_char('y');
        bar.cursor_left(); // cursor after x
        bar.delete_forward(); // removes y
        assert_eq!(bar.text(), "x");
    }

    #[test]
    fn cursor_left_right_move() {
        let mut bar = InputBar::new();
        bar.insert_char('a');
        bar.insert_char('b');
        bar.cursor_left();
        assert_eq!(bar.cursor, 1);
        bar.cursor_right();
        assert_eq!(bar.cursor, 2);
    }

    #[test]
    fn cursor_home_end() {
        let mut bar = InputBar::new();
        bar.insert_char('a');
        bar.insert_char('b');
        bar.cursor_home();
        assert_eq!(bar.cursor, 0);
        bar.cursor_end();
        assert_eq!(bar.cursor, 2);
    }

    #[test]
    fn delete_word_forward() {
        let mut bar = InputBar::new();
        for ch in "hello world".chars() {
            bar.insert_char(ch);
        }
        bar.cursor_home();
        bar.delete_word_forward();
        assert_eq!(bar.text(), " world");
    }

    #[test]
    fn delete_word_backward() {
        let mut bar = InputBar::new();
        for ch in "hello world".chars() {
            bar.insert_char(ch);
        }
        // cursor at end (after "world"), delete backward should remove "world"
        bar.delete_word_backward();
        assert_eq!(bar.text(), "hello ");
    }

    // ── History ──────────────────────────────────────────────────────────

    #[test]
    fn history_prev_next_cycle() {
        let mut bar = InputBar::new();
        bar.text = "first".into();
        bar.submit();
        bar.text = "second".into();
        bar.submit();

        assert_eq!(bar.history_len(), 2);

        // Navigate to prev → should show "second" (most recent)
        bar.history_prev();
        assert_eq!(bar.text(), "second");

        bar.history_prev();
        assert_eq!(bar.text(), "first");

        // Go back to next → "second"
        bar.history_next();
        assert_eq!(bar.text(), "second");

        // Next beyond history → draft restored
        bar.history_next();
        assert_eq!(bar.text(), ""); // draft was empty when we started
    }

    #[test]
    fn history_preserves_draft() {
        let mut bar = InputBar::new();
        bar.text = "my draft".into();
        bar.cursor = 8;

        bar.history_prev(); // empty history → noop, draft preserved
        assert_eq!(bar.text(), "my draft");

        // Add history, then test draft saving
        bar.text = "cmd1".into();
        bar.submit();

        bar.text = "my draft".into();
        bar.history_prev(); // should show "cmd1" and save "my draft" as draft
        assert_eq!(bar.text(), "cmd1");
        bar.history_next(); // should restore "my draft"
        assert_eq!(bar.text(), "my draft");
    }

    #[test]
    fn history_empty_noop() {
        let mut bar = InputBar::new();
        bar.text = "test".into();
        bar.history_prev();
        assert_eq!(bar.text(), "test"); // unchanged
    }

    // ── Submit ───────────────────────────────────────────────────────────

    #[test]
    fn submit_saves_and_clears() {
        let mut bar = InputBar::new();
        bar.text = "hello world".into();
        let action = bar.submit();

        assert_eq!(action, InputAction::Submit("hello world".into()));
        assert!(bar.text().is_empty());
        assert_eq!(bar.history_len(), 1);
    }

    #[test]
    fn submit_empty_returns_none() {
        let mut bar = InputBar::new();
        let action = bar.submit();
        assert_eq!(action, InputAction::None);
        assert_eq!(bar.history_len(), 0);
    }

    #[test]
    fn submit_whitespace_only_returns_none() {
        let mut bar = InputBar::new();
        bar.text = "   ".into();
        let action = bar.submit();
        assert_eq!(action, InputAction::None);
    }

    // ── Interrupt ────────────────────────────────────────────────────────

    #[test]
    fn interrupt_with_text_clears() {
        let mut bar = InputBar::new();
        bar.text = "something".into();
        let action = bar.handle_interrupt();
        assert_eq!(action, InputAction::Clear);
        assert!(bar.text().is_empty());
    }

    #[test]
    fn interrupt_empty_sends_interrupt() {
        let mut bar = InputBar::new();
        let action = bar.handle_interrupt();
        assert_eq!(action, InputAction::Interrupt);
    }

    // ── Command detection ────────────────────────────────────────────────

    #[test]
    fn detect_known_commands() {
        assert_eq!(InputBar::detect_command("/edit"), Some("/edit"));
        assert_eq!(InputBar::detect_command("/debug"), Some("/debug"));
        assert_eq!(InputBar::detect_command("/review"), Some("/review"));
        assert_eq!(InputBar::detect_command("/search"), Some("/search"));
        assert_eq!(InputBar::detect_command("/help"), Some("/help"));
    }

    #[test]
    fn detect_unknown_command() {
        assert_eq!(InputBar::detect_command("/unknown"), None);
        assert_eq!(InputBar::detect_command("just text"), None);
    }

    #[test]
    fn detect_command_with_args() {
        assert_eq!(
            InputBar::detect_command("/search fix the bug"),
            Some("/search")
        );
        assert_eq!(
            InputBar::detect_command("/edit src/main.rs"),
            Some("/edit")
        );
    }

    #[test]
    fn detect_command_case_sensitive() {
        assert_eq!(InputBar::detect_command("/EDIT"), None);
    }

    // ── Empty check ──────────────────────────────────────────────────────

    #[test]
    fn is_empty_checks_text() {
        let mut bar = InputBar::new();
        assert!(bar.is_empty());
        bar.insert_char('x');
        assert!(!bar.is_empty());
    }
}
