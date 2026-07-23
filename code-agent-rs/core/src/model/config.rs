//! Model configuration — provider-agnostic API endpoint settings.
//!
//! All LLM configuration is injected via environment variables or builder setters.
//! No provider-specific defaults are hardcoded — the system works with any
//! OpenAI-compatible API gateway.
//!
//! # 领域描述
//!
//! 模型配置是 AI Agent 与 LLM 模型系列交互的配置上下文。
//! 它集中管理 API 端点、认证密钥、各模型名称、采样参数、
//! 速率限制与重试策略，是整个模型层的单一配置入口。
//! 所有模型共享同一 API Gateway 和密钥，但各自的模型名称可独立覆盖。
//!
//! # Environment Variables
//!
//! | Variable            | Required | Default                      | Description          |
//! |---------------------|----------|------------------------------|----------------------|
//! | `LLM_API_BASE_URL`  | Yes      | `http://localhost:8080/v1`   | API base URL         |
//! | `LLM_API_KEY`       | Yes      | —                            | API authentication   |
//! | `LLM_CHAT_MODEL`    | Yes      | —                            | Chat model name      |
//! | `LLM_EMBEDDING_MODEL`| No      | (same as chat model)         | Embedding model name |
//! | `LLM_RERANKER_MODEL` | No      | —                            | Reranker model name  |
//! | `LLM_GUARD_MODEL`   | No       | —                            | Safety guard model   |
//! | `LLM_VL_MODEL`      | No       | —                            | Vision model name    |
//! | `LLM_TEMPERATURE`   | No       | `0.2`                        | Sampling temperature |
//! | `LLM_MAX_TOKENS`    | No       | `4096`                       | Max tokens per call  |

use crate::model::types::{RateLimitConfig, RetryConfig};

// ---------------------------------------------------------------------------
// Defaults (provider-agnostic, numeric-only)
// ---------------------------------------------------------------------------

/// Default API base URL when no LLM_API_BASE_URL env var is set.
///
/// Defaults to a local development endpoint.
const DEFAULT_API_BASE_URL: &str = "http://localhost:8080/v1";

/// 默认采样温度 —— 代码生成场景下使用较低温度以获得更确定的输出
///
/// 【领域含义】温度参数控制生成结果的随机性。
/// 0.2 是代码生成场景的推荐值，在创造性与确定性之间取得平衡。
const DEFAULT_TEMPERATURE: f32 = 0.2;

/// 默认最大生成令牌数
///
/// 【领域含义】限制单次模型调用生成的令牌数量上限，
/// 防止因生成过长内容导致失控或超时。
const DEFAULT_MAX_TOKENS: u32 = 4096;

// ---------------------------------------------------------------------------
// Environment variable names
// ---------------------------------------------------------------------------

/// Environment variable name for the API base URL.
const ENV_API_BASE_URL: &str = "LLM_API_BASE_URL";

/// Environment variable name for the API key.
const ENV_API_KEY: &str = "LLM_API_KEY";

/// Environment variable name for the chat model.
const ENV_CHAT_MODEL: &str = "LLM_CHAT_MODEL";

/// Environment variable name for the embedding model.
const ENV_EMBEDDING_MODEL: &str = "LLM_EMBEDDING_MODEL";

/// Environment variable name for the reranker model.
const ENV_RERANKER_MODEL: &str = "LLM_RERANKER_MODEL";

/// Environment variable name for the vision-language model.
const ENV_VL_MODEL: &str = "LLM_VL_MODEL";

/// Environment variable name for the safety guard model.
const ENV_GUARD_MODEL: &str = "LLM_GUARD_MODEL";

/// Environment variable name for the sampling temperature.
const ENV_TEMPERATURE: &str = "LLM_TEMPERATURE";

/// Environment variable name for max tokens per completion.
const ENV_MAX_TOKENS: &str = "LLM_MAX_TOKENS";

// ---------------------------------------------------------------------------
// ModelConfig
// ---------------------------------------------------------------------------

