//! Commands 子系统（Phase F）—— 斜杠命令 / 宏命令调度
//!
//! 【领域含义】提供 `Command` 注册表和调度引擎，支持：
//! - 内置命令（如 `/help`、`/plan`）
//! - 插件通过 `Plugin::commands()` 提供的自定义命令
//!
//! 【核心职责】
//! - `Command`：命令定义（名称、描述、处理函数）
//! - `CommandRegistry`：注册、解析、执行命令的统一入口
//!
//! # 使用示例
//!
//! ```rust,ignore
//! use code_agent_core::tools::command::{Command, CommandRegistry, CommandContext};
//!
//! let registry = CommandRegistry::new();
//! registry.register(Command::new("help", "Show available commands", |ctx| {
//!     Box::pin(async move { Ok("Available commands: /help, /plan".into()) })
//! }));
//!
//! let result = registry.execute("/help", CommandContext::default()).await?;
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;

use super::hook::{HookRegistry, ToolEvent};

// ---------------------------------------------------------------------------
// CommandContext
// ---------------------------------------------------------------------------

/// 命令执行的运行时上下文。
///
/// 【领域含义】封装命令执行所需的上下文信息，
/// 包括原始参数字符串和可选的工具注册表引用。
#[derive(Debug, Clone, Default)]
pub struct CommandContext {
    /// 命令名称后的原始参数（例如 `/help all` → `"all"`）
    pub args: String,
    /// 发出命令的用户身份（预留）
    pub user: Option<String>,
    /// 工作目录（预留）
    pub cwd: Option<String>,
}

// ---------------------------------------------------------------------------
// CommandResult
// ---------------------------------------------------------------------------

/// 命令执行结果。
#[derive(Debug, Clone)]
pub struct CommandResult {
    /// 输出消息（显示给用户）
    pub output: String,
    /// 是否执行成功
    pub success: bool,
}

impl CommandResult {
    /// 创建一个成功结果。
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            success: true,
        }
    }

    /// 创建一个失败结果。
    pub fn err(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            success: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Command — 命令定义
// ---------------------------------------------------------------------------

/// 命令处理函数的类型别名。
pub type CommandHandler =
    Arc<dyn Fn(CommandContext) -> BoxFuture<'static, CommandResult> + Send + Sync>;

/// 命令定义 —— 系统可调用的斜杠命令或宏命令。
///
/// 【领域含义】`Command` 是命令系统的领域实体，将命令名称与
/// 处理逻辑绑定。命令可以通过 `CommandRegistry` 注册和执行。
///
/// 【核心职责】
/// - 保存命令的元数据（名称、描述）
/// - 持有异步处理函数
#[derive(Clone)]
pub struct Command {
    /// 命令名称（不含 `/` 前缀，例如 `"help"`）
    pub name: String,
    /// 命令描述（用于 `/help` 列表展示）
    pub description: String,
    /// 异步处理函数
    handler: CommandHandler,
}

impl Command {
    /// 创建一个新命令。
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        handler: impl Fn(CommandContext) -> BoxFuture<'static, CommandResult> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            handler: Arc::new(handler),
        }
    }

    /// 执行命令。
    pub async fn execute(&self, ctx: CommandContext) -> CommandResult {
        (self.handler)(ctx).await
    }
}

// ---------------------------------------------------------------------------
// CommandRegistry
// ---------------------------------------------------------------------------

/// 命令注册表 —— 命令的注册、解析和调度中心。
///
/// 【领域含义】`CommandRegistry` 是命令系统的聚合根，管理所有
/// 已注册命令的生命周期。支持前缀匹配（`/h` 匹配 `help`）。
///
/// 【核心职责】
/// 1. 注册内置命令和插件提供的命令
/// 2. 按名称（含前缀）查找命令
/// 3. 执行命令并返回结果
/// 4. 列出所有可用命令
pub struct CommandRegistry {
    /// 命令名 → Command 的映射
    commands: HashMap<String, Command>,
    /// `/help` 命令共享的描述列表（在 `register` 时自动更新）
    help_descriptions: Arc<std::sync::RwLock<Vec<(String, String)>>>,
    /// 可选的钩子注册表（用于 PreCommand / PostCommand 事件派发）
    hook_registry: Option<Arc<HookRegistry>>,
}

