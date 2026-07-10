//! Configuration management with TOML layered resolution.
//!
//! Resolves configuration from four layers (highest priority wins):
//!
//! 1. **CLI flags** — passed directly by the binary via [`ConfigBuilder`]
//! 2. **Project-local** — `.code-agent/config.toml` in the current directory
//! 3. **Global / user** — `~/.config/code-agent/config.toml`
//! 4. **Builtin defaults** — hard-coded in [`Config::default`]
//!
//! # Usage
//!
//! ```rust,ignore
//! use code_agent_cli::config::{Config, ConfigBuilder, ConfigManager};
//!
//! let config = ConfigBuilder::default()
//!     .model("qwen3.6-27b".into())
//!     .timeout_secs(120)
//!     .build()?;
//! ```

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// AgentConfig — core configuration fields
// ---------------------------------------------------------------------------

/// Agent 配置（可选字段）
///
/// 【领域含义】从 TOML 文件加载或通过 CLI 设置的 Agent 核心配置，所有字段使用 Option 以支持分层合并。
/// 【核心职责】作为配置合并的中间表示，区分"已显式设置"和"未设置"两种状态。
/// 最终通过 Self::finalize 解析为完整的 Config。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentConfig {
    /// 模型名称（如 "qwen3.6-27b"、"gpt-4o"）。
    pub model: Option<String>,

    /// API 密钥 — 模型提供商的认证密钥。
    pub api_key: Option<String>,

    /// API 基础 URL — 模型提供商的 API 地址。
    pub api_base_url: Option<String>,

    /// 执行超时（秒）
    pub timeout_secs: Option<u64>,

    /// 每轮最大 Agent 迭代次数
    pub max_iterations: Option<u32>,

    /// 权限模式 — "auto"（自动）、"ask"（询问）或 "deny"（拒绝）。
    pub permission_mode: Option<String>,

    /// 输出格式 — 无头执行模式："text"、"json" 或 "silent"。
    pub output_format: Option<String>,

    /// 工作目录覆盖
    pub work_dir: Option<PathBuf>,

    /// 缓存目录 — 评估结果/数据集的存放目录。
    pub cache_dir: Option<PathBuf>,

    /// 系统指令 — 注入到每个会话的系统提示。
    pub system_instructions: Option<String>,

    /// 工具白名单 — 如果设置，仅注册这些工具。
    pub tool_allowlist: Option<Vec<String>>,
}

/// 完整配置
///
/// 【领域含义】所有字段均已解析的完整配置，由 AgentConfig::finalize 在合并所有层后生成。
/// 【核心职责】作为 CLI 和 Agent 运行时的最终配置来源，所有字段均为非 Option 的具体值。
#[derive(Clone, Debug)]
pub struct Config {
    /// 模型名称
    pub model: String,

    /// API 密钥
    pub api_key: String,

    /// API 基础 URL
    pub api_base_url: String,

    /// 执行超时（秒）
    pub timeout_secs: u64,

    /// 每轮最大迭代次数
    pub max_iterations: u32,

    /// 权限模式
    pub permission_mode: String,

    /// 输出格式
    pub output_format: String,

    /// 工作目录
    pub work_dir: PathBuf,

    /// 缓存目录
    pub cache_dir: PathBuf,

    /// 系统指令
    pub system_instructions: String,

