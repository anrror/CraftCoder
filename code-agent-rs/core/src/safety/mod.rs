//! 提示注入防御与内容安全层
//!
//! 本模块提供针对 AI 编码智能体的主动防御，防止提示注入（Prompt Injection）、
//! 代码注入（Code Injection）和角色混淆（Role Confusion）攻击。
//!
//! 【领域含义】内容安全层属于安全防护领域（Safety & Security Context）的核心
//! 模块，采用「快速本地正则 + 深度 LLM 检测」的两阶段防御架构，平衡性能与准确率。
//!
//! Prompt injection defense and content safety layer.
//!
//! This module provides proactive defense against prompt injection,
//! code injection, and role-confusion attacks that target AI coding agents.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────┐
//! │         ContentSafetyLayer               │
//! │  ┌──────────────────┐  ┌──────────────┐  │
//! │  │  Regex Pre-filter │→│ Qwen3Guard   │  │
//! │  │  (fast, local)    │  │ (LLM, API)   │  │
//! │  └──────────────────┘  └──────────────┘  │
//! │                    ↓                      │
//! │              SafetyVerdict                │
//! └──────────────────────────────────────────┘
//! ```
//!
//! The safety layer runs **after** tool execution and **before** the tool
//! result enters the model context. It also validates user input before it
//! reaches the agent loop.
//!
//! # Modes
//!
//! | Mode    | Behavior                                                   |
//! |---------|------------------------------------------------------------|
//! | Off   | No blocking; content passes through with audit logging     |
//! | Warn  | Logs warnings for suspicious content; passes through       |
//! | Block | Rejects dangerous content; returns sanitized or error      |

pub mod content_safety;
pub mod guard_client;

use serde::{Deserialize, Serialize};

pub use content_safety::ContentSafetyLayer;
pub use guard_client::Qwen3GuardClient;

// ---------------------------------------------------------------------------
// Safety context
// ---------------------------------------------------------------------------

/// 安全检查上下文 —— 描述待检查内容的来源和生产环境
///
/// 【领域含义】SafetyContext 是安全检查的值对象（Value Object），记录内容
/// 的来源信息（来自哪个工具、所属会话），用于审计日志和问题溯源。
///
/// 【核心职责】
/// - 标识产生内容的工具名称
/// - 携带会话标识符用于审计跟踪
///
/// Context for a safety check: which tool produced the content, and in what
/// session.
#[derive(Clone, Debug, Default)]
pub struct SafetyContext {
    /// The name of the tool that produced the content being checked
    /// (e.g., "bash", "read_file"). None for direct user input.
    pub tool_name: Option<String>,

    /// Optional session identifier for audit trail.
    pub session_id: Option<String>,
}

impl SafetyContext {
    /// 创建工具产出物的安全检查上下文
    ///
    /// 【领域行为】构造一个标识内容来自指定工具输出的上下文。
    /// Create a context for a tool result.
    pub fn for_tool(tool_name: impl Into<String>) -> Self {
        Self {
            tool_name: Some(tool_name.into()),
            session_id: None,
        }
    }

    /// 创建用户输入的检查上下文
    ///
    /// 【领域行为】构造一个标识内容来自用户直接输入的上下文，tool_name 为 None。
    /// Create a context for direct user input.
    pub fn for_user_input() -> Self {
        Self {
            tool_name: None,
            session_id: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Risk level
// ---------------------------------------------------------------------------

/// 风险等级 —— 安全系统对内容威胁程度的分类
///
/// 【领域含义】RiskLevel 是安全领域中的枚举值对象，定义内容的三个威胁等级。
/// 安全系统的各组件基于此等级做出不同的处置决策（记录、警告、拦截、清洗）。
///
/// 【核心职责】
/// - Safe：正常安全内容，无任何可疑模式
/// - Suspicious：存在可疑模式但无法确定恶意意图
/// - Dangerous：确认包含提示注入、代码注入等明确攻击
///
/// Risk level assigned by the safety system.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// Content is safe — no suspicious patterns detected.
    Safe,

    /// Content has suspicious patterns but is not definitively malicious.
    Suspicious,

    /// Content contains clear prompt injection or code injection.
    Dangerous,
}

impl RiskLevel {
    /// 从字符串解析风险等级（不区分大小写）
    ///
    /// 【领域行为】将字符串形式的等级描述解析为 RiskLevel 枚举。未知字符串
    /// 保守默认返回 Suspicious。
    /// Parse from a string (case-insensitive).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "safe" => RiskLevel::Safe,
            "suspicious" => RiskLevel::Suspicious,
            "dangerous" => RiskLevel::Dangerous,
            _ => RiskLevel::Suspicious, // conservative default
        }
    }