/// LLM 模型统一配置 —— 管理所有模型端点的连接参数
///
/// 【领域含义】ModelConfig 是整个模型层的配置聚合根。
/// 它持有 API 端点、认证凭据、所有模型名称、采样参数、
/// 速率限制与重试策略，被各模型客户端共享使用。
/// 所有模型共享同一 API Key 和 Base URL，但各自的模型名称可独立配置。
///
/// Provider-agnostic configuration for all LLM model endpoints.
///
/// All models share the same API key and base URL. Individual model names
/// can be overridden for staging or custom deployments.
///
/// # Examples
///
/// ```rust
/// use code_agent_core::model::ModelConfig;
///
/// let config = ModelConfig::builder()
///     .api_key("sk-abc".into())
///     .api_base_url("http://localhost:8080/v1".into())
///     .model("my-model".into())
///     .build()
///     .expect("valid config");
/// ```
///
/// ```rust,no_run
/// use code_agent_core::model::ModelConfig;
///
/// let config = ModelConfig::from_env()
///     .expect("LLM_API_KEY and LLM_CHAT_MODEL in environment");
/// ```
#[derive(Clone, Debug)]
pub struct ModelConfig {
    /// API Gateway 基础 URL。默认: http://localhost:8080/v1
    ///
    /// Base URL for the OpenAI-compatible API gateway.
    /// Default: `http://localhost:8080/v1`
    pub api_base_url: String,

    /// API 认证密钥。以 Authorization: Bearer {key} 形式发送。
    ///
    /// API key for authentication (sent as `Authorization: Bearer {key}`).
    pub api_key: String,

    /// 聊天模型名称，用于推理与代码生成。
    ///
    /// Core chat model name for reasoning and code generation.
    /// Must be set via `LLM_CHAT_MODEL` env var or builder.
    pub model: String,

    /// 嵌入模型名称，用于代码向量化。
    ///
    /// Embedding model name for code vectorization.
    /// Optional; defaults to chat model if not set.
    pub embedding_model: String,

    /// 重排序模型名称，用于搜索结果精排。
    ///
    /// Reranker model name for search result re-ranking.
    pub reranker_model: String,

    /// 视觉语言模型名称，用于 UI 理解。
    ///
    /// Vision-language model name for UI understanding.
    pub vl_model: String,

    /// 安全守卫模型名称，用于内容过滤。
    ///
    /// Safety guard model name for content filtering.
    pub guard_model: String,

    /// 采样温度 (0.0–2.0)，越低越确定。默认: 0.2
    ///
    /// Sampling temperature (0.0–2.0). Lower = more deterministic.
    /// Default: 0.2
    pub temperature: f32,

    /// 每次补全的最大令牌数。默认: 4096
    ///
    /// Maximum tokens per completion.
    /// Default: 4096
    pub max_tokens: u32,

    /// 是否通过 SSE 流式响应。默认: true
    ///
    /// Whether to stream responses via SSE.
    /// Default: true
    pub stream: bool,

    /// 速率限制配置
    ///
    /// Rate limiting configuration.
    pub rate_limit: RateLimitConfig,

    /// 指数退避重试配置
    ///
    /// Retry configuration with exponential backoff.
    pub retry: RetryConfig,
}

impl ModelConfig {
    /// 构建 ModelConfigBuilder 实例
    ///
    /// 【领域含义】提供链式调用构建器，用于便捷创建 ModelConfig 实例。
    /// 未设置的字段将使用默认值。
    ///
    /// Create a new builder for constructing a `ModelConfig`.
    pub fn builder() -> ModelConfigBuilder {
        ModelConfigBuilder::default()
    }