impl CommandRegistry {
    /// 创建空的命令注册表。
    pub fn new() -> Self {
        Self {
            commands: HashMap::new(),
            help_descriptions: Arc::new(std::sync::RwLock::new(Vec::new())),
            hook_registry: None,
        }
    }

    /// 设置钩子注册表（构建器方法）。
    ///
    /// 设置后，每次 `execute()` 调用将在命令执行前后派发
    /// `PreCommand` 和 `PostCommand` 事件。
    pub fn with_hooks(mut self, registry: Arc<HookRegistry>) -> Self {
        self.hook_registry = Some(registry);
        self
    }

    /// 注册一个命令。
    ///
    /// 如果同名命令已存在，返回 `Err`。
    pub fn register(&mut self, command: Command) -> Result<(), String> {
        let name = command.name.clone();
        if self.commands.contains_key(&name) {
            return Err(format!("command already registered: /{}", name));
        }
        self.commands.insert(name, command);
        // 更新帮助描述列表
        *self.help_descriptions.write().unwrap() = self.list_descriptions();
        Ok(())
    }

    /// 按完整名称查找命令。
    pub fn get(&self, name: &str) -> Option<&Command> {
        // 去掉前导 `/`
        let trimmed = name.trim_start_matches('/');
        self.commands.get(trimmed)
    }

    /// 通过前缀查找命令（唯一匹配）。
    ///
    /// 【领域含义】支持用户输入 `/h` 匹配 `help`。
    /// 如果前缀不明确（匹配多个），返回 `Err`。
    pub fn resolve(&self, input: &str) -> Result<&Command, CommandError> {
        let trimmed = input.trim_start_matches('/');
        if trimmed.is_empty() {
            return Err(CommandError::Empty);
        }

        // 1. 精确匹配
        if let Some(cmd) = self.commands.get(trimmed) {
            return Ok(cmd);
        }

        // 2. 前缀匹配
        let matches: Vec<&String> = self
            .commands
            .keys()
            .filter(|k| k.starts_with(trimmed))
            .collect();

        match matches.len() {
            0 => Err(CommandError::NotFound(trimmed.to_string())),
            1 => Ok(self.commands.get(matches[0]).unwrap()),
            _ => Err(CommandError::Ambiguous {
                input: trimmed.to_string(),
                candidates: matches.into_iter().map(|s| format!("/{}", s)).collect(),
            }),
        }
    }

    /// 执行命令。
    ///
    /// 解析输入 → 查找命令 → 提取参数 → 派发钩子 → 执行。
    /// 支持 `/help all` 形式，其中 `all` 作为 `ctx.args`。
    ///
    /// 如果设置了 `hook_registry`，将在命令执行前后分别派发
    /// `PreCommand` 和 `PostCommand` 事件（包括解析/执行失败路径）。
    pub async fn execute(&self, input: &str, mut ctx: CommandContext) -> CommandResult {
        let input = input.trim();
        let (cmd_name, args) = match input.split_once(char::is_whitespace) {
            Some((name, rest)) => (name, rest.trim().to_string()),
            None => (input, String::new()),
        };

        ctx.args = args.clone();

        // ── PreCommand hook ──────────────────────────────────────────
        if let Some(ref hooks) = self.hook_registry {
            hooks.dispatch(&ToolEvent::PreCommand {
                command: cmd_name.to_string(),
                args: args.clone(),
            });
        }

        let start = std::time::Instant::now();

        // ── Resolve + Execute ─────────────────────────────────────────
        let result = match self.resolve(cmd_name) {
            Ok(cmd) => cmd.execute(ctx).await,
            Err(e) => CommandResult::err(e.to_string()),
        };

        let elapsed_ms = start.elapsed().as_millis() as u64;

        // ── PostCommand hook ──────────────────────────────────────────
        if let Some(ref hooks) = self.hook_registry {
            hooks.dispatch(&ToolEvent::PostCommand {
                command: cmd_name.to_string(),
                args,
                success: result.success,
                elapsed_ms,
            });
        }

        result
    }