    /// 工具白名单
    pub tool_allowlist: Vec<String>,
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

impl Default for Config {
    fn default() -> Self {
        Self {
            model: "qwen3.6-27b".into(),
            api_key: String::new(),
            api_base_url: "https://api.openai.com/v1".into(),
            timeout_secs: 300,
            max_iterations: 50,
            permission_mode: "auto".into(),
            output_format: "text".into(),
            work_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            cache_dir: PathBuf::from(".code-agent/cache"),
            system_instructions: String::new(),
            tool_allowlist: Vec::new(),
        }
    }
}

impl AgentConfig {
    /// 解析为完整配置
    ///
    /// 【领域含义】将可选字段的 AgentConfig 解析为所有字段均有值的完整 Config。
    /// 【核心职责】用 Config::default() 的默认值填充所有 None 字段。
    pub fn finalize(&self) -> Config {
        let defaults = Config::default();
        Config {
            model: self.model.clone().unwrap_or(defaults.model),
            api_key: self.api_key.clone().unwrap_or(defaults.api_key),
            api_base_url: self.api_base_url.clone().unwrap_or(defaults.api_base_url),
            timeout_secs: self.timeout_secs.unwrap_or(defaults.timeout_secs),
            max_iterations: self.max_iterations.unwrap_or(defaults.max_iterations),
            permission_mode: self
                .permission_mode
                .clone()
                .unwrap_or(defaults.permission_mode),
            output_format: self
                .output_format
                .clone()
                .unwrap_or(defaults.output_format),
            work_dir: self.work_dir.clone().unwrap_or(defaults.work_dir),
            cache_dir: self.cache_dir.clone().unwrap_or(defaults.cache_dir),
            system_instructions: self
                .system_instructions
                .clone()
                .unwrap_or(defaults.system_instructions),
            tool_allowlist: self.tool_allowlist.clone().unwrap_or(defaults.tool_allowlist),
        }
    }
}

// ---------------------------------------------------------------------------
// ConfigBuilder
// ---------------------------------------------------------------------------

/// 配置构建器
///
/// 【领域含义】从多个层组装配置的构建器，支持分层合并和 CLI 覆盖。
/// 【核心职责】按优先级顺序合并配置层（内置默认值 → 全局配置 → 项目配置 → CLI 参数）。
///
/// # 层顺序（后应用者优先）
/// ```text
/// ConfigBuilder::default()          ← 内置默认值
///   .merge(global_toml)?            ← ~/.config/code-agent/config.toml
///   .merge(project_toml)?           ← .code-agent/config.toml
///   .model("custom-model")          ← CLI 参数覆盖
///   .build()?                       ← 解析为完整 Config
/// ```
#[derive(Clone, Debug, Default)]
pub struct ConfigBuilder {
    inner: AgentConfig,
}

impl ConfigBuilder {
    /// 创建构建器
    ///
    /// 【领域含义】创建以内置默认值初始化的配置构建器。
    /// 【核心职责】返回默认配置构建器实例。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置模型名称
    ///
    /// 【领域含义】覆盖模型名称配置。
    /// 【核心职责】设置 model 字段，供构建器链式调用。
    pub fn model(mut self, v: String) -> Self {
        self.inner.model = Some(v);
        self
    }

    /// 设置 API 密钥
    ///
    /// 【领域含义】覆盖 API 密钥配置。
    /// 【核心职责】设置 api_key 字段。
    pub fn api_key(mut self, v: String) -> Self {
        self.inner.api_key = Some(v);
        self
    }

    /// 设置 API 基础 URL
    ///
    /// 【领域含义】覆盖 API 基础 URL 配置。
    /// 【核心职责】设置 api_base_url 字段。
    pub fn api_base_url(mut self, v: String) -> Self {
        self.inner.api_base_url = Some(v);
        self
    }

    /// 设置超时时间（秒）
    ///
    /// 【领域含义】覆盖执行超时配置。
    /// 【核心职责】设置 timeout_secs 字段。
    pub fn timeout_secs(mut self, v: u64) -> Self {
        self.inner.timeout_secs = Some(v);
        self
    }

    /// 设置最大迭代次数
    ///
    /// 【领域含义】覆盖每轮最大 Agent 迭代次数配置。
    /// 【核心职责】设置 max_iterations 字段。
    pub fn max_iterations(mut self, v: u32) -> Self {
        self.inner.max_iterations = Some(v);
        self
    }

    /// 设置权限模式
    ///
    /// 【领域含义】覆盖权限模式配置。
    /// 【核心职责】设置 permission_mode 字段。
    pub fn permission_mode(mut self, v: String) -> Self {
        self.inner.permission_mode = Some(v);
        self
    }

    /// 设置输出格式
    ///
    /// 【领域含义】覆盖无头执行模式的输出格式配置。
    /// 【核心职责】设置 output_format 字段。
    pub fn output_format(mut self, v: String) -> Self {
        self.inner.output_format = Some(v);
        self
    }

    /// 设置工作目录
    ///
    /// 【领域含义】覆盖工作目录配置。
    /// 【核心职责】设置 work_dir 字段。
    pub fn work_dir(mut self, v: PathBuf) -> Self {
        self.inner.work_dir = Some(v);
        self
    }