    /// 从环境变量创建 ModelConfig
    ///
    /// 【领域含义】读取标准 LLM_* 环境变量，适用于容器化部署与 CI/CD 场景，
    /// 避免在代码中硬编码敏感信息。
    ///
    /// Required: `LLM_API_KEY`, `LLM_CHAT_MODEL`
    /// Optional: `LLM_API_BASE_URL`, `LLM_EMBEDDING_MODEL`, `LLM_RERANKER_MODEL`,
    ///           `LLM_GUARD_MODEL`, `LLM_VL_MODEL`, `LLM_TEMPERATURE`, `LLM_MAX_TOKENS`
    ///
    /// # Errors
    ///
    /// Returns `Err` if `LLM_API_KEY` or `LLM_CHAT_MODEL` is not set.
    pub fn from_env() -> Result<Self, ConfigError> {
        let api_key = std::env::var(ENV_API_KEY)
            .map_err(|_| ConfigError::MissingEnvVar(ENV_API_KEY.to_string()))?;
        let model = std::env::var(ENV_CHAT_MODEL)
            .map_err(|_| ConfigError::MissingEnvVar(ENV_CHAT_MODEL.to_string()))?;
        let base_url = std::env::var(ENV_API_BASE_URL)
            .unwrap_or_else(|_| DEFAULT_API_BASE_URL.to_string());
        let embedding_model = std::env::var(ENV_EMBEDDING_MODEL)
            .unwrap_or_else(|_| model.clone());
        let reranker_model = std::env::var(ENV_RERANKER_MODEL).unwrap_or_default();
        let vl_model = std::env::var(ENV_VL_MODEL).unwrap_or_default();
        let guard_model = std::env::var(ENV_GUARD_MODEL).unwrap_or_default();
        let temperature: f32 = std::env::var(ENV_TEMPERATURE)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_TEMPERATURE);
        let max_tokens: u32 = std::env::var(ENV_MAX_TOKENS)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MAX_TOKENS);

        Ok(Self {
            api_base_url: base_url,
            api_key,
            model,
            embedding_model,
            reranker_model,
            vl_model,
            guard_model,
            temperature,
            max_tokens,
            stream: true,
            rate_limit: RateLimitConfig::default(),
            retry: RetryConfig::default(),
        })
    }

    /// 生成聊天补全端点的完整 URL
    ///
    /// 【领域含义】拼接 api_base_url 与 chat/completions 路径，
    /// 用于客户端发送流式或非流式补全请求。
    ///
    /// Full URL for the chat completions endpoint.
    pub fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.api_base_url.trim_end_matches('/'))
    }

    /// 生成嵌入端点的完整 URL
    ///
    /// 【领域含义】拼接 api_base_url 与 embeddings 路径，
    /// 用于客户端发送向量化请求。
    ///
    /// Full URL for the embeddings endpoint.
    pub fn embeddings_url(&self) -> String {
        format!("{}/embeddings", self.api_base_url.trim_end_matches('/'))
    }

    /// 生成重排序端点的完整 URL
    ///
    /// 【领域含义】拼接 api_base_url 与 rerank 路径，
    /// 用于客户端发送精排请求。
    ///
    /// Full URL for the rerank endpoint.
    pub fn rerank_url(&self) -> String {
        format!("{}/rerank", self.api_base_url.trim_end_matches('/'))
    }
}