    /// 判断是否为危险等级
    ///
    /// 【领域行为】返回当前等级是否为 Dangerous。
    /// True if the risk level is Dangerous.
    pub fn is_dangerous(&self) -> bool {
        matches!(self, RiskLevel::Dangerous)
    }
}

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RiskLevel::Safe => write!(f, "safe"),
            RiskLevel::Suspicious => write!(f, "suspicious"),
            RiskLevel::Dangerous => write!(f, "dangerous"),
        }
    }
}

// ---------------------------------------------------------------------------
// Safety verdict
// ---------------------------------------------------------------------------

/// 安全判决书 —— 安全检查的结果报告
///
/// 【领域含义】SafetyVerdict 是安全检查领域中的结果值对象，聚合了安全检测
/// 的所有结论信息：是否通过、风险等级、检测到的风险类别、清洗后内容和判决理由。
///
/// 【核心职责】
/// - 封装安全检测的完整结果
/// - 提供多种构造工厂方法（safe / dangerous / suspicious / sanitized）
/// - 作为 Qwen3GuardClient 和 ContentSafetyLayer 之间的统一返回值
///
/// The result of a safety check.
///
/// Returned by [Qwen3GuardClient::check] and embedded in the output of
/// [ContentSafetyLayer::sanitize] and [ContentSafetyLayer::check_input].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SafetyVerdict {
    /// True if the content passed all safety checks.
    pub is_safe: bool,

    /// The overall risk level for this content.
    pub risk_level: RiskLevel,

    /// Specific categories of risk detected (e.g., "prompt_injection",
    /// "code_injection", "role_confusion").
    #[serde(default)]
    pub risk_categories: Vec<String>,

    /// Sanitized version of the content if the original was modified.
    /// None means the original content was either safe or blocked entirely.
    #[serde(default)]
    pub sanitized_content: Option<String>,

    /// Human-readable explanation of the verdict.
    #[serde(default)]
    pub reason: Option<String>,
}

impl SafetyVerdict {
    /// 构造「安全」判决（快速路径 —— 无需 API 调用）
    ///
    /// 【领域行为】创建一个表示内容安全无威胁的判决，用于正则预过滤通过后
    /// 或安全模式为 Off 时的快速返回。
    /// Create a safe verdict (fast path — no API call needed).
    pub fn safe() -> Self {
        Self {
            is_safe: true,
            risk_level: RiskLevel::Safe,
            risk_categories: vec![],
            sanitized_content: None,
            reason: Some("No threat detected".into()),
        }
    }

    /// 构造「危险」判决（正则检测快速路径）
    ///
    /// 【领域行为】创建一个表示内容危险需要拦截的判决，用于正则预过滤直接
    /// 检测到明确的注入攻击模式。
    /// Create a dangerous verdict from regex-only detection.
    pub fn dangerous(reason: impl Into<String>, categories: Vec<String>) -> Self {
        Self {
            is_safe: false,
            risk_level: RiskLevel::Dangerous,
            risk_categories: categories,
            sanitized_content: None,
            reason: Some(reason.into()),
        }
    }

    /// 构造「可疑」判决
    ///
    /// 【领域行为】创建一个表示内容可疑但无法确定恶意意图的判决，用于正则
    /// 检测到可疑模式或守护模型返回不确定结果时。
    /// Create a suspicious verdict.
    pub fn suspicious(reason: impl Into<String>, categories: Vec<String>) -> Self {
        Self {
            is_safe: false,
            risk_level: RiskLevel::Suspicious,
            risk_categories: categories,
            sanitized_content: None,
            reason: Some(reason.into()),
        }
    }