    /// 获取所有已注册命令的名称列表。
    pub fn command_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.commands.keys().map(|n| format!("/{}", n)).collect();
        names.sort();
        names
    }

    /// 获取所有已注册命令的描述列表。
    pub fn list_descriptions(&self) -> Vec<(String, String)> {
        let mut items: Vec<(String, String)> = self
            .commands
            .values()
            .map(|c| (format!("/{}", c.name), c.description.clone()))
            .collect();
        items.sort_by(|a, b| a.0.cmp(&b.0));
        items
    }

    /// 注册数量。
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// CommandError
// ---------------------------------------------------------------------------

/// 命令解析/执行错误。
#[derive(Debug, Clone)]
pub enum CommandError {
    /// 输入为空
    Empty,
    /// 未找到命令
    NotFound(String),
    /// 前缀匹配到多个命令
    Ambiguous {
        input: String,
        candidates: Vec<String>,
    },
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::Empty => write!(f, "empty command input"),
            CommandError::NotFound(name) => write!(f, "unknown command: /{name}"),
            CommandError::Ambiguous { input, candidates } => {
                write!(f, "/{input} is ambiguous, did you mean: {}", candidates.join(", "))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// register_defaults — 注册内置命令
// ---------------------------------------------------------------------------

impl CommandRegistry {
    /// 注册内置命令（`/help`, `/status` 等）。
    ///
    /// 应该在所有插件命令注册之前调用，确保内置命令可用。
    pub fn register_defaults(&mut self) {
        // ── /help ────────────────────────────────────────────────────
        self.register_help_cmd();

        // ── /status ──────────────────────────────────────────────────
        self.register_status_cmd();
    }

    fn register_help_cmd(&mut self) {
        if self.commands.contains_key("help") {
            return;
        }
        let help_descriptions = Arc::clone(&self.help_descriptions);
        self.commands.insert(
            "help".to_string(),
            Command::new("help", "Show available commands and usage", move |ctx: CommandContext| {
                let desc_clone = help_descriptions.clone();
                Box::pin(async move {
                    let desc = desc_clone.read().unwrap().clone();
                    let args = ctx.args.to_lowercase();
                    let filtered: Vec<&(String, String)> = if args.is_empty() {
                        desc.iter().collect()
                    } else {
                        desc.iter().filter(|(name, _)| name.contains(&args)).collect()
                    };

                    if filtered.is_empty() {
                        return CommandResult::ok("No matching commands found. Try `/help`.");
                    }

                    let max_name_len = filtered.iter().map(|(n, _)| n.len()).max().unwrap_or(10);
                    let mut lines = vec![
                        "Available commands:".to_string(),
                        "─".repeat(60),
                    ];
                    for (name, desc_text) in &filtered {
                        lines.push(format!("  {:<width$}  {}", name, desc_text, width = max_name_len));
                    }
                    lines.push("─".repeat(60));
                    lines.push("Tip: use `/help <keyword>` to filter.".to_string());

                    CommandResult::ok(lines.join("\n"))
                })
            }),
        );
        // Refresh descriptions
        *self.help_descriptions.write().unwrap() = self.list_descriptions();
    }

    fn register_status_cmd(&mut self) {
        if self.commands.contains_key("status") {
            return;
        }
        // /status shows a snapshot of the command registry stats.
        // Runtime integration (TUI/exec) can extend this with session info.
        self.commands.insert(
            "status".to_string(),
            Command::new("status", "Show system status and loaded commands", |_ctx: CommandContext| {
                Box::pin(async {
                    // This is a placeholder — the TUI or exec layer should
                    // replace or extend this handler with real session state.
                    CommandResult::ok(
                        "System Status\n\
                         ─────────────\n\
                         All systems nominal. Use `/help` for available commands."
                    )
                })
            }),
        );
        *self.help_descriptions.write().unwrap() = self.list_descriptions();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_get() {
        let mut reg = CommandRegistry::new();
        assert!(reg.is_empty());

        reg.register(Command::new("help", "Show help", |_ctx| {
            Box::pin(async { CommandResult::ok("help output".to_string()) })
        }))
        .unwrap();

        assert_eq!(reg.len(), 1);
        assert!(reg.get("help").is_some());
        assert!(reg.get("/help").is_some());
    }

    #[tokio::test]
    async fn test_duplicate_register_fails() {
        let mut reg = CommandRegistry::new();
        reg.register(Command::new("ping", "Ping", |_ctx| {
            Box::pin(async { CommandResult::ok("pong".to_string()) })
        }))
        .unwrap();

        let result = reg.register(Command::new("ping", "Duplicate", |_ctx| {
            Box::pin(async { CommandResult::ok("pong".to_string()) })
        }));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_resolve_exact() {
        let mut reg = CommandRegistry::new();
        reg.register(Command::new("help", "Show help", |_ctx| {
            Box::pin(async { CommandResult::ok("help".to_string()) })
        }))
        .unwrap();

        assert!(reg.resolve("help").is_ok());
        assert!(reg.resolve("/help").is_ok());
    }

    #[tokio::test]
    async fn test_resolve_prefix_unique() {
        let mut reg = CommandRegistry::new();
        reg.register(Command::new("help", "Show help", |_ctx| {
            Box::pin(async { CommandResult::ok("help".to_string()) })
        }))
        .unwrap();
        reg.register(Command::new("history", "Show history", |_ctx| {
            Box::pin(async { CommandResult::ok("history".to_string()) })
        }))
        .unwrap();

        // "hi" should uniquely match "history"
        let result = reg.resolve("hi");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().name, "history");
    }

    #[tokio::test]
    async fn test_resolve_prefix_ambiguous() {
        let mut reg = CommandRegistry::new();
        reg.register(Command::new("help", "Show help", |_ctx| {
            Box::pin(async { CommandResult::ok("help".to_string()) })
        }))
        .unwrap();
        reg.register(Command::new("history", "Show history", |_ctx| {
            Box::pin(async { CommandResult::ok("history".to_string()) })
        }))
        .unwrap();

        // "h" matches both "help" and "history"
        let result = reg.resolve("h");
        assert!(matches!(result, Err(CommandError::Ambiguous { .. })));
    }

    #[tokio::test]
    async fn test_resolve_not_found() {
        let reg = CommandRegistry::new();
        let result = reg.resolve("nosuch");
        assert!(matches!(result, Err(CommandError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_execute_with_args() {
        let mut reg = CommandRegistry::new();
        reg.register(Command::new("echo", "Echo args", |ctx| {
            Box::pin(async move { CommandResult::ok(ctx.args) })
        }))
        .unwrap();

        let result = reg.execute("/echo hello world", CommandContext::default()).await;
        assert!(result.success);
        assert_eq!(result.output, "hello world");
    }

    #[tokio::test]
    async fn test_execute_no_slash() {
        let mut reg = CommandRegistry::new();
        reg.register(Command::new("ping", "Ping", |_ctx| {
            Box::pin(async { CommandResult::ok("pong".to_string()) })
        }))
        .unwrap();

        let result = reg.execute("ping", CommandContext::default()).await;
        assert!(result.success);
        assert_eq!(result.output, "pong");
    }

    #[tokio::test]
    async fn test_list_descriptions() {
        let mut reg = CommandRegistry::new();
        reg.register(Command::new("alpha", "First command", |_ctx| {
            Box::pin(async { CommandResult::ok(String::new()) })
        }))
        .unwrap();
        reg.register(Command::new("beta", "Second command", |_ctx| {
            Box::pin(async { CommandResult::ok(String::new()) })
        }))
        .unwrap();

        let list = reg.list_descriptions();
        assert_eq!(list.len(), 2);
        // Sorted
        assert_eq!(list[0].0, "/alpha");
        assert_eq!(list[0].1, "First command");
        assert_eq!(list[1].0, "/beta");
    }

    #[tokio::test]
    async fn test_default_registry_has_help() {
        let mut reg = CommandRegistry::new();
        reg.register_defaults();
        assert!(reg.len() >= 1);
        assert!(reg.get("help").is_some());

        // /help should work
        let result = reg.execute("/help", CommandContext::default()).await;
        assert!(result.success);
        assert!(result.output.contains("help"));
    }

    #[tokio::test]
    async fn test_empty_input_error() {
        let reg = CommandRegistry::new();
        let result = reg.execute("", CommandContext::default()).await;
        assert!(!result.success);
        assert!(result.output.contains("empty"));
    }

    // ── Hook dispatch tests ──────────────────────────────────────────

    use crate::tools::hook::ToolHook;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 测试钩子：分别记录 PreCommand 和 PostCommand 调用次数
    struct CommandCountingHook {
        pre_count: AtomicUsize,
        post_count: AtomicUsize,
    }

    impl CommandCountingHook {
        fn new() -> Self {
            Self {
                pre_count: AtomicUsize::new(0),
                post_count: AtomicUsize::new(0),
            }
        }

        fn pre_count(&self) -> usize {
            self.pre_count.load(Ordering::SeqCst)
        }

        fn post_count(&self) -> usize {
            self.post_count.load(Ordering::SeqCst)
        }
    }

    impl ToolHook for CommandCountingHook {
        fn on_event(&self, event: &ToolEvent) {
            match event {
                ToolEvent::PreCommand { .. } => {
                    self.pre_count.fetch_add(1, Ordering::SeqCst);
                }
                ToolEvent::PostCommand { .. } => {
                    self.post_count.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn test_hook_dispatch_on_success() {
        let mut hook_registry = HookRegistry::new();
        let hook = Arc::new(CommandCountingHook::new());
        hook_registry.register(hook.clone());

        let mut reg = CommandRegistry::new().with_hooks(Arc::new(hook_registry));
        reg.register(Command::new("ping", "Ping", |_ctx| {
            Box::pin(async { CommandResult::ok("pong") })
        }))
        .unwrap();

        let result = reg.execute("ping", CommandContext::default()).await;
        assert!(result.success);
        assert_eq!(hook.pre_count(), 1);
        assert_eq!(hook.post_count(), 1);
    }

    #[tokio::test]
    async fn test_hook_dispatch_on_error() {
        let mut hook_registry = HookRegistry::new();
        let hook = Arc::new(CommandCountingHook::new());
        hook_registry.register(hook.clone());

        let reg = CommandRegistry::new().with_hooks(Arc::new(hook_registry));

        let result = reg.execute("/nonexistent", CommandContext::default()).await;
        assert!(!result.success);
        // Hooks still fire on both pre and post even for failed commands
        assert_eq!(hook.pre_count(), 1);
        assert_eq!(hook.post_count(), 1);
    }

    #[tokio::test]
    async fn test_no_hooks_still_works() {
        // Without hooks, execute() should work identically to before
        let mut reg = CommandRegistry::new();
        reg.register(Command::new("ping", "Ping", |_ctx| {
            Box::pin(async { CommandResult::ok("pong") })
        }))
        .unwrap();

        let result = reg.execute("ping", CommandContext::default()).await;
        assert!(result.success);
        assert_eq!(result.output, "pong");
    }
}
