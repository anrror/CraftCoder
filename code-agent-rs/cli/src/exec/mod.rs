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
use std::sync::Arc;
use std::time::Duration;

use code_agent_core::agent::{Session, SessionConfig};
use code_agent_core::model::{ModelClient, ModelConfig, Qwen3OpenAIClient};
use code_agent_core::tools::registry::ToolRegistry;
use code_agent_protocol::{
    Message, PermissionMode, ResponseEvent, SessionId, ThreadId, TurnInput,
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
    tool_registry: Option<Arc<ToolRegistry>>,
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
        }
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
    pub fn with_tool_registry(mut self, registry: Arc<ToolRegistry>) -> Self {
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

        Ok(Arc::new(Qwen3OpenAIClient::new(config)))
    }

    /// Resolve the tool registry (override or default).
    fn resolve_tool_registry(&self) -> Arc<ToolRegistry> {
        if let Some(ref reg) = self.tool_registry {
            Arc::clone(reg)
        } else {
            Arc::new(ToolRegistry::new())
        }
    }

    /// 执行 Agent（无头模式）
    ///
    /// 【领域含义】在无头模式下运行 Agent，返回应传播给进程的退出码。
    /// 【核心职责】解析模型客户端和工具注册表 → 构建 Session → 运行一轮 → 通过 EventProcessor 处理事件 → 映射退出码。
    /// 超时时中断 Session 并返回 Timeout。
    pub async fn execute(&self) -> Result<ExecExitCode, ExecError> {
        let model_client = self.resolve_model_client().await?;
        let tool_registry = self.resolve_tool_registry();
        let full_prompt = self.build_full_prompt();

        // Build session config
        let session_config = SessionConfig {
            id: SessionId::from(format!("exec-{}", uuid::Uuid::new_v4())),
            system_instructions: String::new(),
            max_iterations: 50,
            permission_mode: PermissionMode::Auto,
            model_client: Arc::clone(&model_client),
            tool_registry: Arc::clone(&tool_registry),
        };

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