    /// 设置缓存目录
    ///
    /// 【领域含义】覆盖缓存目录配置。
    /// 【核心职责】设置 cache_dir 字段。
    pub fn cache_dir(mut self, v: PathBuf) -> Self {
        self.inner.cache_dir = Some(v);
        self
    }

    /// 设置系统指令
    ///
    /// 【领域含义】覆盖系统指令配置。
    /// 【核心职责】设置 system_instructions 字段。
    pub fn system_instructions(mut self, v: String) -> Self {
        self.inner.system_instructions = Some(v);
        self
    }

    /// 设置工具白名单
    ///
    /// 【领域含义】覆盖工具白名单配置。
    /// 【核心职责】设置 tool_allowlist 字段。
    pub fn tool_allowlist(mut self, v: Vec<String>) -> Self {
        self.inner.tool_allowlist = Some(v);
        self
    }

    /// 合并配置层
    ///
    /// 【领域含义】将另一个 AgentConfig 层合并到当前构建器中。
    /// 【核心职责】仅覆盖 other 中为 Some 的字段，已有值被覆盖（后合并者优先）。
    pub fn merge(mut self, other: &AgentConfig) -> Self {
        if let Some(v) = &other.model {
            self.inner.model = Some(v.clone());
        }
        if let Some(v) = &other.api_key {
            self.inner.api_key = Some(v.clone());
        }
        if let Some(v) = &other.api_base_url {
            self.inner.api_base_url = Some(v.clone());
        }
        if let Some(v) = other.timeout_secs {
            self.inner.timeout_secs = Some(v);
        }
        if let Some(v) = other.max_iterations {
            self.inner.max_iterations = Some(v);
        }
        if let Some(v) = &other.permission_mode {
            self.inner.permission_mode = Some(v.clone());
        }
        if let Some(v) = &other.output_format {
            self.inner.output_format = Some(v.clone());
        }
        if let Some(v) = &other.work_dir {
            self.inner.work_dir = Some(v.clone());
        }
        if let Some(v) = &other.cache_dir {
            self.inner.cache_dir = Some(v.clone());
        }
        if let Some(v) = &other.system_instructions {
            self.inner.system_instructions = Some(v.clone());
        }
        if let Some(v) = &other.tool_allowlist {
            self.inner.tool_allowlist = Some(v.clone());
        }
        self
    }

    /// 构建完整配置
    ///
    /// 【领域含义】将构建器解析为所有字段均有值的完整 Config。
    /// 【核心职责】调用 inner.finalize() 填充所有默认值。
    pub fn build(&self) -> Config {
        self.inner.finalize()
    }
}

// ---------------------------------------------------------------------------
// ConfigManager
// ---------------------------------------------------------------------------

/// 配置管理器
///
/// 【领域含义】管理 TOML 配置的加载、解析和保存，提供分层配置解析能力。
/// 【核心职责】提供全局配置和项目配置的加载/保存接口，支持分层合并。
///
/// 分层解析：
/// - `load_global()` — 从 `~/.config/code-agent/config.toml` 加载
/// - `load_project(cwd)` — 从 `.code-agent/config.toml` 加载
/// - `load_all()` — 合并两者到 ConfigBuilder
#[derive(Debug)]
pub struct ConfigManager;

impl ConfigManager {
    // ── path helpers ──────────────────────────────────────────────

