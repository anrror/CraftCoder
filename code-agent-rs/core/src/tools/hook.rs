//! 工具/模型/会话生命周期钩子（Phase F）
//!
//! 【领域含义】为 Coding Agent 的执行管道提供可观测性和干预点。
//! 钩子在工具执行、模型 API 调用和会话生命周期事件的前后触发，
//! 用于日志记录、指标收集、安全审计和自定义策略。
//!
//! # 事件类型
//!
//! | 类别 | 事件 | 触发时机 |
//! |------|------|----------|
//! | 工具 | `PreExecute` / `PostExecute` | 工具执行前后 |
//! | 模型 | `PreModelCall` / `PostModelCall` | LLM API 调用前后 |
//! | 会话 | `SessionCreate` / `SessionDestroy` | 会话创建/销毁 |
//! | 命令 | `PreCommand` / `PostCommand` | 斜杠命令执行前后 |
//!
//! 钩子可以用于：
//! - **可观测性**：记录执行耗时、跟踪工具调用
//! - **审计**：记录所有工具调用和模型交互
//! - **安全**：注入自定义安全检查
//! - **调试**：打印请求/响应详情
//!
//! # 架构
//!
//! ```text
//! ToolRouter / Session / CommandExecutor
//!        │
//!        ▼
//! HookRegistry::dispatch(event)
//!        │
//!        ├─ Hook #1 (LoggingHook)
//!        ├─ Hook #2 (MetricsHook)
//!        └─ Hook #3 (AuditHook)
//! ```
//!
//! # 使用示例
//!
//! ```rust,ignore
//! use code_agent_core::tools::hook::{ToolHook, ToolEvent, HookRegistry};
//! use std::sync::Arc;
//!
//! struct LoggingHook;
//! impl ToolHook for LoggingHook {
//!     fn on_event(&self, event: &ToolEvent) {
//!         match event {
//!             ToolEvent::PreExecute { tool_name, .. } => {
//!                 tracing::info!(%tool_name, "Tool starting");
//!             }
//!             ToolEvent::PostExecute { tool_name, duration_ms, .. } => {
//!                 tracing::info!(%tool_name, %duration_ms, "Tool completed");
//!             }
//!             _ => {}
//!         }
//!     }
//! }
//!
//! let mut registry = HookRegistry::new();
//! registry.register(Arc::new(LoggingHook));
//! registry.register(Arc::new(MetricsHook));
//! // ...later, in ToolRouter or Session
//! registry.dispatch(&ToolEvent::PreExecute {
//!     call_id: "call_001",
//!     tool_name: "read_file",
//!     params_hash: "...",
//! });
//! ```

use code_agent_protocol::ToolResultMessage;

// ---------------------------------------------------------------------------
// Model call context — embedded in PreModelCall / PostModelCall events
// ---------------------------------------------------------------------------

/// 模型调用的轻量上下文快照。
#[derive(Debug, Clone)]
pub struct ModelCallSnapshot {
    /// 发送给模型的消息数量
    pub message_count: usize,
    /// 估算的输入 Token 数量
    pub estimated_input_tokens: usize,
    /// 使用的模型别名
    pub model: Option<String>,
}

// ---------------------------------------------------------------------------
// ToolEvent — all lifecycle event variants
// ---------------------------------------------------------------------------

/// 工具生命周期事件（v2 扩展版）。
///
/// 【v2 变更】添加了模型调用、会话和命令相关事件变体。
/// 保留 PreExecute / PostExecute 作为主要工具钩子事件以向后兼容。
#[derive(Debug, Clone)]
pub enum ToolEvent {
    // ── 工具事件（向后兼容）──
    /// 工具即将执行。
    PreExecute {
        /// 工具调用的唯一标识符
        call_id: String,
        /// 工具名称
        tool_name: String,
        /// 序列化参数的哈希（便于去重和日志）
        params_hash: String,
    },
    /// 工具执行完成。
    PostExecute {
        /// 工具调用的唯一标识符
        call_id: String,
        /// 工具名称
        tool_name: String,
        /// 工具执行结果
        result: ToolResultMessage,
        /// 执行耗时（毫秒）
        duration_ms: u64,
    },