    /// 构造「已清洗且安全」的判决
    ///
    /// 【领域行为】创建一个表示原内容经过清洗后安全的判决，内容已被修改
    /// （如关键敏感词被替换），但修改后内容可以安全传递给模型。
    /// Create a safe verdict with sanitized content.
    pub fn sanitized(cleaned: String, reason: impl Into<String>) -> Self {
        Self {
            is_safe: true,
            risk_level: RiskLevel::Safe,
            risk_categories: vec![],
            sanitized_content: Some(cleaned),
            reason: Some(reason.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Safety mode
// ---------------------------------------------------------------------------

/// 安全模式 —— 控制安全层对检测到的威胁的响应策略
///
/// 【领域含义】SafetyMode 是安全领域的策略枚举，定义了安全系统在检测到
/// 不同威胁等级时应采取的处置方式。对应三种典型策略：放行、仅警告、拦截。
///
/// 【核心职责】
/// - Off：不拦截，仅审计日志记录
/// - Warn：可疑内容放行并记录警告，危险内容拦截
/// - Block：所有可疑或危险内容均拦截
///
/// Controls how the safety layer responds to detected threats.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyMode {
    /// No blocking — content passes through with audit logging only.
    Off,

    /// Suspicious content triggers a warning log but still passes through.
    /// Dangerous content is blocked.
    Warn,

    /// All suspicious or dangerous content is blocked.
    Block,
}

// ---------------------------------------------------------------------------
// Safety configuration
// ---------------------------------------------------------------------------

/// 安全配置聚合 —— 内容安全层的全部可配置参数
///
/// 【领域含义】SafetyConfig 是安全领域的配置聚合（Configuration Aggregate），
/// 封装了安全层的运行策略参数：安全模式、内容长度限制、守护模型 API 连接参数等。
/// 通过 Builder 模式构造，提供默认值保障。
///
/// 【核心职责】
/// - 定义安全层的运行模式（Off / Warn / Block）
/// - 配置守护模型的 API 端点、密钥和模型名称
/// - 设置内容长度上限以控制 API 调用成本
///
/// Configuration for the content safety layer.
#[derive(Clone, Debug)]
pub struct SafetyConfig {
    /// The safety enforcement mode.
    pub mode: SafetyMode,

    /// Maximum content length (in characters) to send to the guard model.
    /// Content longer than this is truncated before the guard check.
    /// Default: 100_000
    pub max_content_length: usize,

    /// Base URL for the OpenAI-compatible API gateway.
    pub api_base_url: String,

    /// API key for authentication.
    pub api_key: String,

    /// Guard model name.
    pub guard_model: String,
}

impl SafetyConfig {
    /// 创建 SafetyConfig 构建器
    ///
    /// 【领域行为】返回一个默认配置的 Builder，允许调用方链式设置参数后构建。
    /// Create a builder for [SafetyConfig].
    pub fn builder() -> SafetyConfigBuilder {
        SafetyConfigBuilder::default()
    }
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            mode: SafetyMode::Block,
            max_content_length: 100_000,
            api_base_url: String::new(),
            api_key: String::new(),
            guard_model: String::new(),
        }
    }
}

/// SafetyConfig 构建器 —— 遵循 Builder 模式实现参数化构造
///
/// 【领域含义】提供链式调用 API，允许调用方仅设置需要自定义的字段，
/// 未设置的字段使用 SafetyConfig::default() 中的默认值。
/// Builder for [SafetyConfig].
#[derive(Default)]
pub struct SafetyConfigBuilder {
    mode: Option<SafetyMode>,
    max_content_length: Option<usize>,
    api_base_url: Option<String>,
    api_key: Option<String>,
    guard_model: Option<String>,
}

impl SafetyConfigBuilder {
    /// 设置安全模式
    ///
    /// 【领域行为】指定安全层的运行模式（Off / Warn / Block）。
    /// Set the safety mode.
    pub fn mode(mut self, mode: SafetyMode) -> Self {
        self.mode = Some(mode);
        self
    }

    /// 设置守护模型检查的最大内容长度（字符数）
    ///
    /// 【领域行为】超出此长度的内容在发送给守护模型前会被截断。
    /// Set the max content length for guard checks.
    pub fn max_content_length(mut self, len: usize) -> Self {
        self.max_content_length = Some(len);
        self
    }

    /// 设置 API 基础 URL
    ///
    /// 【领域行为】指定兼容 OpenAI API 的网关地址。
    /// Set the API base URL.
    pub fn api_base_url(mut self, url: String) -> Self {
        self.api_base_url = Some(url);
        self
    }

    /// 设置 API 密钥
    ///
    /// 【领域行为】设置用于 API 认证的 Bearer Token。
    /// Set the API key.
    pub fn api_key(mut self, key: String) -> Self {
        self.api_key = Some(key);
        self
    }

    /// 设置守护模型名称
    ///
    /// 【领域行为】指定用于内容安全检测的模型标识符。
    /// Set the guard model name.
    pub fn guard_model(mut self, model: String) -> Self {
        self.guard_model = Some(model);
        self
    }