    /// 获取全局配置路径
    ///
    /// 【领域含义】返回全局配置文件的路径：`~/.config/code-agent/config.toml`。
    /// 【核心职责】使用 dirs crate 确定用户配置目录。
    pub fn global_config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("code-agent").join("config.toml"))
    }

    /// 获取项目配置路径
    ///
    /// 【领域含义】返回项目本地配置文件的路径：`<cwd>/.code-agent/config.toml`。
    /// 【核心职责】基于当前工作目录构造项目配置路径。
    pub fn project_config_path(cwd: &Path) -> PathBuf {
        cwd.join(".code-agent").join("config.toml")
    }

    // ── load single layer ───────────────────────────────────────

    /// 加载全局配置
    ///
    /// 【领域含义】从 `~/.config/code-agent/config.toml` 加载全局/用户 TOML 配置。
    /// 【核心职责】读取并解析全局配置文件，文件不存在时返回 None。
    pub fn load_global() -> Option<AgentConfig> {
        let path = Self::global_config_path()?;
        Self::read_toml(&path)
    }

    /// 加载项目配置
    ///
    /// 【领域含义】从指定工作目录下的 `.code-agent/config.toml` 加载项目本地配置。
    /// 【核心职责】读取并解析项目配置文件，文件不存在时返回 None。
    pub fn load_project(cwd: &Path) -> Option<AgentConfig> {
        let path = Self::project_config_path(cwd);
        Self::read_toml(&path)
    }

    // ── load all layers ─────────────────────────────────────────

    /// 加载所有配置层
    ///
    /// 【领域含义】加载所有配置层并返回预合并的构建器。
    /// 【核心职责】按优先级顺序合并：内置默认值 → 全局配置 → 项目配置。
    /// 调用方可在此基础上应用 CLI 参数覆盖。
    pub fn load_all(cwd: &Path) -> ConfigBuilder {
        let mut builder = ConfigBuilder::new();

        if let Some(global) = Self::load_global() {
            builder = builder.merge(&global);
        }

        if let Some(project) = Self::load_project(cwd) {
            builder = builder.merge(&project);
        }

        builder
    }

    // ── save ────────────────────────────────────────────────────

    /// 保存全局配置
    ///
    /// 【领域含义】将 AgentConfig 保存到全局配置路径。
    /// 【核心职责】创建父目录（如需要），写入 TOML 文件，返回写入的路径。
    pub fn save_global(config: &AgentConfig) -> Result<PathBuf, ConfigError> {
        let path = Self::global_config_path().ok_or(ConfigError::NoConfigDir)?;
        Self::write_toml(&path, config)?;
        Ok(path)
    }

    /// 保存项目配置
    ///
    /// 【领域含义】将 AgentConfig 保存到项目本地配置路径。
    /// 【核心职责】创建父目录（如需要），写入 TOML 文件，返回写入的路径。
    pub fn save_project(cwd: &Path, config: &AgentConfig) -> Result<PathBuf, ConfigError> {
        let path = Self::project_config_path(cwd);
        Self::write_toml(&path, config)?;
        Ok(path)
    }

    // ── internal helpers ────────────────────────────────────────

    fn read_toml(path: &PathBuf) -> Option<AgentConfig> {
        if !path.exists() {
            return None;
        }
        let content = std::fs::read_to_string(path).ok()?;
        toml::from_str(&content).ok()
    }

    fn write_toml(path: &PathBuf, config: &AgentConfig) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(ConfigError::Io)?;
        }
        let content =
            toml::to_string_pretty(config).map_err(|e| ConfigError::Deserialize(e.to_string()))?;
        std::fs::write(path, content).map_err(ConfigError::Io)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ConfigError
// ---------------------------------------------------------------------------

