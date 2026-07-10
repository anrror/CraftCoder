//! Interactive Terminal UI (TUI) for the AI Coding Agent.
//!
//! This module implements a full-featured terminal interface using ratatui and
//! crossterm. It provides:
//!
//! - **[`App`]**: the main TUI application struct with the event loop
//! - **[`chat::ChatPane`]**: scrollable conversation pane with colored messages
//! - **[`input::InputBar`]**: multi-line input with history and command palette
//! - **[`status::StatusBar`]**: session info, connection status, mode indicator
//! - **[`events`]**: crossterm event handling and agent stream bridging
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────┐
//! │ App (main loop)                      │
//! │  ├─ ChatPane (messages)              │
//! │  ├─ InputBar (user input)            │
//! │  ├─ StatusBar (session info)         │
//! │  ├─ EventHandler (crossterm → events)│
//! │  └─ AgentEventBridge (agent stream)  │
//! └──────────────────────────────────────┘
//! ```
//!
//! # Layout
//!
//! ```text
//! ┌─────────────────────────────────┐
//! │ Chat                            │
//! │                                 │
//! │ ...messages...                  │
//! │                                 │
//! ├─────────────────────────────────┤
//! │ Input                           │
//! ├─────────────────────────────────┤
//! │ Status                          │
//! └─────────────────────────────────┘
//! ```

pub mod chat;
pub mod events;
pub mod feedback;
pub mod input;
pub mod status;

use std::io::{self, stdout, Stdout};
use std::time::Duration;