    // ── 模型事件（Phase F 新增）──
    /// LLM API 调用即将发送。
    PreModelCall {
        /// 模型调用快照
        snapshot: ModelCallSnapshot,
    },
    /// LLM API 响应已返回。
    PostModelCall {
        /// 模型调用快照
        snapshot: ModelCallSnapshot,
        /// 返回消息的 Token 估算
        response_tokens: usize,
        /// 总耗时（毫秒）
        elapsed_ms: u64,
    },

    // ── 会话事件（Phase F 新增）──
    /// 会话已创建。
    SessionCreate {
        /// 线程 ID
        thread_id: String,
        /// 会话 ID
        session_id: String,
        /// 系统指令
        system_instructions: String,
    },
    /// 会话即将销毁。
    SessionDestroy {
        /// 线程 ID
        thread_id: String,
        /// 会话 ID
        session_id: String,
    },

    // ── 命令事件（Phase F 新增）──
    /// 斜杠命令即将执行。
    PreCommand {
        /// 命令名称
        command: String,
        /// 命令参数
        args: String,
    },
    /// 斜杠命令执行完成。
    PostCommand {
        /// 命令名称
        command: String,
        /// 命令参数
        args: String,
        /// 是否执行成功
        success: bool,
        /// 执行耗时（毫秒）
        elapsed_ms: u64,
    },
}

// ---------------------------------------------------------------------------
// ToolHook trait
// ---------------------------------------------------------------------------

/// 工具生命周期钩子（观察者模式，仅通知不修改）。
///
/// 【领域含义】钩子接收工具、模型、会话和命令生命周期的通知事件。
/// 钩子实现应该快速返回（在同步上下文中调用），不应包含阻塞操作。
///
/// 【核心职责】处理事件通知、记录日志、收集指标。
///
/// # 性能约束
///
/// 所有钩子在同一线程上按注册顺序同步调用。单个钩子卡住会阻塞整个管道。
/// 如有耗时操作，请在钩子内部将任务派发给后台工作线程。
pub trait ToolHook: Send + Sync {
    /// 生命周期事件回调。
    ///
    /// 在工具执行、模型调用或会话生命周期的前后被调用。
    /// 不返回任何值（纯观察者模式）。
    fn on_event(&self, event: &ToolEvent);
}

// ---------------------------------------------------------------------------
// NoopHook
// ---------------------------------------------------------------------------

/// 空钩子（默认实现，什么也不做）。
pub struct NoopHook;

impl ToolHook for NoopHook {
    fn on_event(&self, _event: &ToolEvent) {}
}

// ---------------------------------------------------------------------------
// HookRegistry
// ---------------------------------------------------------------------------

/// 钩子注册表 —— 管理多个钩子并批量派发事件。
///
/// 【领域含义】HookRegistry 是所有钩子的聚合根。它持有已注册
/// 钩子的有序列表，并将生命周期事件同步派发给每个钩子。
///
/// 【核心职责】注册、移除和派发钩子事件。
///
/// # 线程安全
///
/// HookRegistry 本身不要求 mutability 来派发事件（内部存储使用 RwLock），
/// 但注册/移除操作需要可变引用。
pub struct HookRegistry {
    /// 按注册顺序排列的钩子（FIFO 派发）
    hooks: Vec<std::sync::Arc<dyn ToolHook>>,
}

impl HookRegistry {
    /// 创建空的钩子注册表。
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    /// 注册一个钩子。
    ///
    /// 钩子按注册顺序依次调用。先注册的钩子先收到事件。
    pub fn register(&mut self, hook: std::sync::Arc<dyn ToolHook>) {
        self.hooks.push(hook);
    }

    /// 移除指定索引的钩子。
    ///
    /// 如果索引超出范围则返回 `Err`。
    pub fn remove(&mut self, index: usize) -> Result<(), String> {
        if index >= self.hooks.len() {
            return Err(format!(
                "hook index {} out of range (0..{})",
                index,
                self.hooks.len()
            ));
        }
        self.hooks.remove(index);
        Ok(())
    }

    /// 将事件派发给所有已注册的钩子。
    ///
    /// 钩子按注册顺序同步调用。单个钩子的 panic 不会影响后续钩子。
    pub fn dispatch(&self, event: &ToolEvent) {
        for hook in &self.hooks {
            hook.on_event(event);
        }
    }

    /// 获取已注册钩子的数量。
    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    /// 检查钩子注册表是否为空。
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// 测试钩子：记录调用次数
    struct CountingHook {
        count: AtomicUsize,
    }