    /// 构建 SafetyConfig 实例
    ///
    /// 【领域行为】将 Builder 中设置的参数合并默认值，构造最终的 SafetyConfig。
    /// 未显式设置的字段将使用 SafetyConfig::default() 中的默认值。
    /// Build the [SafetyConfig].
    pub fn build(self) -> SafetyConfig {
        let defaults = SafetyConfig::default();
        SafetyConfig {
            mode: self.mode.unwrap_or(defaults.mode),
            max_content_length: self.max_content_length.unwrap_or(defaults.max_content_length),
            api_base_url: self.api_base_url.unwrap_or(defaults.api_base_url),
            api_key: self.api_key.unwrap_or(defaults.api_key),
            guard_model: self.guard_model.unwrap_or(defaults.guard_model),
        }
    }
}

// ---------------------------------------------------------------------------
// Sanitized content result
// ---------------------------------------------------------------------------

/// 清洗内容结果 —— 工具输出经过安全层处理后的产物
///
/// 【领域含义】SanitizedContent 是安全处理过程的结果值对象，包含经过（或
/// 未经过）安全处理的最终内容及其安全判决书，以及内容是否被修改的标识。
///
/// 【核心职责】
/// - 封装安全处理后的最终内容
/// - 携带对应的安全判决元数据
/// - 标记内容是否被修改以通知上层调用方
///
/// Result of sanitizing tool output through the safety layer.
#[derive(Clone, Debug)]
pub struct SanitizedContent {
    /// The (possibly modified) content that is safe to pass to the model.
    pub content: String,

    /// The safety verdict for this content.
    pub verdict: SafetyVerdict,

    /// Whether the content was modified by the safety layer.
    pub was_modified: bool,
}

// ---------------------------------------------------------------------------
// Input verdict
// ---------------------------------------------------------------------------

/// 用户输入判决 —— 用户消息经过安全检查的结果
///
/// 【领域含义】InputVerdict 是用户输入安全检查的结果值对象，用于用户消息
/// 进入智能体循环前的前置过滤决策。
///
/// 【核心职责】
/// - 判断用户输入是否允许进入智能体循环
/// - 携带安全判决书用于审计
/// - 提供被拒绝时的用户友好提示信息
///
/// Result of checking user input for injection attempts.
#[derive(Clone, Debug)]
pub struct InputVerdict {
    /// Whether the input is allowed to proceed.
    pub allowed: bool,

    /// The safety verdict for this input.
    pub verdict: SafetyVerdict,

    /// If not allowed, a user-facing message explaining why.
    pub block_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Safety error
// ---------------------------------------------------------------------------

/// 安全层错误 —— 安全检查过程中可能发生的各类异常
///
/// 【领域含义】SafetyError 是安全领域的错误类型聚合，封装了安全检测
/// 全流程中可能出现的错误：网络传输错误、序列化错误、API 错误、
/// 响应格式错误和配置错误。
///
/// Errors that can occur during safety checks.
#[derive(Debug, thiserror::Error)]
pub enum SafetyError {
    /// Transport-level error calling the guard API.
    #[error("Guard API HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// JSON serialization or deserialization error.
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// The guard API returned an error status.
    #[error("Guard API error {status}: {message}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Error message from the API.
        message: String,
    },

    /// The guard model response could not be parsed as a valid verdict.
    #[error("Invalid guard response: {0}")]
    InvalidResponse(String),

    /// Configuration error.
    #[error("Configuration error: {0}")]
    Config(String),
}

/// Convenience type alias for safety operation results.
pub type SafetyResult<T> = Result<T, SafetyError>;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── SafetyContext ──

    #[test]
    fn safety_context_for_tool() {
        let ctx = SafetyContext::for_tool("bash");
        assert_eq!(ctx.tool_name.as_deref(), Some("bash"));
        assert!(ctx.session_id.is_none());
    }

    #[test]
    fn safety_context_for_user_input() {
        let ctx = SafetyContext::for_user_input();
        assert!(ctx.tool_name.is_none());
    }

    // ── RiskLevel ──

    #[test]
    fn risk_level_parse() {
        assert_eq!(RiskLevel::from_str("Safe"), RiskLevel::Safe);
        assert_eq!(RiskLevel::from_str("SAFE"), RiskLevel::Safe);
        assert_eq!(RiskLevel::from_str("suspicious"), RiskLevel::Suspicious);
        assert_eq!(RiskLevel::from_str("DANGEROUS"), RiskLevel::Dangerous);
        assert_eq!(RiskLevel::from_str("unknown"), RiskLevel::Suspicious);
    }

    #[test]
    fn risk_level_is_dangerous() {
        assert!(!RiskLevel::Safe.is_dangerous());
        assert!(!RiskLevel::Suspicious.is_dangerous());
        assert!(RiskLevel::Dangerous.is_dangerous());
    }