use crossterm::{
    cursor,
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{layout::{Constraint, Direction, Layout}, backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;

use crate::tui::chat::ChatPane;
use crate::tui::events::{is_interrupt, AppEvent, EventHandler};
use crate::tui::feedback::FeedbackPrompt;
use crate::tui::input::{InputAction, InputBar};
use crate::tui::status::StatusBar;

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

/// TUI 应用主结构
///
/// 【领域含义】终端 UI 应用的主聚合，拥有终端、所有 UI 组件和事件循环。
/// 【核心职责】管理 ChatPane、InputBar、StatusBar 等 UI 组件，通过事件通道与 Agent 引擎通信。
///
/// # 使用示例
///
/// ```rust,ignore
/// let mut app = App::new(agent_rx)?;
/// app.run().await?;
/// ```
pub struct App {
    /// Terminal backend for ratatui rendering.
    terminal: Terminal<CrosstermBackend<Stdout>>,

    /// Chat message pane.
    chat: ChatPane,

    /// Input bar with history.
    input: InputBar,

    /// Status bar at the bottom.
    status: StatusBar,

    /// Receiver for unified TUI events (crossterm + timer ticks).
    event_rx: mpsc::UnboundedReceiver<AppEvent>,

    /// Sender for events (kept so background tasks don't panic).
    _event_tx: mpsc::UnboundedSender<AppEvent>,

    /// Receiver for agent engine response events.
    agent_rx: mpsc::UnboundedReceiver<AppEvent>,

    /// Feedback prompt shown after each turn.
    feedback: FeedbackPrompt,

    /// Whether the TUI is running.
    running: bool,

    /// Whether the TUI should quit on next tick.
    should_quit: bool,
}

impl App {
    /// 创建 TUI 应用
    ///
    /// 【领域含义】构造 TUI 应用实例，初始化终端和所有 UI 组件。
    /// 【核心职责】启用原始模式、进入备选屏幕、创建事件通道。
    ///
    /// # 参数
    /// * `agent_rx` — Agent ResponseEvent 的接收端，TUI 从此通道读取流式响应和工具调用。
    ///
    /// # 错误
    /// 如果终端设置失败（如不在终端中运行或无法进入备选屏幕）则返回 I/O 错误。
    pub fn new(agent_rx: mpsc::UnboundedReceiver<AppEvent>) -> io::Result<Self> {
        // Set up terminal
        terminal::enable_raw_mode()?;
        let mut stdout = stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        // Create event channel
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        Ok(Self {
            terminal,
            chat: ChatPane::new(),
            input: InputBar::new(),
            status: StatusBar::new(),
            feedback: FeedbackPrompt::new(),
            event_rx,
            _event_tx: event_tx,
            agent_rx,
            running: false,
            should_quit: false,
        })
    }

    /// 运行主事件循环
    ///
    /// 【领域含义】启动 TUI 主事件循环，阻塞直到用户退出（空输入时 Ctrl+C 或 Ctrl+D）。
    /// 【核心职责】循环执行：绘制帧 → 接收事件（键盘/Agent/定时器/窗口调整）→ 分发处理。
    ///
    /// # 事件循环
    /// ```text
    /// loop {
    ///     draw();
    ///     event = recv(crossterm_events | agent_events | timer_ticks);
    ///     match event {
    ///         Key => handle keyboard input,
    ///         Agent => update chat + status,
    ///         Tick => redraw status,
    ///         Resize => reflow layout,
    ///     }
    /// }
    /// ```
    pub async fn run(&mut self) -> io::Result<()> {
        // Start the crossterm event handler
        let handler_tx = self._event_tx.clone();
        let handler = EventHandler::new(handler_tx, Duration::from_millis(250));
        handler.spawn();

        self.running = true;

        while self.running {
            // Draw the current frame
            self.terminal.draw(|f| {
                let feedback_height: u16 = if self.feedback.is_active() { 6 } else { 0 };

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Min(3),              // chat (fills most space)
                        Constraint::Length(feedback_height), // feedback prompt
                        Constraint::Length(3),            // input bar
                        Constraint::Length(1),            // status bar
                    ])
                    .split(f.area());

                self.chat.render(f, chunks[0]);
                self.feedback.render(f, chunks[1]);
                self.input.render(f, chunks[2]);
                self.status.render(f, chunks[3]);
            })?;

            // Wait for an event from any source
            let event = tokio::select! {
                Some(ev) = self.event_rx.recv() => ev,
                Some(agent_ev) = self.agent_rx.recv() => {
                    self.handle_agent_event(agent_ev);
                    continue;
                }
                else => break,
            };

            match event {
                AppEvent::Key(key) => self.handle_key_event(key),
                AppEvent::Resize(_, _) => {
                    // Layout is recalculated on next draw automatically
                }
                AppEvent::Tick => {
                    // Periodic tick — no action needed beyond redraw
                }
                AppEvent::Agent(..) | AppEvent::AgentStreamEnded | AppEvent::AgentError(..) => {
                    self.handle_agent_event(event);
                }
            }

            if self.should_quit {
                self.running = false;
            }
        }

        Ok(())
    }

    /// 恢复终端原始状态
    ///
    /// 【领域含义】退出前将终端恢复到原始状态。
    /// 【核心职责】禁用原始模式、离开备选屏幕、禁用鼠标捕获、显示光标。
    /// Drop 实现中也会自动执行，但显式清理更安全。
    pub fn cleanup(&mut self) -> io::Result<()> {
        terminal::disable_raw_mode()?;
        execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            cursor::Show,
        )?;
        self.terminal.show_cursor()?;
        Ok(())
    }

    // ── Event handlers ──────────────────────────────────────────────────

    /// Handle a crossterm key event.
    fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};

        // If feedback prompt is active, route keys to it first
        if self.feedback.is_active() && self.feedback.handle_key(key) {
            // Check if feedback was submitted
            if self.feedback.state == feedback::FeedbackPromptState::Submitted {
                if let Some(event) = self.feedback.build_event() {
                    // Event is ready to be stored by the caller
                    // (the caller has access to the FeedbackCollector)
                    self.chat.push_message(chat::ChatMessage {
                        role: chat::ChatRole::System,
                        content: format!(
                            "Feedback submitted: {}",
                            if event.rating == code_agent_core::feedback::Rating::ThumbsUp {
                                "👍 Thumbs Up"
                            } else {
                                "👎 Thumbs Down"
                            }
                        ),
                        is_streaming: false,
                    });
                }
                self.feedback.hide();
            } else if self.feedback.state == feedback::FeedbackPromptState::Skipped {
                self.chat.push_message(chat::ChatMessage {
                    role: chat::ChatRole::System,
                    content: "Feedback skipped".into(),
                    is_streaming: false,
                });
                self.feedback.hide();
            }
            return;
        }

        // Search mode: Esc clears search
        if self.chat.is_searching() && key.code == KeyCode::Esc {
            self.chat.search_clear();
            return;
        }

        // Global: Ctrl+C interrupt
        if is_interrupt(&key) {
            let action = self.input.handle_interrupt();
            match action {
                InputAction::Interrupt => {
                    self.chat.push_message(chat::ChatMessage {
                        role: chat::ChatRole::System,
                        content: "Interrupt signal sent".into(),
                        is_streaming: false,
                    });
                }
                InputAction::Clear => { /* input cleared, continue */ }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Enter => {
                let action = self.input.submit();
                match action {
                    InputAction::Submit(text) => {
                        self.chat.push_message(chat::ChatMessage {
                            role: chat::ChatRole::User,
                            content: text.clone(),
                            is_streaming: false,
                        });
                        // The text is sent to the agent engine via the channel
                        // (handled by the caller in a full integration)
                        self.chat.scroll_to_bottom();
                    }
                    InputAction::None => {}
                    _ => {}
                }
            }

            KeyCode::Up => {
                if self.input.is_empty() || self.input.history_len() > 0 {
                    self.input.history_prev();
                } else {
                    self.chat.scroll_up(3);
                }
            }

            KeyCode::Down => {
                if self.input.is_empty() || self.input.history_len() > 0 {
                    self.input.history_next();
                } else {
                    self.chat.scroll_down(3);
                }
            }

            KeyCode::PageUp => {
                self.chat.scroll_up(15);
            }

            KeyCode::PageDown => {
                self.chat.scroll_down(15);
            }

            KeyCode::Home => {
                self.input.cursor_home();
            }

            KeyCode::End => {
                self.input.cursor_end();
            }

            KeyCode::Left => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.input.delete_word_backward();
                } else {
                    self.input.cursor_left();
                }
            }

            KeyCode::Right => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.input.delete_word_forward();
                } else {
                    self.input.cursor_right();
                }
            }

            KeyCode::Backspace => {
                self.input.delete_backward();
            }

            KeyCode::Delete => {
                self.input.delete_forward();
            }

            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+L: clear screen (traditionally)
                self.terminal.clear().ok();
            }

            KeyCode::Char(ch) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    // Other Ctrl+char combinations ignored
                    return;
                }
                self.input.insert_char(ch);
            }

            _ => {}
        }
    }

    /// Handle an agent response event (streaming, tool call, etc.).
    fn handle_agent_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Agent(agent_ev) => {
                match agent_ev {
                    code_agent_protocol::ResponseEvent::AgentMessageDelta { content } => {
                        self.chat.push_streaming(&content);
                    }
                    code_agent_protocol::ResponseEvent::ToolCallBegin { tool_call } => {
                        let args = serde_json::to_string(&tool_call.arguments)
                            .unwrap_or_default();
                        self.chat.push_tool_call(&tool_call.name, &args);
                    }
                    code_agent_protocol::ResponseEvent::ToolCallEnd { result, .. } => {
                        let is_error = result.is_error();
                        let output = result
                            .output
                            .unwrap_or_default();
                        self.chat.push_tool_result(&output, is_error);
                    }
                    code_agent_protocol::ResponseEvent::TurnComplete { ref turn_id, .. } => {
                        self.chat.finalize_streaming();
                        self.status.increment_turn();
                        // Show feedback prompt after each turn
                        self.feedback.show(
                            &self.status.session_id,
                            &turn_id.0,
                            serde_json::json!({
                                "turn_count": self.status.turn_count,
                                "prompt_tokens": self.status.prompt_tokens,
                                "completion_tokens": self.status.completion_tokens,
                            }),
                        );
                    }
                    code_agent_protocol::ResponseEvent::TokenUsage {
                        prompt_tokens,
                        completion_tokens,
                    } => {
                        self.status.update_tokens(prompt_tokens, completion_tokens);
                    }
                    code_agent_protocol::ResponseEvent::TurnStarted { .. } => {
                        // Turn started — nothing to do until we get content
                    }
                    code_agent_protocol::ResponseEvent::Error { message } => {
                        self.chat.push_message(chat::ChatMessage {
                            role: chat::ChatRole::Error,
                            content: message,
                            is_streaming: false,
                        });
                    }
                }
            }
            AppEvent::AgentStreamEnded => {
                self.chat.finalize_streaming();
            }
            AppEvent::AgentError(err) => {
                self.chat.push_message(chat::ChatMessage {
                    role: chat::ChatRole::Error,
                    content: err,
                    is_streaming: false,
                });
            }
            _ => {}
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // Best-effort terminal cleanup
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            cursor::Show,
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::status::{ConnectionStatus, InputMode};

    // These tests validate the component structures; full rendering tests
    // use insta snapshots (see tests/ directory).

    #[test]
    fn chat_pane_has_expected_defaults() {
        let chat = ChatPane::new();
        assert_eq!(chat.message_count(), 0);
        assert!(chat.is_auto_scrolling());
    }

    #[test]
    fn input_bar_has_expected_defaults() {
        let input = InputBar::new();
        assert!(input.is_empty());
        assert_eq!(input.history_len(), 0);
    }

    #[test]
    fn status_bar_has_expected_defaults() {
        let status = StatusBar::new();
        assert_eq!(status.connection, ConnectionStatus::Disconnected);
        assert_eq!(status.mode, InputMode::Insert);
    }
}
