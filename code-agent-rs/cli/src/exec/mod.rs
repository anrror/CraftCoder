//! Headless exec mode for CI/automation.
//!
//! Provides [`ExecCli`] — a non-interactive agent runner that connects
//! directly to [`code_agent_core::agent::Session`] (not via the App Server)
//! and streams results to stdout.
//!
//! # Usage
//!
//! ```bash
//! code-agent exec "fix lint errors" --model qwen3.6-27b --output json
//! ```
//!
//! # Exit Codes
//!
//! | Code | Meaning       |
//! |------|---------------|
//! | 0    | Success       |
//! | 1    | Agent error   |
//! | 2    | Tool error    |
//! | 3    | Timeout       |

use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use code_agent_core::agent::plan::{DecompositionEngine, KnowledgeConfig, PlanConfig, SpecConfig};
use code_agent_core::agent::{Session, SessionConfigBuilder, SessionRunner};
use code_agent_core::model::{create_model_client, ModelClient, ModelConfig, ProviderKind};
use code_agent_core::tools::plugin::{McpConnectionConfig, McpPlugin, PluginLoadResult, PluginManager};
use code_agent_core::tools::registry::{DefaultToolRegistry, ToolRegistry};
use code_agent_tools::mcp::adapter::McpClientConnector;
use crate::config::McpServerConfig;
use code_agent_protocol::{
    Message, ResponseEvent, SessionId, ThreadId, TurnInput,
};

mod processor;

pub use processor::EventProcessor;

// ---------------------------------------------------------------------------
// OutputFormat
// ---------------------------------------------------------------------------

/// 输出格式
///
/// 【领域含义】控制 Agent 事件如何渲染到标准输出的领域枚举。
/// 【核心职责】区分 JSON 流式输出、纯文本输出和静默模式三种格式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    /// JSON — 每个 ResponseEvent 作为一行 JSON 输出到 stdout。
    Json,
    /// 文本 — 仅输出最终答案文本（不含工具调用噪音）。
    Text,
    /// 静默 — 无输出，调用方仅关心退出码。
    Silent,
}

