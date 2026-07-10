//! Chat panel renderer.
//!
//! Renders a scrollable conversation pane with:
//! - Color-coded messages: cyan = user, green = agent, yellow = tool calls, red = errors
//! - Streaming support: partial assistant messages rendered as they arrive
//! - Auto-scroll on new content
//! - Search within session via `/search` command

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};

use code_agent_protocol::Message;

// ---------------------------------------------------------------------------
// ChatMessage — display wrapper
// ---------------------------------------------------------------------------

/// 聊天消息
///
/// 【领域含义】带有颜色和角色元数据的渲染后聊天消息值对象。
/// 【核心职责】封装消息角色、文本内容和流式状态，供 ChatPane 渲染。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMessage {
    /// 角色 — 产生此消息的角色。
    pub role: ChatRole,
    /// 文本内容（可能是部分流式内容）。
    pub content: String,
    /// 是否正在流式传输（部分内容）。
    pub is_streaming: bool,
}

/// 聊天角色
///
/// 【领域含义】表示聊天消息角色的领域枚举，用于颜色分配和标签显示。
/// 【核心职责】区分用户、Agent、工具调用、工具结果、错误和系统六种角色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatRole {
    /// 用户
    User,
    /// Agent
    Agent,
    /// 工具调用
    ToolCall,
    /// 工具结果
    ToolResult,
    /// 错误
    Error,
    /// 系统
    System,
}

impl From<&Message> for ChatRole {
    fn from(msg: &Message) -> Self {
        match msg {
            Message::UserMessage { .. } => ChatRole::User,
            Message::AssistantMessage { .. } => ChatRole::Agent,
            Message::ToolCall(_) => ChatRole::ToolCall,
            Message::ToolResult(tr) => {
                if tr.is_error() {
                    ChatRole::Error
                } else {
                    ChatRole::ToolResult
                }
            }
        }
    }
}

impl ChatRole {
    /// 获取角色颜色
    ///
    /// 【领域含义】返回渲染此角色消息时使用的颜色。
    /// 【核心职责】供 ChatPane 渲染时设置文本颜色。
    pub fn color(self) -> Color {
        match self {
            ChatRole::User => Color::Cyan,
            ChatRole::Agent => Color::Green,
            ChatRole::ToolCall => Color::Yellow,
            ChatRole::ToolResult => Color::Gray,
            ChatRole::Error => Color::Red,
            ChatRole::System => Color::Magenta,
        }
    }

    /// 获取角色标签
    ///
    /// 【领域含义】返回消息前显示的角色标签。
    /// 【核心职责】供 ChatPane 渲染时显示角色标识。
    pub fn label(self) -> &'static str {
        match self {
            ChatRole::User => "You",
            ChatRole::Agent => "Agent",
            ChatRole::ToolCall => "Tool →",
            ChatRole::ToolResult => "Tool ←",
            ChatRole::Error => "ERROR",
            ChatRole::System => "System",
        }
    }
}

// ---------------------------------------------------------------------------
// ChatPane
// ---------------------------------------------------------------------------

/// 聊天面板
///
/// 【领域含义】可滚动的聊天面板，渲染对话消息，支持流式消息、自动滚动和全文搜索。
/// 【核心职责】管理消息列表、滚动状态和搜索功能，提供 ratatui 渲染接口。
///
/// # 颜色方案
/// | 角色       | 颜色    |
/// |-----------|---------|
/// | User      | Cyan    |
/// | Agent     | Green   |
/// | ToolCall  | Yellow  |
/// | ToolResult| Gray    |
/// | Error     | Red     |
/// | System    | Magenta |
pub struct ChatPane {
    /// All messages in the conversation (the full history, not display lines).
    messages: Vec<ChatMessage>,

    /// Current scroll offset (0 = bottom, larger = scrolled up).
    scroll_offset: u16,

    /// Whether auto-scroll is enabled. Cleared when the user scrolls up,
    /// re-enabled when they scroll to the bottom.
    auto_scroll: bool,

    /// Current search query, if any.
    search_query: Option<String>,

    /// Indices of messages matching the current search, if any.
    search_matches: Vec<usize>,

    /// Current position in the search matches list.
    search_index: usize,
}