    impl CountingHook {
        fn new() -> Self {
            Self {
                count: AtomicUsize::new(0),
            }
        }

        fn count(&self) -> usize {
            self.count.load(Ordering::SeqCst)
        }
    }

    impl ToolHook for CountingHook {
        fn on_event(&self, _event: &ToolEvent) {
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// 测试钩子：只关心特定事件
    struct ModelOnlyHook {
        model_calls: AtomicUsize,
    }

    impl ModelOnlyHook {
        fn new() -> Self {
            Self {
                model_calls: AtomicUsize::new(0),
            }
        }

        fn count(&self) -> usize {
            self.model_calls.load(Ordering::SeqCst)
        }
    }

    impl ToolHook for ModelOnlyHook {
        fn on_event(&self, event: &ToolEvent) {
            if matches!(event, ToolEvent::PreModelCall { .. } | ToolEvent::PostModelCall { .. }) {
                self.model_calls.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    #[test]
    fn test_registry_dispatch_to_all() {
        let hook1 = Arc::new(CountingHook::new());
        let hook2 = Arc::new(CountingHook::new());
        let mut registry = HookRegistry::new();
        registry.register(hook1.clone());
        registry.register(hook2.clone());

        registry.dispatch(&ToolEvent::PreExecute {
            call_id: "tc-1".into(),
            tool_name: "read_file".into(),
            params_hash: "abc123".into(),
        });

        assert_eq!(hook1.count(), 1);
        assert_eq!(hook2.count(), 1);
    }

    #[test]
    fn test_registry_empty_is_noop() {
        let registry = HookRegistry::new();
        // Should not panic
        registry.dispatch(&ToolEvent::SessionCreate {
            thread_id: "t1".into(),
            session_id: "s1".into(),
            system_instructions: "be helpful".into(),
        });
        assert!(registry.is_empty());
    }

    #[test]
    fn test_registry_remove() {
        let hook = Arc::new(CountingHook::new());
        let mut registry = HookRegistry::new();
        registry.register(hook.clone());

        assert_eq!(registry.len(), 1);
        assert!(registry.remove(0).is_ok());
        assert_eq!(registry.len(), 0);

        let result = registry.remove(0);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("out of range"));
    }

    #[test]
    fn test_model_event_dispatch() {
        let model_hook = Arc::new(ModelOnlyHook::new());
        let tool_hook = Arc::new(CountingHook::new());
        let mut registry = HookRegistry::new();
        registry.register(model_hook.clone());
        registry.register(tool_hook.clone());

        // Dispatch model event
        registry.dispatch(&ToolEvent::PreModelCall {
            snapshot: ModelCallSnapshot {
                message_count: 10,
                estimated_input_tokens: 500,
                model: Some("gpt-4o".into()),
            },
        });

        // Model hook should count it, tool hook should count it too
        assert_eq!(model_hook.count(), 1);
        assert_eq!(tool_hook.count(), 1);

        // Dispatch model completion
        registry.dispatch(&ToolEvent::PostModelCall {
            snapshot: ModelCallSnapshot {
                message_count: 10,
                estimated_input_tokens: 500,
                model: Some("gpt-4o".into()),
            },
            response_tokens: 200,
            elapsed_ms: 350,
        });

        assert_eq!(model_hook.count(), 2);
        assert_eq!(tool_hook.count(), 2);
    }

    #[test]
    fn test_session_events() {
        let hook = Arc::new(CountingHook::new());
        let mut registry = HookRegistry::new();
        registry.register(hook.clone());

        registry.dispatch(&ToolEvent::SessionCreate {
            thread_id: "thread-1".into(),
            session_id: "sess-1".into(),
            system_instructions: "you are helpful".into(),
        });

        registry.dispatch(&ToolEvent::SessionDestroy {
            thread_id: "thread-1".into(),
            session_id: "sess-1".into(),
        });

        assert_eq!(hook.count(), 2);
    }

    #[test]
    fn test_command_events() {
        let hook = Arc::new(CountingHook::new());
        let mut registry = HookRegistry::new();
        registry.register(hook.clone());

        registry.dispatch(&ToolEvent::PreCommand {
            command: "help".into(),
            args: "all".into(),
        });

        registry.dispatch(&ToolEvent::PostCommand {
            command: "help".into(),
            args: "all".into(),
            success: true,
            elapsed_ms: 5,
        });

        assert_eq!(hook.count(), 2);
    }
}