impl OutputFormat {
    /// 从 CLI 参数解析输出格式
    ///
    /// 【领域含义】将 `--output` CLI 标志的值解析为 OutputFormat 枚举。
    /// 【核心职责】支持 "json"、"text"、"silent" 三种格式的解析。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self, ExecError> {
        match s.to_lowercase().as_str() {
            "json" => Ok(Self::Json),
            "text" => Ok(Self::Text),
            "silent" => Ok(Self::Silent),
            other => Err(ExecError::Config(format!(
                "unknown output format '{other}'. Valid: json, text, silent"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// ExecExitCode
// ---------------------------------------------------------------------------

/// 执行退出码
///
/// 【领域含义】ExecCli::execute 返回的进程退出码，对应标准 Unix 退出码约定。
/// 【核心职责】区分成功、Agent 错误、工具错误和超时四种退出状态，可直接传播给 std::process::exit。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum ExecExitCode {
    /// 成功 — 任务完成。
    Success = 0,
    /// Agent 错误 — 模型错误、达到最大迭代次数等。
    AgentError = 1,
    /// 工具错误 — 工具执行失败。
    ToolError = 2,
    /// 超时 — 执行超时。
    Timeout = 3,
}

impl ExecExitCode {
    /// 获取数值退出码
    ///
    /// 【领域含义】返回枚举对应的 i32 数值退出码。
    /// 【核心职责】供进程退出时使用。
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}

// ---------------------------------------------------------------------------
// ExecError
// ---------------------------------------------------------------------------

/// 执行错误
///
/// 【领域含义】表示无头执行模式设置或执行过程中可能发生的领域错误。
/// 【核心职责】封装配置错误、I/O 错误和模型提供商错误。
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    /// 配置或 CLI 参数错误
    #[error("config error: {0}")]
    Config(String),

    /// 输出写入 I/O 错误
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// 模型提供商错误
    #[error("model error: {0}")]
    Model(#[from] code_agent_core::model::ModelError),
}

// ---------------------------------------------------------------------------
// ExecCli
// ---------------------------------------------------------------------------

/// 无头 CLI 运行器
///
/// 【领域含义】面向 CI/自动化的无头 Agent 运行器，直接连接 code_agent_core::agent::Session。
/// 【核心职责】提供非交互式执行能力，支持 JSON/文本/静默输出格式，适用于 CI 流水线和脚本。
///
/// 当 `plan` 为 true 时，使用 Plan 引擎进行任务分解（DAG）后执行。
///
/// # 示例
///
/// ```rust,ignore
/// # #[tokio::main]
/// # async fn main() {
/// use code_agent_cli::exec::{ExecCli, OutputFormat};
///
/// let cli = ExecCli::new("fix the build".into())
///     .model("qwen3.6-27b".into())
///     .timeout_secs(120)
///     .output_format(OutputFormat::Json);
///
/// let exit_code = cli.execute().await.unwrap();
/// std::process::exit(exit_code.as_i32());
/// # }
/// ```
pub struct ExecCli {
    /// 用户任务提示
    prompt: String,
    /// 模型名称覆盖（默认：ModelConfig 默认值）
    model: Option<String>,
    /// 最大执行秒数（超时后终止 Agent）
    timeout_secs: u64,
    /// stdout 输出格式
    output_format: OutputFormat,
    /// 工作目录覆盖
    cwd: Option<PathBuf>,
    /// 模型客户端覆盖（用于测试，设置后忽略 model）
    model_client: Option<Arc<dyn ModelClient>>,
    /// 工具注册表覆盖（用于测试）
    tool_registry: Option<Arc<dyn ToolRegistry>>,
    /// 是否使用 Plan 引擎（DAG 任务分解 + 并行执行）
    plan: bool,
    /// 质量门禁配置（Plan 模式使用，None = 默认值）
    quality: Option<crate::config::QualityConfig>,
    /// 知识模块配置（Plan 模式使用，None = 默认值）
    knowledge: Option<KnowledgeConfig>,
}

impl ExecCli {
    /// 创建无头 CLI 运行器
    ///
    /// 【领域含义】构造 ExecCli 实例，指定用户提示。
    /// 【核心职责】初始化运行器，默认超时 300 秒、文本输出、从环境变量读取模型配置。
    pub fn new(prompt: String) -> Self {
        Self {
            prompt,
            model: None,
            timeout_secs: 300,
            output_format: OutputFormat::Text,
            cwd: None,
            model_client: None,
            tool_registry: None,
            plan: false,
            quality: None,
            knowledge: None,
        }
    }

    /// 设置质量门禁配置
    pub fn quality(mut self, v: crate::config::QualityConfig) -> Self {
        self.quality = Some(v);
        self
    }

    /// 设置知识模块配置
    pub fn knowledge(mut self, v: KnowledgeConfig) -> Self {
        self.knowledge = Some(v);
        self
    }

    /// 设置模型名称
    ///
    /// 【领域含义】覆盖使用的模型名称。
    /// 【核心职责】设置 model 字段。
    pub fn model(mut self, name: String) -> Self {
        self.model = Some(name);
        self
    }

    /// 设置超时时间（秒）
    ///
    /// 【领域含义】设置 Agent 执行的最大超时时间。
    /// 【核心职责】设置 timeout_secs 字段。
    pub fn timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// 设置输出格式
    ///
    /// 【领域含义】设置 stdout 的输出格式。
    /// 【核心职责】设置 output_format 字段。
    pub fn output_format(mut self, fmt: OutputFormat) -> Self {
        self.output_format = fmt;
        self
    }

    /// 设置工作目录
    ///
    /// 【领域含义】设置 Agent 执行的工作目录。
    /// 【核心职责】设置 cwd 字段。
    pub fn cwd(mut self, path: PathBuf) -> Self {
        self.cwd = Some(path);
        self
    }

    /// 启用 Plan 引擎模式（DAG 任务分解 + 并行执行）
    ///
    /// 【领域含义】启用 Plan 引擎后，prompt 会被分解为结构化 DAG 任务计划再执行。
    pub fn plan(mut self, enabled: bool) -> Self {
        self.plan = enabled;
        self
    }

    /// 设置模型客户端覆盖（用于测试）
    ///
    /// 【领域含义】覆盖模型客户端，主要用于测试场景。
    /// 【核心职责】设置 model_client 字段，设置后 model 字段被忽略。
    pub fn with_model_client(mut self, client: Arc<dyn ModelClient>) -> Self {
        self.model_client = Some(client);
        self
    }

    /// 设置工具注册表覆盖（用于测试）
    ///
    /// 【领域含义】覆盖工具注册表，主要用于测试场景。
    /// 【核心职责】设置 tool_registry 字段。
    pub fn with_tool_registry(mut self, registry: Arc<dyn ToolRegistry>) -> Self {
        self.tool_registry = Some(registry);
        self
    }

    /// Read stdin into the task context if data is available.
    ///
    /// When piping input (`echo "some context" | code-agent exec "fix bugs"`),
    /// the stdin content is appended to the prompt as additional context.
    fn read_stdin_context(&self) -> Option<String> {
        // Only try to read if stdin is a pipe (not a TTY).
        // On Windows, we check via atty crate equivalent — we use a simple
        // peek approach: try reading a single byte non-blockingly.
        // Since we're in a headless CI context, we just attempt reads.

        // Simple approach: read all of stdin with a short timeout.
        // If there's nothing, we return None.
        let mut buffer = String::new();
        match io::stdin().read_to_string(&mut buffer) {
            Ok(_) if !buffer.is_empty() => Some(buffer.trim().to_string()),
            Ok(_) => None,
            Err(_) => None,
        }
    }

    /// Build the full prompt including stdin context.
    fn build_full_prompt(&self) -> String {
        if let Some(context) = self.read_stdin_context() {
            format!("{}\n\n--- Additional context (from stdin) ---\n{}", self.prompt, context)
        } else {
            self.prompt.clone()
        }
    }

    /// Resolve the model client (override or environment).
    async fn resolve_model_client(&self) -> Result<Arc<dyn ModelClient>, ExecError> {
        if let Some(ref client) = self.model_client {
            return Ok(Arc::clone(client));
        }

        let mut builder = ModelConfig::builder();
        builder = builder.api_key(
            std::env::var("LLM_API_KEY")
                .map_err(|_| ExecError::Config("LLM_API_KEY not set".into()))?,
        );

        let model = if let Some(ref model) = self.model {
            model.clone()
        } else {
            std::env::var("LLM_CHAT_MODEL")
                .map_err(|_| ExecError::Config("LLM_CHAT_MODEL not set (set via --model or env var)".into()))?
        };
        builder = builder.model(model);

        let config = builder
            .build()
            .map_err(|e| ExecError::Config(e.to_string()))?;

        Ok(create_model_client(
            ProviderKind::from_model_name(&config.model),
            config,
        ))
    }

    /// Resolve the tool registry, registering built-in tools and MCP plugins by default.
    fn resolve_tool_registry(&self) -> Arc<dyn ToolRegistry> {
        if let Some(ref reg) = self.tool_registry {
            return Arc::clone(reg);
        }
        let mut registry = DefaultToolRegistry::new();
        code_agent_tools::register_all_core_tools(&mut registry);
        register_mcp_plugins_from_env(&mut registry);

        Arc::new(registry)
    }
}

/// Create a ToolRegistry pre-populated with all built-in tools.
///
/// Shared helper for exec mode and TUI mode to avoid repeating
/// the tool registration boilerplate.
///
/// `mcp_configs` — optional MCP server configurations from TOML config;
/// these are registered in addition to any `MCP_SERVERS` / `MCP_SERVER_COMMAND`
/// environment variable (env vars have higher priority).
pub fn create_default_tool_registry(mcp_configs: &[McpServerConfig]) -> DefaultToolRegistry {
    let mut registry = DefaultToolRegistry::new();

    // ── Phase 0: 内置文件工具（canonical registration in tools crate）──
    code_agent_tools::register_all_core_tools(&mut registry);

    // ── Phase 1: Shell 沙箱工具 ─────────────────────────────────
    register_shell_tool(&mut registry);

    // ── Phase 2: Git 版本控制工具 ───────────────────────────────
    register_git_tools(&mut registry);

    // ── Phase 3: LSP 代码智能工具 ───────────────────────────────
    register_lsp_tools(&mut registry);

    // ── Phase 4: MCP 远程工具（通过 Plugin 系统）─────────────────
    // 环境变量（MCP_SERVERS / MCP_SERVER_COMMAND）优先于 TOML 配置
    register_mcp_plugins_from_env(&mut registry);
    // TOML 配置文件中的 [mcp_servers] 节作为补充
    if !mcp_configs.is_empty() {
        register_mcp_plugins_from_configs(mcp_configs, &mut registry);
    }

    registry
}

/// 注册 Shell 沙箱执行工具
fn register_shell_tool(registry: &mut dyn ToolRegistry) {
    use crate::tools::shell::ShellTool;
    let tool: Arc<dyn code_agent_core::tools::Tool> = Arc::new(ShellTool::new());
    if let Err(e) = registry.register(tool) {
        tracing::warn!(error = %e, "Failed to register ShellTool");
    }
}

/// 注册 Git 版本控制工具（status / diff / log）
fn register_git_tools(registry: &mut dyn ToolRegistry) {
    use crate::tools::git::{GitStatusTool, GitDiffTool, GitLogTool, GitCommitTool};
    let repo_path = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let tools: [Arc<dyn code_agent_core::tools::Tool>; 4] = [
        Arc::new(GitStatusTool::new(repo_path.clone())),
        Arc::new(GitDiffTool::new(repo_path.clone())),
        Arc::new(GitLogTool::new(repo_path.clone())),
        Arc::new(GitCommitTool::new(repo_path)),
    ];
    for tool in tools {
        if let Err(e) = registry.register(tool) {
            tracing::warn!(error = %e, "Failed to register Git tool");
        }
    }
}

/// 注册 LSP 代码智能工具（diagnostics / definition / references / hover）
fn register_lsp_tools(registry: &mut dyn ToolRegistry) {
    use crate::tools::lsp::{
        LspDiagnosticsTool, LspDefinitionTool, LspReferencesTool, LspHoverTool,
    };
    let tools: [Arc<dyn code_agent_core::tools::Tool>; 4] = [
        Arc::new(LspDiagnosticsTool::new()),
        Arc::new(LspDefinitionTool::new()),
        Arc::new(LspReferencesTool::new()),
        Arc::new(LspHoverTool::new()),
    ];
    for tool in tools {
        if let Err(e) = registry.register(tool) {
            tracing::warn!(error = %e, "Failed to register LSP tool");
        }
    }
}

/// 从环境变量注册 MCP 插件，委托给 `code_agent_tools::mcp::register_from_env`。
fn register_mcp_plugins_from_env(registry: &mut dyn ToolRegistry) {
    code_agent_tools::mcp::register_from_env(registry);
}

/// 从一组 `McpServerConfig` 配置注册 MCP 插件。
///
/// 【领域含义】为每个有效的服务器配置创建 `McpPlugin` 实例，
/// 通过 `PluginManager` 批量加载到 `ToolRegistry`。
///
/// 【核心职责】遍历配置 → 过滤无效 → 创建 McpPlugin → load_all。
fn register_mcp_plugins_from_configs(configs: &[McpServerConfig], registry: &mut dyn ToolRegistry) {
    let rt = match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle,
        Err(_) => {
            tracing::warn!("Cannot register MCP plugins: no tokio runtime available");
            return;
        }
    };

    rt.block_on(async {
        let mut pm = PluginManager::new();

        for cfg in configs {
            if !cfg.is_valid() {
                tracing::warn!(
                    name = ?cfg.name,
                    "Skipping MCP server config: must set command or url"
                );
                continue;
            }

            let connector = McpClientConnector::new();
            let name = cfg.display_name().to_string();
            let connection_config = if let Some(ref cmd) = cfg.command {
                McpConnectionConfig::Stdio {
                    command: cmd.clone(),
                    args: cfg.args.clone(),
                }
            } else if let Some(ref url) = cfg.url {
                McpConnectionConfig::Http { url: url.clone() }
            } else {
                continue; // is_valid already checked, but satisfy exhaustiveness
            };

            let plugin = McpPlugin::new(name.clone(), name, connection_config, Box::new(connector));
            pm.register(Box::new(plugin));
        }

        let results = pm.load_all(registry).await;
        for result in &results {
            match result {
                PluginLoadResult::Success { name } => {
                    tracing::info!(plugin = %name, "MCP plugin registered successfully");
                }
                PluginLoadResult::Failed { name, error } => {
                    tracing::warn!(plugin = %name, error = %error, "MCP plugin failed to load");
                }
            }
        }
    });
}

// =========================================================================
// ExecCli impl
// =========================================================================

impl ExecCli {
    /// 执行 Agent（无头模式）
    ///
    /// 【领域含义】在无头模式下运行 Agent，返回应传播给进程的退出码。
    /// 【核心职责】解析模型客户端和工具注册表 → 构建 Session → 运行一轮 → 通过 EventProcessor 处理事件 → 映射退出码。
    /// 超时时中断 Session 并返回 Timeout。
    /// 当 `plan` 为 true 时，使用 Plan 引擎分解 prompt 为 DAG 任务后执行。
    pub async fn execute(&self) -> Result<ExecExitCode, ExecError> {
        let model_client = self.resolve_model_client().await?;
        let tool_registry = self.resolve_tool_registry();
        let full_prompt = self.build_full_prompt();

        // ── Plan mode: decompose + DAG execution ────────────────
        if self.plan {
            return self.execute_plan(model_client, tool_registry, &full_prompt).await;
        }

        // ── Normal mode: single session turn ────────────────────
        // Build session config
        let session_config = SessionConfigBuilder::default()
            .id(SessionId::from(format!("exec-{}", uuid::Uuid::new_v4())))
            .max_iterations(50)
            .model_client(Arc::clone(&model_client))
            .tool_registry(Arc::clone(&tool_registry))
            .build();

        let mut session = Session::new(session_config).await;
        let mut processor = EventProcessor::new(self.output_format);

        // Build turn input
        let turn_input = TurnInput {
            thread_id: ThreadId::from("exec-thread"),
            messages: vec![Message::UserMessage {
                content: full_prompt,
            }],
        };

        // Execute with timeout
        let timeout = Duration::from_secs(self.timeout_secs);
        let events_result: Result<Vec<ResponseEvent>, _> =
            tokio::time::timeout(timeout, session.run_turn(turn_input)).await;

        match events_result {
            Ok(events) => {
                processor.process(&events);
                Ok(processor.exit_code())
            }
            Err(_elapsed) => {
                // Timeout — kill the session
                session.interrupt();
                if self.output_format != OutputFormat::Silent {
                    eprintln!("error: exec timed out after {}s", self.timeout_secs);
                }
                Ok(ExecExitCode::Timeout)
            }
        }
    }

    /// Plan 模式：分解 prompt → DAG 执行
    async fn execute_plan(
        &self,
        model_client: Arc<dyn ModelClient>,
        tool_registry: Arc<dyn ToolRegistry>,
        prompt: &str,
    ) -> Result<ExecExitCode, ExecError> {
        use code_agent_core::agent::plan::Decomposer;

        let output = self.output_format;

        // 1. Build a non-streaming model client for decomposition
        let decomp_client = self.resolve_decomp_client().await?;

        // 2. Decompose
        let engine = DecompositionEngine::new(decomp_client);
        let mut plan = engine
            .decompose(prompt, "")
            .await
            .map_err(|e| ExecError::Config(format!("Decomposition failed: {e}")))?;

        if output != OutputFormat::Silent {
            println!(
                "Plan: {} ({} phases, {} nodes)",
                plan.name,
                plan.phases.len(),
                plan.nodes.len()
            );
            for phase in &plan.phases {
                println!("  ▸ {phase}");
            }
        }

        // 3. Execute plan (auto-approved in exec mode)
        let plan_config = build_plan_config(self.quality.as_ref());
        let mut runner = SessionRunner::new(model_client, tool_registry)
            .with_plan_config(plan_config);

        // Phase E: 注入知识提供者
        if let Some(ref kcfg) = self.knowledge {
            let kp = kcfg.build_provider().await;
            runner = runner.with_knowledge(kp);
        }

        let cancel = Arc::new(AtomicBool::new(false));
        let timeout = Duration::from_secs(self.timeout_secs);

        let result = tokio::time::timeout(timeout, runner.run_plan(&mut plan, cancel, None)).await;

        match result {
            Ok(Ok(report)) => {
                if output != OutputFormat::Silent {
                    println!(
                        "Plan completed: {}/{} nodes succeeded",
                        report.completed_nodes, report.total_nodes
                    );
                }
                if report.success {
                    Ok(ExecExitCode::Success)
                } else {
                    Ok(ExecExitCode::AgentError)
                }
            }
            Ok(Err(e)) => {
                if output != OutputFormat::Silent {
                    eprintln!("Plan execution error: {e}");
                }
                Ok(ExecExitCode::AgentError)
            }
            Err(_elapsed) => {
                if output != OutputFormat::Silent {
                    eprintln!("error: plan exec timed out after {}s", self.timeout_secs);
                }
                Ok(ExecExitCode::Timeout)
            }
        }
    }

    /// Build a non-streaming model client for plan decomposition.
    async fn resolve_decomp_client(&self) -> Result<Box<dyn ModelClient>, ExecError> {
        if let Some(ref client) = self.model_client {
            let model_name = client.model_name().to_string();
            let config = ModelConfig::builder()
                .model(model_name)
                .api_key(String::new())
                .stream(false)
                .build()
                .map_err(|e| ExecError::Config(e.to_string()))?;
            return Ok(build_decomp_client(config));
        }

        let mut builder = ModelConfig::builder();
        builder = builder.api_key(
            std::env::var("LLM_API_KEY")
                .map_err(|_| ExecError::Config("LLM_API_KEY not set".into()))?,
        );

        let model = if let Some(ref model) = self.model {
            model.clone()
        } else {
            std::env::var("LLM_CHAT_MODEL")
                .map_err(|_| ExecError::Config("LLM_CHAT_MODEL not set".into()))?
        };
        builder = builder.model(model);
        builder = builder.stream(false);

        let config = builder
            .build()
            .map_err(|e| ExecError::Config(e.to_string()))?;

        Ok(build_decomp_client(config))
    }
}

/// Build a boxed non-streaming model client for plan decomposition.
///
/// This is separate from `create_model_client()` because the decomposition
/// engine takes `Box<dyn ModelClient>`, not `Arc<dyn ModelClient>`.
fn build_decomp_client(config: ModelConfig) -> Box<dyn ModelClient> {
    use code_agent_core::model::ProviderKind;

    match ProviderKind::from_model_name(&config.model) {
        ProviderKind::Qwen3 => Box::new(code_agent_core::model::Qwen3OpenAIClient::new(config)),
        ProviderKind::Anthropic => Box::new(code_agent_core::model::AnthropicClient::new(config)),
        ProviderKind::Gemini => Box::new(code_agent_core::model::GeminiClient::new(config)),
    }
}

// ---------------------------------------------------------------------------
// build_plan_config — 从 CLI QualityConfig 构造 PlanConfig
// ---------------------------------------------------------------------------

/// 从可选的 QualityConfig 构建 PlanConfig。
///
/// 【领域含义】将 CLI TOML 配置中的 `[quality]` 节映射为 Plan 引擎配置。
/// QualityConfig 覆盖 PlanConfig 的 gate 开关 + 重试次数 + Spec 规则。
fn build_plan_config(quality: Option<&crate::config::QualityConfig>) -> PlanConfig {
    let mut config = PlanConfig::default();

    if let Some(q) = quality {
        config.gate_enabled = q.gate_enabled;
        config.default_max_retries = q.default_max_retries;
        config.spec_config = Some(SpecConfig {
            require_no_error: q.require_no_error,
            require_events: q.require_events,
            require_final_message: q.require_final_message,
            require_spec: q.require_spec,
            auto_retry: q.auto_retry,
            retry_with_variant: q.retry_with_variant,
            ..Default::default()
        });
    }

    config
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_format_parsing() {
        assert_eq!(OutputFormat::from_str("json").unwrap(), OutputFormat::Json);
        assert_eq!(OutputFormat::from_str("JSON").unwrap(), OutputFormat::Json);
        assert_eq!(OutputFormat::from_str("text").unwrap(), OutputFormat::Text);
        assert_eq!(OutputFormat::from_str("silent").unwrap(), OutputFormat::Silent);
    }

    #[test]
    fn output_format_unknown() {
        assert!(OutputFormat::from_str("yaml").is_err());
    }

    #[test]
    fn exit_code_values() {
        assert_eq!(ExecExitCode::Success.as_i32(), 0);
        assert_eq!(ExecExitCode::AgentError.as_i32(), 1);
        assert_eq!(ExecExitCode::ToolError.as_i32(), 2);
        assert_eq!(ExecExitCode::Timeout.as_i32(), 3);
    }

    #[test]
    fn exec_cli_builder_defaults() {
        let cli = ExecCli::new("test".into());
        assert_eq!(cli.prompt, "test");
        assert_eq!(cli.timeout_secs, 300);
        assert_eq!(cli.output_format, OutputFormat::Text);
        assert!(cli.model.is_none());
        assert!(cli.cwd.is_none());
    }

    #[test]
    fn exec_cli_builder_chaining() {
        let cli = ExecCli::new("test".into())
            .model("qwen".into())
            .timeout_secs(60)
            .output_format(OutputFormat::Json)
            .cwd(PathBuf::from("/tmp"));

        assert_eq!(cli.model.as_deref(), Some("qwen"));
        assert_eq!(cli.timeout_secs, 60);
        assert_eq!(cli.output_format, OutputFormat::Json);
        assert_eq!(cli.cwd.as_deref(), Some(PathBuf::from("/tmp").as_path()));
    }
}