impl Default for ChatPane {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatPane {
    /// 创建聊天面板
    ///
    /// 【领域含义】构造空的聊天面板实例。
    /// 【核心职责】初始化消息列表、滚动偏移和搜索状态。
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
            search_query: None,
            search_matches: Vec::new(),
            search_index: 0,
        }
    }

    // ── Message management ─────────────────────────────────────────────

    /// 追加消息到聊天历史
    ///
    /// 【领域含义】将新消息追加到聊天历史中。
    /// 【核心职责】如果自动滚动启用，视图将自动滚动显示新消息。
    pub fn push_message(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
    }

    /// 追加或更新最后一条流式消息
    ///
    /// 【领域含义】追加流式内容到当前正在流式传输的消息，或创建新的流式消息。
    /// 【核心职责】如果最后一条消息也是流式 Agent 消息，则追加内容；否则创建新消息。
    pub fn push_streaming(&mut self, content: &str) {
        if let Some(last) = self.messages.last_mut() {
            if last.is_streaming && last.role == ChatRole::Agent {
                last.content.push_str(content);
                return;
            }
        }
        self.push_message(ChatMessage {
            role: ChatRole::Agent,
            content: content.to_string(),
            is_streaming: true,
        });
    }

    /// 标记最后一条流式消息为完成
    ///
    /// 【领域含义】将最后一条流式消息标记为已完成。
    /// 【核心职责】设置 is_streaming = false。
    pub fn finalize_streaming(&mut self) {
        if let Some(last) = self.messages.last_mut() {
            if last.is_streaming {
                last.is_streaming = false;
            }
        }
    }

    /// 追加工具调用消息
    ///
    /// 【领域含义】将工具调用信息作为消息追加到聊天历史。
    /// 【核心职责】创建 ToolCall 角色的消息，包含工具名称和参数。
    pub fn push_tool_call(&mut self, name: &str, args: &str) {
        self.push_message(ChatMessage {
            role: ChatRole::ToolCall,
            content: format!("{} {}", name, args),
            is_streaming: false,
        });
    }

    /// 追加工具结果消息
    ///
    /// 【领域含义】将工具执行结果作为消息追加到聊天历史。
    /// 【核心职责】创建 ToolResult 或 Error 角色的消息，包含输出内容。
    pub fn push_tool_result(&mut self, output: &str, is_error: bool) {
        self.push_message(ChatMessage {
            role: if is_error { ChatRole::Error } else { ChatRole::ToolResult },
            content: output.to_string(),
            is_streaming: false,
        });
    }

    // ── Scrolling ──────────────────────────────────────────────────────

    /// 向上滚动 n 行
    ///
    /// 【领域含义】将聊天视图向上滚动指定行数，禁用自动滚动。
    /// 【核心职责】增加滚动偏移，关闭自动滚动。
    pub fn scroll_up(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
        self.auto_scroll = false;
    }

    /// 向下滚动 n 行
    ///
    /// 【领域含义】将聊天视图向下滚动指定行数，到达底部时重新启用自动滚动。
    /// 【核心职责】减少滚动偏移，到底部时恢复自动滚动。
    pub fn scroll_down(&mut self, n: u16) {
        if self.scroll_offset <= n {
            self.scroll_offset = 0;
            self.auto_scroll = true;
        } else {
            self.scroll_offset -= n;
        }
    }

    /// 跳到底部
    ///
    /// 【领域含义】将聊天视图跳转到底部，重新启用自动滚动。
    /// 【核心职责】重置滚动偏移，启用自动滚动。
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }

    // ── Search ─────────────────────────────────────────────────────────

    /// 搜索聊天历史
    ///
    /// 【领域含义】在聊天历史中搜索指定查询文本。
    /// 【核心职责】不区分大小写搜索，返回匹配数量。
    pub fn search(&mut self, query: String) -> usize {
        self.search_query = Some(query.clone());
        self.search_matches.clear();
        self.search_index = 0;

        let q_lower = query.to_lowercase();
        for (i, msg) in self.messages.iter().enumerate() {
            if msg.content.to_lowercase().contains(&q_lower) {
                self.search_matches.push(i);
            }
        }
        self.search_matches.len()
    }

    /// 跳转到下一个搜索匹配
    ///
    /// 【领域含义】移动到搜索结果中的下一个匹配项。
    /// 【核心职责】循环递增搜索索引，滚动到匹配位置。
    pub fn search_next(&mut self) {
        if !self.search_matches.is_empty() {
            self.search_index = (self.search_index + 1) % self.search_matches.len();
            self.scroll_to_match_current();
        }
    }

    /// 跳转到上一个搜索匹配
    ///
    /// 【领域含义】移动到搜索结果中的上一个匹配项。
    /// 【核心职责】循环递减搜索索引，滚动到匹配位置。
    pub fn search_prev(&mut self) {
        if !self.search_matches.is_empty() {
            self.search_index = if self.search_index == 0 {
                self.search_matches.len() - 1
            } else {
                self.search_index - 1
            };
            self.scroll_to_match_current();
        }
    }

    /// 清除搜索
    ///
    /// 【领域含义】清除当前搜索状态。
    /// 【核心职责】重置搜索查询、匹配列表和索引。
    pub fn search_clear(&mut self) {
        self.search_query = None;
        self.search_matches.clear();
        self.search_index = 0;
    }

    /// 判断自动滚动是否启用
    ///
    /// 【领域含义】返回当前是否启用自动滚动。
    /// 【核心职责】供外部查询自动滚动状态。
    pub fn is_auto_scrolling(&self) -> bool {
        self.auto_scroll
    }

    /// 判断搜索是否激活
    ///
    /// 【领域含义】返回当前是否处于搜索状态。
    /// 【核心职责】供外部查询搜索状态。
    pub fn is_searching(&self) -> bool {
        self.search_query.is_some()
    }

    /// Scroll to make the current search match visible.
    fn scroll_to_match_current(&mut self) {
        if let Some(&msg_idx) = self.search_matches.get(self.search_index) {
            self.scroll_offset = msg_idx.saturating_sub(2) as u16;
            self.auto_scroll = false;
        }
    }

    // ── Message count ──────────────────────────────────────────────────

    /// 获取消息数量
    ///
    /// 【领域含义】返回聊天历史中的消息总数。
    /// 【核心职责】供外部查询消息数量。
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// 获取所有消息的文本内容
    ///
    /// 【领域含义】将所有消息内容连接为字符串（用于快照测试）。
    /// 【核心职责】供测试验证消息内容。
    pub fn all_text(&self) -> String {
        self.messages
            .iter()
            .map(|m| format!("[{}] {}", m.role.label(), m.content))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

impl ChatPane {
    /// Render the chat pane into the given frame area.
    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        // Build ratatui lines from messages, respecting scroll offset
        let lines = self.build_lines(area.width as usize);

        let paragraph = Paragraph::new(lines)
            .block(
                Block::bordered()
                    .title(" Chat ")
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .scroll((self.scroll_offset, 0));

        f.render_widget(paragraph, area);
    }

    /// Build styled `Line`s from the message list for the given display width.
    fn build_lines(&self, width: usize) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();

        for (idx, msg) in self.messages.iter().enumerate() {
            let role_color = msg.role.color();

            // Check if this message is a search match
            let is_match = self.search_query.is_some()
                && self.search_matches.contains(&idx);

            // Role label line
            let mut label_style = Style::default()
                .fg(role_color)
                .add_modifier(Modifier::BOLD);

            if is_match {
                label_style = label_style.bg(Color::Rgb(80, 80, 0));
            }

            if msg.is_streaming {
                label_style = label_style.add_modifier(Modifier::ITALIC);
            }

            lines.push(Line::from(vec![
                Span::styled(msg.role.label(), label_style),
            ]));

            // Wrap content to terminal width
            let wrapped = textwrap::wrap(&msg.content, width.saturating_sub(4));

            for chunk in wrapped {
                let content_style = if is_match {
                    Style::default().fg(role_color).bg(Color::Rgb(80, 80, 0))
                } else {
                    Style::default().fg(role_color)
                };

                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(chunk.to_string(), content_style),
                ]));
            }
        }

        // Search indicator bar at bottom if searching
        if let Some(ref query) = self.search_query {
            let total = self.search_matches.len();
            let current = if total > 0 {
                self.search_index + 1
            } else {
                0
            };

            let bar = format!(
                " Search: \"{}\"  {}/{}  (n: next, p: prev, Esc: clear)",
                query, current, total
            );
            lines.push(Line::from(Span::styled(
                bar,
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Rgb(200, 200, 0)),
            )));
        }

        lines
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── ChatRole tests ───────────────────────────────────────────────────

    #[test]
    fn role_from_user_message() {
        let msg = Message::UserMessage {
            content: "hello".into(),
        };
        assert_eq!(ChatRole::from(&msg), ChatRole::User);
    }

    #[test]
    fn role_from_assistant_message() {
        let msg = Message::AssistantMessage {
            content: "hi".into(),
        };
        assert_eq!(ChatRole::from(&msg), ChatRole::Agent);
    }

    #[test]
    fn role_from_tool_call_message() {
        use code_agent_protocol::ToolCall;
        let msg = Message::ToolCall(ToolCall {
            id: "tc1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({}),
        });
        assert_eq!(ChatRole::from(&msg), ChatRole::ToolCall);
    }

    #[test]
    fn role_from_tool_result_success() {
        use code_agent_protocol::ToolResultMessage;
        let msg = Message::ToolResult(ToolResultMessage {
            tool_call_id: "tc1".into(),
            output: Some("ok".into()),
            error: None,
        });
        assert_eq!(ChatRole::from(&msg), ChatRole::ToolResult);
    }

    #[test]
    fn role_from_tool_result_error() {
        use code_agent_protocol::ToolResultMessage;
        let msg = Message::ToolResult(ToolResultMessage {
            tool_call_id: "tc1".into(),
            output: None,
            error: Some("fail".into()),
        });
        assert_eq!(ChatRole::from(&msg), ChatRole::Error);
    }

    #[test]
    fn role_colors_match_spec() {
        assert_eq!(ChatRole::User.color(), Color::Cyan);
        assert_eq!(ChatRole::Agent.color(), Color::Green);
        assert_eq!(ChatRole::ToolCall.color(), Color::Yellow);
        assert_eq!(ChatRole::Error.color(), Color::Red);
    }

    #[test]
    fn role_labels() {
        assert_eq!(ChatRole::User.label(), "You");
        assert_eq!(ChatRole::Agent.label(), "Agent");
        assert_eq!(ChatRole::ToolCall.label(), "Tool →");
        assert_eq!(ChatRole::Error.label(), "ERROR");
    }

    // ── ChatPane: push_message ───────────────────────────────────────────

    #[test]
    fn chat_pane_starts_empty() {
        let pane = ChatPane::new();
        assert_eq!(pane.message_count(), 0);
        assert!(pane.auto_scroll);
    }

    #[test]
    fn push_message_adds_to_history() {
        let mut pane = ChatPane::new();
        pane.push_message(ChatMessage {
            role: ChatRole::User,
            content: "hello".into(),
            is_streaming: false,
        });
        assert_eq!(pane.message_count(), 1);
    }

    #[test]
    fn push_tool_call_formats_correctly() {
        let mut pane = ChatPane::new();
        pane.push_tool_call("read_file", r#"{"path":"src/main.rs"}"#);
        assert_eq!(pane.message_count(), 1);
        let all = pane.all_text();
        assert!(all.contains("read_file"));
        assert!(all.contains("src/main.rs"));
    }

    // ── ChatPane: streaming ──────────────────────────────────────────────

    #[test]
    fn streaming_appends_to_last() {
        let mut pane = ChatPane::new();
        pane.push_streaming("I think");
        pane.push_streaming(" therefore");
        pane.push_streaming(" I am.");
        assert_eq!(pane.message_count(), 1);
        let all = pane.all_text();
        assert!(all.contains("I think therefore I am."));
        assert!(pane.messages[0].is_streaming);
    }

    #[test]
    fn finalize_streaming_stops_flag() {
        let mut pane = ChatPane::new();
        pane.push_streaming("hello");
        pane.finalize_streaming();
        assert!(!pane.messages[0].is_streaming);
    }

    #[test]
    fn streaming_new_message_when_last_is_complete() {
        let mut pane = ChatPane::new();
        pane.push_message(ChatMessage {
            role: ChatRole::User,
            content: "hi".into(),
            is_streaming: false,
        });
        pane.push_streaming("response");
        // Should have 2 messages now since last was not streaming
        assert_eq!(pane.message_count(), 2);
        assert_eq!(pane.messages[1].role, ChatRole::Agent);
    }

    // ── ChatPane: scroll ─────────────────────────────────────────────────

    #[test]
    fn scroll_up_disables_auto_scroll() {
        let mut pane = ChatPane::new();
        assert!(pane.auto_scroll);
        pane.scroll_up(5);
        assert!(!pane.auto_scroll);
        assert_eq!(pane.scroll_offset, 5);
    }

    #[test]
    fn scroll_to_bottom_enables_auto_scroll() {
        let mut pane = ChatPane::new();
        pane.scroll_up(10);
        pane.scroll_to_bottom();
        assert_eq!(pane.scroll_offset, 0);
        assert!(pane.auto_scroll);
    }

    #[test]
    fn scroll_down_to_bottom_restores_auto_scroll() {
        let mut pane = ChatPane::new();
        pane.scroll_up(3);
        pane.scroll_down(5); // scroll down by more than offset → hit bottom
        assert_eq!(pane.scroll_offset, 0);
        assert!(pane.auto_scroll);
    }

    #[test]
    fn scroll_down_partial() {
        let mut pane = ChatPane::new();
        pane.scroll_up(10);
        pane.scroll_down(3);
        assert_eq!(pane.scroll_offset, 7);
        assert!(!pane.auto_scroll);
    }

    // ── ChatPane: search ─────────────────────────────────────────────────

    #[test]
    fn search_finds_matches() {
        let mut pane = ChatPane::new();
        pane.push_message(ChatMessage {
            role: ChatRole::User,
            content: "Find the bug".into(),
            is_streaming: false,
        });
        pane.push_message(ChatMessage {
            role: ChatRole::Agent,
            content: "Bug is at line 42".into(),
            is_streaming: false,
        });

        let count = pane.search("bug".into());
        assert_eq!(count, 2);
    }

    #[test]
    fn search_is_case_insensitive() {
        let mut pane = ChatPane::new();
        pane.push_message(ChatMessage {
            role: ChatRole::User,
            content: "HELLO".into(),
            is_streaming: false,
        });

        let count = pane.search("hello".into());
        assert_eq!(count, 1);
    }

    #[test]
    fn search_no_matches() {
        let mut pane = ChatPane::new();
        pane.push_message(ChatMessage {
            role: ChatRole::User,
            content: "hello".into(),
            is_streaming: false,
        });

        let count = pane.search("xyzzy".into());
        assert_eq!(count, 0);
    }

    #[test]
    fn search_next_and_prev_cycle() {
        let mut pane = ChatPane::new();
        for i in 0..5 {
            pane.push_message(ChatMessage {
                role: ChatRole::User,
                content: format!("msg {}", i),
                is_streaming: false,
            });
        }

        pane.search("msg".into());
        // After search, index is 0, pointing to msg[0]
        assert_eq!(pane.search_index, 0);

        pane.search_next();
        assert_eq!(pane.search_index, 1);
        pane.search_next();
        assert_eq!(pane.search_index, 2);
        pane.search_prev();
        assert_eq!(pane.search_index, 1);
    }

    #[test]
    fn search_clear_resets() {
        let mut pane = ChatPane::new();
        pane.push_message(ChatMessage {
            role: ChatRole::User,
            content: "find me".into(),
            is_streaming: false,
        });
        pane.search("find".into());
        pane.search_clear();

        assert!(pane.search_query.is_none());
        assert!(pane.search_matches.is_empty());
    }

    // ── ChatPane: all_text ───────────────────────────────────────────────

    #[test]
    fn all_text_formats_messages() {
        let mut pane = ChatPane::new();
        pane.push_message(ChatMessage {
            role: ChatRole::User,
            content: "hello".into(),
            is_streaming: false,
        });
        pane.push_message(ChatMessage {
            role: ChatRole::Agent,
            content: "world".into(),
            is_streaming: false,
        });

        let all = pane.all_text();
        assert!(all.contains("[You] hello"));
        assert!(all.contains("[Agent] world"));
    }
}
