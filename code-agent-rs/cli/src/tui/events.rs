//! Event handling for the TUI.
//!
//! Defines [`AppEvent`] — the unified event type consumed by the main loop —
//! and the [`EventHandler`] that produces events from crossterm input,
//! window resizes, and periodic ticks.

use std::time::Duration;

use crossterm::event::{self, Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use tokio::sync::mpsc;

use code_agent_core::agent::plan::PlanProgressEvent;
use code_agent_protocol::ResponseEvent;

// ---------------------------------------------------------------------------
// AppEvent — unified event type
// ---------------------------------------------------------------------------

/// TUI 事件
///
/// 【领域含义】TUI 主事件循环消费的统一事件类型，由 EventHandler 从 crossterm 输入、定时器和 Agent 流式响应通道产生。
/// 【核心职责】统一封装键盘事件、窗口调整、定时器、Agent 事件和错误。
#[derive(Clone, Debug)]
pub enum AppEvent {
    /// 键盘事件 — 按键被按下。Ctrl+C 为中断信号。
    Key(KeyEvent),

    /// 窗口调整 — 终端大小变化，包含新 (列, 行)。
    Resize(u16, u16),

    /// 定时器 — 周期性状态栏更新。
    Tick,

    /// Agent 事件 — 来自 Agent 引擎的流式响应、工具调用等。
    Agent(ResponseEvent),

    /// Agent 流结束 — Agent 响应流已结束。
    AgentStreamEnded,

    /// Agent 错误 — Agent 引擎发出的错误。
    AgentError(String),

    // ── Plan engine events ─────────────────────────────────────────

    /// 计划已分解完成 — 待用户审批。
    PlanDecomposed {
        /// 计划名称
        plan_name: String,
        /// 阶段数量
        phase_count: usize,
        /// 总节点数
        node_count: usize,
        /// 阶段列表（按顺序）
        phases: Vec<String>,
    },

    /// 计划执行进度事件 — 节点开始/完成/失败/取消等。
    PlanProgress(PlanProgressEvent),

    /// 需要用户审批 — 计划已完成分解，等待用户确认是否执行。
    PlanApprovalRequired {
        /// 计划名称
        plan_name: String,
        /// 阶段数量
        phase_count: usize,
        /// 总节点数
        node_count: usize,
    },

    /// 计划执行完成 — 含执行结果摘要。
    PlanCompleted {
        /// 是否全部成功
        success: bool,
        /// 人工可读的摘要
        summary: String,
    },

    /// 计划相关错误（分解失败、执行异常等）。
    PlanError(String),
}

// ---------------------------------------------------------------------------
// EventHandler
// ---------------------------------------------------------------------------

/// 事件处理器
///
/// 【领域含义】轮询 crossterm 键盘和窗口调整事件，并发出周期性定时器事件。
/// 【核心职责】在后台任务中运行，将 crossterm 事件转换为 AppEvent 发送到主循环。
///
/// # 使用示例
///
/// ```rust,ignore
/// let (tx, rx) = mpsc::unbounded_channel();
/// let handler = EventHandler::new(tx, Duration::from_millis(250));
/// handler.spawn(); // runs in background
/// ```
pub struct EventHandler {
    /// 事件发送端 — 发送到主循环消费的通道。
    tx: mpsc::UnboundedSender<AppEvent>,

    /// 定时器间隔 — 周期性 Tick 事件的间隔。
    tick_rate: Duration,
}

impl EventHandler {
    /// 创建事件处理器
    ///
    /// 【领域含义】构造 EventHandler 实例。
    /// 【核心职责】指定事件发送通道和定时器间隔。
    pub fn new(tx: mpsc::UnboundedSender<AppEvent>, tick_rate: Duration) -> Self {
        Self { tx, tick_rate }
    }

    /// 启动事件循环
    ///
    /// 【领域含义】在 tokio 后台任务中启动事件循环。
    /// 【核心职责】在定时器间隔之间轮询 crossterm 事件，循环运行直到通道关闭。
    pub fn spawn(self) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(self.tick_rate);

            loop {
                ticker.tick().await;

                // Emit tick event
                if self.tx.send(AppEvent::Tick).is_err() {
                    break; // receiver dropped
                }

                // Drain all pending crossterm events
                while event::poll(Duration::ZERO).unwrap_or(false) {
                    let ev = match event::read() {
                        Ok(CrosstermEvent::Key(key)) => AppEvent::Key(key),
                        Ok(CrosstermEvent::Resize(cols, rows)) => AppEvent::Resize(cols, rows),
                        Ok(_) => continue, // ignore mouse, focus, paste
                        Err(_) => break,   // stop on I/O error
                    };
                    if self.tx.send(ev).is_err() {
                        return; // receiver dropped
                    }
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Agent event bridge
// ---------------------------------------------------------------------------

/// Agent 事件桥接器
///
/// 【领域含义】连接 Agent 流式 ResponseEvent 通道和 TUI 事件系统的桥接器。
/// 【核心职责】从 Agent 流读取事件，转发为 AppEvent::Agent 到主循环，流结束时发出 AppEvent::AgentStreamEnded。
pub struct AgentEventBridge {
    tx: mpsc::UnboundedSender<AppEvent>,
}

impl AgentEventBridge {
    /// 创建桥接器
    ///
    /// 【领域含义】构造 Agent 事件桥接器，发送到指定通道。
    /// 【核心职责】保存事件发送端。
    pub fn new(tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        Self { tx }
    }

    /// 启动桥接任务
    ///
    /// 【领域含义】启动后台任务，从 Agent 响应流读取事件并转发到主循环。
    /// 【核心职责】流结束时发出 AgentStreamEnded，流错误时发出 AgentError。
    pub fn spawn(
        &self,
        stream: impl futures::Stream<Item = Result<ResponseEvent, String>> + Send + Unpin + 'static,
    ) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let mut stream = Box::pin(stream);
            while let Some(result) = stream.next().await {
                match result {
                    Ok(event) => {
                        if tx.send(AppEvent::Agent(event)).is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        if tx.send(AppEvent::AgentError(err)).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = tx.send(AppEvent::AgentStreamEnded);
        });
    }
}

// ---------------------------------------------------------------------------
// Key helpers
// ---------------------------------------------------------------------------

/// 判断是否为 Ctrl+C 中断信号
///
/// 【领域含义】判断按键事件是否为标准中断信号 Ctrl+C。
/// 【核心职责】供 TUI 主循环识别中断操作。
pub fn is_interrupt(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// 判断是否为 Ctrl+D 退出信号
///
/// 【领域含义】判断按键事件是否为 Ctrl+D（某些终端中用于退出）。
/// 【核心职责】供 TUI 主循环识别退出操作。
pub fn is_eof(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// 判断是否为 Enter 键
///
/// 【领域含义】判断按键事件是否为 Enter（无修饰键）。
/// 【核心职责】供 TUI 主循环识别提交操作。
pub fn is_enter(key: &KeyEvent) -> bool {
    key.code == KeyCode::Enter && key.modifiers.is_empty()
}

/// 判断是否为 Escape 键
///
/// 【领域含义】判断按键事件是否为 Escape。
/// 【核心职责】供 TUI 主循环识别取消操作。
pub fn is_escape(key: &KeyEvent) -> bool {
    key.code == KeyCode::Esc
}

/// 判断是否为向上导航键
///
/// 【领域含义】判断按键事件是否为向上导航（上箭头或 Ctrl+P）。
/// 【核心职责】供 TUI 主循环识别向上导航操作。
pub fn is_up(key: &KeyEvent) -> bool {
    key.code == KeyCode::Up
        || (key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL))
}

/// 判断是否为向下导航键
///
/// 【领域含义】判断按键事件是否为向下导航（下箭头或 Ctrl+N）。
/// 【核心职责】供 TUI 主循环识别向下导航操作。
pub fn is_down(key: &KeyEvent) -> bool {
    key.code == KeyCode::Down
        || (key.code == KeyCode::Char('n') && key.modifiers.contains(KeyModifiers::CONTROL))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_interrupt_ctrl_c() {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(is_interrupt(&key));
    }

    #[test]
    fn test_is_interrupt_plain_c() {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(!is_interrupt(&key));
    }

    #[test]
    fn test_is_eof_ctrl_d() {
        let key = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(is_eof(&key));
    }

    #[test]
    fn test_is_enter() {
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(is_enter(&key));
    }

    #[test]
    fn test_is_enter_with_alt() {
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        assert!(!is_enter(&key));
    }

    #[test]
    fn test_is_escape() {
        let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert!(is_escape(&key));
    }

    #[test]
    fn test_is_up_arrow() {
        let key = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert!(is_up(&key));
    }

    #[test]
    fn test_is_up_ctrl_p() {
        let key = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert!(is_up(&key));
    }

    #[test]
    fn test_is_down_arrow() {
        let key = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        assert!(is_down(&key));
    }

    #[test]
    fn test_is_down_ctrl_n() {
        let key = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert!(is_down(&key));
    }

    #[test]
    fn test_app_event_debug() {
        let ev = AppEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let dbg = format!("{:?}", ev);
        assert!(dbg.contains("Key"));
    }
}