/// 配置错误
///
/// 【领域含义】表示配置操作过程中可能发生的领域错误。
/// 【核心职责】封装配置目录不可用、TOML 解析失败、I/O 错误等异常场景。
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// 无法确定配置目录（如在无头系统上）。
    #[error("cannot determine config directory")]
    NoConfigDir,

    /// TOML 反序列化错误。
    #[error("invalid TOML: {0}")]
    Deserialize(String),

    /// 读写配置文件时的 I/O 错误。
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Defaults ─────────────────────────────────────────────────

    #[test]
    fn defaults_are_sensible() {
        let c = Config::default();
        assert_eq!(c.model, "qwen3.6-27b");
        assert_eq!(c.timeout_secs, 300);
        assert_eq!(c.max_iterations, 50);
        assert_eq!(c.permission_mode, "auto");
        assert_eq!(c.output_format, "text");
    }

    #[test]
    fn agent_config_finalize_fills_blanks() {
        let partial = AgentConfig {
            model: Some("custom-model".into()),
            ..Default::default()
        };
        let resolved = partial.finalize();
        assert_eq!(resolved.model, "custom-model");
        assert_eq!(resolved.timeout_secs, 300); // from default
    }

    // ── Builder ──────────────────────────────────────────────────

    #[test]
    fn builder_overrides_work() {
        let config = ConfigBuilder::new()
            .model("gpt-4".into())
            .timeout_secs(120)
            .output_format("json".into())
            .build();

        assert_eq!(config.model, "gpt-4");
        assert_eq!(config.timeout_secs, 120);
        assert_eq!(config.output_format, "json");
        // Fields not set should be defaults
        assert_eq!(config.max_iterations, 50);
    }

    #[test]
    fn builder_merge_layers_correctly() {
        let global = AgentConfig {
            model: Some("global-model".into()),
            timeout_secs: Some(60),
            ..Default::default()
        };

        let project = AgentConfig {
            model: Some("project-model".into()),
            max_iterations: Some(25),
            ..Default::default()
        };

        let config = ConfigBuilder::new()
            .merge(&global)
            .merge(&project)
            .build();

        // Project overrides global for model
        assert_eq!(config.model, "project-model");
        // Global-only field retained
        assert_eq!(config.timeout_secs, 60);
        // Project-only field applied
        assert_eq!(config.max_iterations, 25);
    }

    #[test]
    fn builder_cli_wins_over_all() {
        let global = AgentConfig {
            model: Some("global-model".into()),
            timeout_secs: Some(30),
            ..Default::default()
        };

        let project = AgentConfig {
            model: Some("project-model".into()),
            timeout_secs: Some(60),
            ..Default::default()
        };

        let config = ConfigBuilder::new()
            .merge(&global)
            .merge(&project)
            .model("cli-model".into()) // CLI wins
            .build();

        assert_eq!(config.model, "cli-model");
        assert_eq!(config.timeout_secs, 60); // project overwrote global
    }

    #[test]
    fn merge_none_fields_dont_overwrite() {
        let existing = AgentConfig {
            model: Some("keep-me".into()),
            timeout_secs: Some(42),
            ..Default::default()
        };

        let overlay = AgentConfig {
            timeout_secs: Some(99),
            ..Default::default()
        };

        let config = ConfigBuilder::new()
            .merge(&existing)
            .merge(&overlay)
            .build();

        // model was not in overlay, so it's kept from existing
        assert_eq!(config.model, "keep-me");
        // timeout was in overlay, so it overwrites
        assert_eq!(config.timeout_secs, 99);
    }

    // ── Path helpers ─────────────────────────────────────────────

    #[test]
    fn project_config_path_is_relative() {
        let cwd = PathBuf::from("/home/user/project");
        let path = ConfigManager::project_config_path(&cwd);
        assert!(path.ends_with(".code-agent/config.toml"));
        assert!(path.starts_with("/home/user/project"));
    }

    // ── Save / roundtrip ─────────────────────────────────────────

    #[test]
    fn save_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().to_path_buf();

        let config = AgentConfig {
            model: Some("roundtrip-model".into()),
            timeout_secs: Some(77),
            max_iterations: Some(10),
            ..Default::default()
        };

        let saved_path = ConfigManager::save_project(&cwd, &config).unwrap();
        assert!(saved_path.exists());

        let loaded = ConfigManager::load_project(&cwd).unwrap();
        assert_eq!(loaded.model, config.model);
        assert_eq!(loaded.timeout_secs, config.timeout_secs);
        assert_eq!(loaded.max_iterations, config.max_iterations);
    }

    #[test]
    fn load_missing_file_returns_none() {
        let cwd = PathBuf::from("/tmp/nonexistent-9f3a2b1c");
        let result = ConfigManager::load_project(&cwd);
        assert!(result.is_none());
    }

    // ── TOML serialization ────────────────────────────────────────

    #[test]
    fn toml_roundtrip_preserves_fields() {
        let original = AgentConfig {
            model: Some("toml-test".into()),
            api_key: Some("sk-abc123".into()),
            timeout_secs: Some(120),
            max_iterations: Some(30),
            permission_mode: Some("auto".into()),
            output_format: Some("json".into()),
            system_instructions: Some("Be helpful.".into()),
            tool_allowlist: Some(vec!["read_file".into(), "bash".into()]),
            ..Default::default()
        };

        let serialized = toml::to_string_pretty(&original).unwrap();
        let parsed: AgentConfig = toml::from_str(&serialized).unwrap();

        assert_eq!(parsed.model, original.model);
        assert_eq!(parsed.api_key, original.api_key);
        assert_eq!(parsed.timeout_secs, original.timeout_secs);
        assert_eq!(parsed.max_iterations, original.max_iterations);
        assert_eq!(parsed.tool_allowlist, original.tool_allowlist);
    }
}