    #[test]
    fn risk_level_display() {
        assert_eq!(RiskLevel::Safe.to_string(), "safe");
        assert_eq!(RiskLevel::Suspicious.to_string(), "suspicious");
        assert_eq!(RiskLevel::Dangerous.to_string(), "dangerous");
    }

    #[test]
    fn risk_level_serde_round_trip() {
        for level in [RiskLevel::Safe, RiskLevel::Suspicious, RiskLevel::Dangerous] {
            let json = serde_json::to_string(&level).expect("serialize");
            let parsed: RiskLevel = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(level, parsed);
        }
    }

    // ── SafetyVerdict ──

    #[test]
    fn verdict_safe_defaults() {
        let v = SafetyVerdict::safe();
        assert!(v.is_safe);
        assert_eq!(v.risk_level, RiskLevel::Safe);
        assert!(v.risk_categories.is_empty());
    }

    #[test]
    fn verdict_dangerous() {
        let v = SafetyVerdict::dangerous("Injection detected", vec!["prompt_injection".into()]);
        assert!(!v.is_safe);
        assert_eq!(v.risk_level, RiskLevel::Dangerous);
        assert!(v.risk_categories.contains(&"prompt_injection".to_string()));
    }

    #[test]
    fn verdict_serde_round_trip() {
        let v = SafetyVerdict {
            is_safe: false,
            risk_level: RiskLevel::Dangerous,
            risk_categories: vec!["prompt_injection".into()],
            sanitized_content: None,
            reason: Some("Blocked".into()),
        };
        let json = serde_json::to_string(&v).expect("serialize");
        let parsed: SafetyVerdict = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.is_safe, false);
        assert_eq!(parsed.risk_level, RiskLevel::Dangerous);
        assert_eq!(parsed.reason.as_deref(), Some("Blocked"));
    }

    #[test]
    fn verdict_deserialize_minimal() {
        let json = r#"{"is_safe":true,"risk_level":"safe"}"#;
        let v: SafetyVerdict = serde_json::from_str(json).expect("deserialize");
        assert!(v.is_safe);
        assert!(v.risk_categories.is_empty());
        assert!(v.sanitized_content.is_none());
    }

    // ── SafetyConfig ──

    #[test]
    fn safety_config_defaults() {
        let config = SafetyConfig::default();
        assert_eq!(config.mode, SafetyMode::Block);
        assert_eq!(config.max_content_length, 100_000);
        assert!(config.guard_model.is_empty());
    }

    #[test]
    fn safety_config_builder() {
        let config = SafetyConfig::builder()
            .mode(SafetyMode::Warn)
            .max_content_length(50_000)
            .api_key("sk-test".into())
            .guard_model("my-guard".into())
            .build();
        assert_eq!(config.mode, SafetyMode::Warn);
        assert_eq!(config.max_content_length, 50_000);
        assert_eq!(config.api_key, "sk-test");
        assert_eq!(config.guard_model, "my-guard");
    }

    #[test]
    fn safety_config_builder_defaults_fill_in() {
        let config = SafetyConfig::builder()
            .api_key("sk-min".into())
            .build();
        assert_eq!(config.mode, SafetyMode::Block); // default
        assert_eq!(config.max_content_length, 100_000); // default
    }

    // ── SafetyMode serde ──

    #[test]
    fn safety_mode_serde() {
        for mode in [SafetyMode::Off, SafetyMode::Warn, SafetyMode::Block] {
            let json = serde_json::to_string(&mode).expect("serialize");
            let parsed: SafetyMode = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(mode, parsed);
        }
    }

    #[test]
    fn safety_mode_json_values() {
        let off = serde_json::to_string(&SafetyMode::Off).expect("serialize");
        let block = serde_json::to_string(&SafetyMode::Block).expect("serialize");
        assert_eq!(off, "\"off\"");
        assert_eq!(block, "\"block\"");
    }

    // ── SafetyError display ──

    #[test]
    fn safety_error_display() {
        let err = SafetyError::Api {
            status: 500,
            message: "Internal error".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("500"));
        assert!(msg.contains("Internal error"));
    }

    #[test]
    fn safety_error_from_serde() {
        let err: SafetyError = serde_json::from_str::<serde_json::Value>("bad")
            .unwrap_err()
            .into();
        assert!(matches!(err, SafetyError::Serde(_)));
    }

    #[test]
    fn safety_error_config() {
        let err = SafetyError::Config("missing key".into());
        assert!(err.to_string().contains("missing key"));
    }

    #[test]
    fn safety_error_invalid_response() {
        let err = SafetyError::InvalidResponse("bad json".into());
        assert!(err.to_string().contains("bad json"));
    }
}