impl Default for ModelConfig {
    /// 返回空的占位值。用户必须通过 builder 或 `from_env()` 配置。
    ///
    /// Returns empty placeholder values. Users must configure via builder or
    /// `from_env()`.
    fn default() -> Self {
        Self {
            api_base_url: DEFAULT_API_BASE_URL.to_string(),
            api_key: String::new(),
            model: String::new(),
            embedding_model: String::new(),
            reranker_model: String::new(),
            vl_model: String::new(),
            guard_model: String::new(),
            temperature: DEFAULT_TEMPERATURE,
            max_tokens: DEFAULT_MAX_TOKENS,
            stream: true,
            rate_limit: RateLimitConfig::default(),
            retry: RetryConfig::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// ModelConfig 构建器 —— 链式配置模型参数
///
/// 【领域含义】通过 Builder 模式提供类型安全的配置组装方式。
/// 所有字段均有占位默认值（空字符串），调用方可按需覆盖。
/// 必须设置字段：api_key, model。
///
/// # Examples
///
/// ```rust
/// use code_agent_core::model::ModelConfig;
///
/// let config = ModelConfig::builder()
///     .api_key("sk-abc".into())
///     .api_base_url("http://localhost:8080/v1".into())
///     .model("my-chat-model".into())
///     .temperature(0.1)
///     .build()
///     .expect("valid");
/// ```
#[derive(Default)]
pub struct ModelConfigBuilder {
    api_base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    embedding_model: Option<String>,
    reranker_model: Option<String>,
    vl_model: Option<String>,
    guard_model: Option<String>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    stream: Option<bool>,
    rate_limit: Option<RateLimitConfig>,
    retry: Option<RetryConfig>,
}

impl ModelConfigBuilder {
    /// 设置 API Gateway 基础 URL
    ///
    /// 【领域含义】设置 API 端点地址，用于测试或自定义部署。
    /// Defaults to `http://localhost:8080/v1` if not set.
    pub fn api_base_url(mut self, url: String) -> Self {
        self.api_base_url = Some(url);
        self
    }

    /// 设置 API 认证密钥（必填）
    ///
    /// 【领域含义】设置 API 密钥，构建时会验证此字段不为空。
    pub fn api_key(mut self, key: String) -> Self {
        self.api_key = Some(key);
        self
    }

    /// 设置聊天模型名称（必填）
    ///
    /// 【领域含义】设置主推理模型名称。
    pub fn model(mut self, model: String) -> Self {
        self.model = Some(model);
        self
    }

    /// 设置嵌入模型名称
    ///
    /// 【领域含义】如果不设置，默认与聊天模型一致。
    pub fn embedding_model(mut self, model: String) -> Self {
        self.embedding_model = Some(model);
        self
    }

    /// 设置重排序模型名称
    pub fn reranker_model(mut self, model: String) -> Self {
        self.reranker_model = Some(model);
        self
    }

    /// 设置视觉语言模型名称
    pub fn vl_model(mut self, model: String) -> Self {
        self.vl_model = Some(model);
        self
    }

    /// 设置安全守卫模型名称
    pub fn guard_model(mut self, model: String) -> Self {
        self.guard_model = Some(model);
        self
    }

    /// 设置采样温度
    ///
    /// 【领域含义】控制生成的随机性。代码场景建议 0.1–0.3，
    /// 创意场景可适当提高至 0.7–1.0。
    pub fn temperature(mut self, temp: f32) -> Self {
        self.temperature = Some(temp);
        self
    }

    /// 设置每次补全的最大令牌数
    ///
    /// 【领域含义】限制单次输出长度。长文档生成场景可增大此值。
    pub fn max_tokens(mut self, max: u32) -> Self {
        self.max_tokens = Some(max);
        self
    }

    /// 设置是否启用 SSE 流式响应
    ///
    /// 【领域含义】流式（true）适用于实时交互场景，
    /// 非流式（false）适用于只需最终结果的批处理场景。
    pub fn stream(mut self, stream: bool) -> Self {
        self.stream = Some(stream);
        self
    }

    /// 设置速率限制配置
    ///
    /// 【领域含义】配置每分钟/每天的最大请求数与突发限制，
    /// 防止因请求过频触发 API 限流。
    pub fn rate_limit(mut self, config: RateLimitConfig) -> Self {
        self.rate_limit = Some(config);
        self
    }

    /// 设置重试配置
    ///
    /// 【领域含义】配置可重试错误（429、5xx）的自动重试策略，
    /// 包含最大重试次数、初始延迟与退避乘数。
    pub fn retry(mut self, config: RetryConfig) -> Self {
        self.retry = Some(config);
        self
    }

    /// 构建最终的 ModelConfig 实例
    ///
    /// # 错误
    ///
    /// 若未设置 api_key 或 model 则返回 ConfigError::MissingEnvVar。
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::MissingEnvVar` if required fields are missing.
    pub fn build(self) -> Result<ModelConfig, ConfigError> {
        let defaults = ModelConfig::default();
        let model = self.model.ok_or(ConfigError::MissingEnvVar(ENV_CHAT_MODEL.to_string()))?;
        Ok(ModelConfig {
            api_base_url: self.api_base_url.unwrap_or(defaults.api_base_url),
            api_key: self.api_key.ok_or(ConfigError::MissingEnvVar(ENV_API_KEY.to_string()))?,
            model,
            embedding_model: self
                .embedding_model
                .unwrap_or(defaults.embedding_model),
            reranker_model: self.reranker_model.unwrap_or(defaults.reranker_model),
            vl_model: self.vl_model.unwrap_or(defaults.vl_model),
            guard_model: self.guard_model.unwrap_or(defaults.guard_model),
            temperature: self.temperature.unwrap_or(defaults.temperature),
            max_tokens: self.max_tokens.unwrap_or(defaults.max_tokens),
            stream: self.stream.unwrap_or(defaults.stream),
            rate_limit: self.rate_limit.unwrap_or(defaults.rate_limit),
            retry: self.retry.unwrap_or(defaults.retry),
        })
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// 配置错误类型
///
/// 【领域含义】定义模型配置阶段可能发生的错误，
/// 当前包含缺失必需环境变量的场景。
///
/// Configuration errors.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// 必需的环境变量未设置
    ///
    /// A required environment variable was not set.
    #[error("Missing required environment variable: {0}")]
    MissingEnvVar(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Global mutex to prevent parallel env-var-dependent tests from interfering.
    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn default_config_has_placeholder_values() {
        let config = ModelConfig::default();
        // Placeholder defaults — model names are empty
        assert!(config.model.is_empty());
        assert!(config.embedding_model.is_empty());
        assert!(config.reranker_model.is_empty());
        assert!(config.vl_model.is_empty());
        assert!(config.guard_model.is_empty());
        // API base URL defaults to localhost
        assert_eq!(config.api_base_url, DEFAULT_API_BASE_URL);
        // Numeric defaults are reasonable
        assert_eq!(config.temperature, DEFAULT_TEMPERATURE);
        assert_eq!(config.max_tokens, DEFAULT_MAX_TOKENS);
        assert!(config.stream);
        assert_eq!(config.rate_limit.requests_per_minute, Some(60));
        assert_eq!(config.retry.max_retries, 3);
    }

    #[test]
    fn builder_requires_api_key_and_model() {
        let result = ModelConfig::builder().build();
        assert!(result.is_err());
        match result {
            Err(ConfigError::MissingEnvVar(_)) => {}
            _ => panic!("expected MissingEnvVar"),
        }
    }

    #[test]
    fn builder_requires_api_key() {
        let result = ModelConfig::builder().model("my-model".into()).build();
        assert!(result.is_err());
    }

    #[test]
    fn builder_requires_model() {
        let result = ModelConfig::builder().api_key("sk-test".into()).build();
        assert!(result.is_err());
    }

    #[test]
    fn builder_with_all_required_succeeds() {
        let config = ModelConfig::builder()
            .api_key("sk-test".into())
            .model("test-model".into())
            .api_base_url("http://localhost:9999/v1".into())
            .build()
            .expect("should build");
        assert_eq!(config.api_key, "sk-test");
        assert_eq!(config.model, "test-model");
        assert_eq!(config.api_base_url, "http://localhost:9999/v1");
    }

    #[test]
    fn builder_overrides_defaults() {
        let config = ModelConfig::builder()
            .api_key("sk-abc".into())
            .model("custom-model".into())
            .temperature(0.5)
            .max_tokens(2048)
            .stream(false)
            .build()
            .expect("should build");
        assert_eq!(config.model, "custom-model");
        assert_eq!(config.temperature, 0.5);
        assert_eq!(config.max_tokens, 2048);
        assert!(!config.stream);
    }

    #[test]
    fn urls_are_correctly_constructed() {
        let config = ModelConfig::builder()
            .api_key("sk-abc".into())
            .model("test-model".into())
            .api_base_url("https://api.example.com/v1".into())
            .build()
            .expect("should build");
        assert_eq!(config.chat_completions_url(), "https://api.example.com/v1/chat/completions");
        assert_eq!(config.embeddings_url(), "https://api.example.com/v1/embeddings");
        assert_eq!(config.rerank_url(), "https://api.example.com/v1/rerank");
    }

    #[test]
    fn urls_trim_trailing_slash() {
        let config = ModelConfig::builder()
            .api_key("sk-abc".into())
            .model("test-model".into())
            .api_base_url("https://api.example.com/v1/".into())
            .build()
            .expect("should build");
        assert_eq!(config.chat_completions_url(), "https://api.example.com/v1/chat/completions");
    }

    #[test]
    fn from_env_missing_key_fails() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        std::env::remove_var(ENV_API_KEY);
        std::env::set_var(ENV_CHAT_MODEL, "test-chat-model");
        let result = ModelConfig::from_env();
        assert!(result.is_err());
    }

    #[test]
    fn from_env_missing_model_fails() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var(ENV_API_KEY, "sk-env-test");
        std::env::remove_var(ENV_CHAT_MODEL);
        let result = ModelConfig::from_env();
        assert!(result.is_err());
        std::env::remove_var(ENV_API_KEY);
    }

    #[test]
    fn from_env_with_key_and_model_succeeds() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var(ENV_API_KEY, "sk-env-test");
        std::env::set_var(ENV_CHAT_MODEL, "test-chat-model");
        let config = ModelConfig::from_env().expect("should succeed");
        assert_eq!(config.api_key, "sk-env-test");
        assert_eq!(config.model, "test-chat-model");
        // Default base URL
        assert_eq!(config.api_base_url, DEFAULT_API_BASE_URL);
        // Embedding model defaults to chat model
        assert_eq!(config.embedding_model, "test-chat-model");
        std::env::remove_var(ENV_API_KEY);
        std::env::remove_var(ENV_CHAT_MODEL);
    }

    #[test]
    fn from_env_with_custom_base_url() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var(ENV_API_KEY, "sk-env-test");
        std::env::set_var(ENV_CHAT_MODEL, "test-model");
        std::env::set_var(ENV_API_BASE_URL, "https://custom.example.com/v1");
        let config = ModelConfig::from_env().expect("should succeed");
        assert_eq!(config.api_base_url, "https://custom.example.com/v1");
        std::env::remove_var(ENV_API_KEY);
        std::env::remove_var(ENV_CHAT_MODEL);
        std::env::remove_var(ENV_API_BASE_URL);
    }

    #[test]
    fn from_env_reads_all_model_names() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var(ENV_API_KEY, "sk-env-test");
        std::env::set_var(ENV_CHAT_MODEL, "chat-model");
        std::env::set_var(ENV_EMBEDDING_MODEL, "embed-model");
        std::env::set_var(ENV_RERANKER_MODEL, "rerank-model");
        std::env::set_var(ENV_GUARD_MODEL, "guard-model");
        std::env::set_var(ENV_VL_MODEL, "vl-model");
        std::env::set_var(ENV_TEMPERATURE, "0.7");
        std::env::set_var(ENV_MAX_TOKENS, "8192");
        let config = ModelConfig::from_env().expect("should succeed");
        assert_eq!(config.embedding_model, "embed-model");
        assert_eq!(config.reranker_model, "rerank-model");
        assert_eq!(config.guard_model, "guard-model");
        assert_eq!(config.vl_model, "vl-model");
        assert_eq!(config.temperature, 0.7);
        assert_eq!(config.max_tokens, 8192);
        // Clean up
        for var in &[ENV_API_KEY, ENV_CHAT_MODEL, ENV_EMBEDDING_MODEL,
                      ENV_RERANKER_MODEL, ENV_GUARD_MODEL, ENV_VL_MODEL,
                      ENV_TEMPERATURE, ENV_MAX_TOKENS] {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn config_error_display() {
        let err = ConfigError::MissingEnvVar("LLM_API_KEY".into());
        let msg = err.to_string();
        assert!(msg.contains("LLM_API_KEY"));
    }
}
