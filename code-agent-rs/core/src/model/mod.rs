//! 多模型供应商抽象层
//!
//! 【领域含义】本模块定义了与语言模型（Chat）、嵌入模型（Embedding）和
//! 重排序模型（Reranker）交互的 trait 和实现。遵循「面向接口编程」原则，
//! Agent 核心只依赖 `ModelClient` trait，不依赖具体实现。
//!
//! 【核心抽象】
//! - `ModelClient`: 聊天补全模型的标准接口（provider-agnostic）
//! - `EmbeddingClient`: 文本嵌入模型接口
//! - `RerankerClient`: 文档重排序模型接口
//!
//! 【当前供应商】
//! | Provider   | File            | Protocol           |
//! |-----------|-----------------|--------------------|
//! | Qwen3     | [`qwen3`]       | OpenAI-compatible  |
//! | Embedding | [`embedding`]   | OpenAI-compatible  |
//! | Reranker  | [`reranker`]    | Custom             |
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────┐
//! │         Agent Core              │
//! │  (uses ModelClient trait)       │
//! └──────────────┬──────────────────┘
//!                │
//!    ┌───────────┴───────────┐
//!    ▼                       ▼
//! ┌──────────────┐   ┌──────────────────┐
//! │ Qwen3 ChatGPT │   │ (future: Ollama, │
//! │ (OpenAI API)  │   │  Anthropic, ...) │
//! └──────────────┘   └──────────────────┘
//! ```

pub mod anthropic;
pub mod config;
pub mod embedding;
pub mod gemini;
pub mod qwen3;
pub mod reranker;
pub mod types;

use async_trait::async_trait;
use code_agent_protocol::{Message, ResponseEvent};
use futures::stream::Stream;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

pub use config::ModelConfig;
pub use types::{RateLimitConfig, RetryConfig, TokenUsage, ToolDefinition};

// Re-export provider implementations
pub use anthropic::AnthropicClient;
pub use embedding::{EmbeddingClient, Qwen3EmbeddingClient};
pub use gemini::GeminiClient;
pub use qwen3::Qwen3OpenAIClient;
pub use reranker::{Qwen3RerankerClient, RerankerClient};

// ---------------------------------------------------------------------------
// Model error
// ---------------------------------------------------------------------------

/// 模型调用错误 —— 供应商无关的错误类型
///
/// 【领域含义】覆盖所有模型 API 调用可能出现的错误场景：
/// 网络传输错误、序列化错误、API 错误码、速率限制、配置缺失、
/// I/O 错误和流异常终止。
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// 传输层错误（网络、TLS、超时）
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// JSON 序列化或反序列化错误
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// API 返回了错误状态码
    #[error("API error {status}: {message}")]
    Api {
        /// HTTP 状态码
        status: u16,
        /// API 错误消息
        message: String,
    },

    /// 速率限制超出
    #[error("Rate limited: {0}")]
    RateLimited(String),

    /// 缺少必需配置
    #[error("Configuration error: {0}")]
    Config(String),

    /// 流读取期间的 I/O 错误
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// 流在没有有效完成的情况下结束
    #[error("Stream ended unexpectedly")]
    StreamEnded,

    /// 通用兜底错误
    #[error("{0}")]
    Other(String),
}

/// 模型操作的结果类型别名
pub type ModelResult<T> = Result<T, ModelError>;

// ---------------------------------------------------------------------------
// ModelClient trait
// ---------------------------------------------------------------------------

/// 聊天补全模型客户端 —— 核心领域接口
///
/// 【领域含义】这是 Coding Agent 的核心抽象接口。每个模型供应商
/// （Qwen3、Ollama、Anthropic 等）都实现此 trait，使 Agent 核心
/// 可以做到供应商无关。
///
/// 【设计原则】
/// - **流式优先**: `complete_stream` 是主要 API。
///   非流式 `complete` 方法有默认实现（收集流事件为最终字符串），
///   供应商 SHOULD 覆盖此实现以提高效率。
/// - **工具感知**: 供应商接收工具定义，可以发射
///   [`ResponseEvent::ToolCallBegin`] 事件。
/// - **Token 追踪**: 供应商通过 `ResponseEvent::TokenUsage` 报告
///   token 使用量，并通过 `last_token_usage` 暴露以进行成本监控。
#[async_trait]
pub trait ModelClient: Send + Sync {
    /// 模型名称如配置（例如 `"my-chat-model"`）
    fn model_name(&self) -> &str;

    /// 发送聊天补全请求并流式返回 `ResponseEvent`
    ///
    /// 事件包括文本增量、工具调用调用、token 使用统计和错误通知。
    /// 成功时流以 `TurnComplete` 事件结束。
    ///
    /// # Arguments
    /// * `messages` — 对话历史（系统、用户、助手、工具结果）
    /// * `tools` — 模型可以调用的工具定义
    /// * `temperature` — 每个请求的温度覆盖（`None` 表示使用客户端默认值）
    async fn complete_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        temperature: Option<f32>,
    ) -> ModelResult<Box<dyn Stream<Item = ResponseEvent> + Send + Unpin>>;

    /// 非流式补全 —— 用于简单查询
    ///
    /// 默认实现收集所有流增量到单个字符串。
    /// 供应商 SHOULD 覆盖此实现以提高效率。
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        temperature: Option<f32>,
    ) -> ModelResult<String> {
        use futures::StreamExt;

        let mut stream = self.complete_stream(messages, tools, temperature).await?;
        let mut result = String::new();
        while let Some(event) = stream.next().await {
            if let ResponseEvent::AgentMessageDelta { content } = event {
                result.push_str(&content);
            }
        }
        Ok(result)
    }

    /// 获取最近一次调用的 token 使用统计
    fn last_token_usage(&self) -> Option<TokenUsage>;
}

// ---------------------------------------------------------------------------
// Shared client utilities
// ---------------------------------------------------------------------------

/// 构建具有合理默认值的 `reqwest::Client` 用于 API 调用
pub(crate) fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .pool_max_idle_per_host(5)
        .build()
        .expect("reqwest Client should always build with default config")
}

/// 将协议 `Message` 转换为聊天补全请求消息格式
pub(crate) fn convert_messages(messages: &[Message]) -> Vec<types::ChatMessage> {
    messages
        .iter()
        .map(|msg| match msg {
            Message::UserMessage { content } => types::ChatMessage::User {
                content: content.clone(),
            },
            Message::AssistantMessage { content } => types::ChatMessage::Assistant {
                content: serde_json::Value::String(content.clone()),
                tool_calls: None,
            },
            Message::ToolCall(tc) => types::ChatMessage::Assistant {
                content: serde_json::Value::Null,
                tool_calls: Some(vec![types::ChatToolCall {
                    id: tc.id.clone(),
                    call_type: "function".to_string(),
                    function: types::ChatFunctionCall {
                        name: tc.name.clone(),
                        arguments: serde_json::to_string(&tc.arguments).unwrap_or_default(),
                    },
                }]),
            },
            Message::ToolResult(tr) => types::ChatMessage::Tool {
                content: tr.output.clone().unwrap_or_default(),
                tool_call_id: tr.tool_call_id.clone(),
            },
        })
        .collect()
}

/// 将 `ToolDefinition` 转换为聊天补全请求工具格式
pub(crate) fn convert_tools(tools: &[ToolDefinition]) -> Vec<types::ChatToolDef> {
    tools
        .iter()
        .map(|tool| types::ChatToolDef {
            tool_type: "function".to_string(),
            function: types::ChatFunctionDef {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
            },
        })
        .collect()
}

/// 判断状态码是否可重试
pub(crate) fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

// ---------------------------------------------------------------------------
// Provider factory
// ---------------------------------------------------------------------------

/// 模型供应商种类
///
/// 【领域含义】标识底层 LLM 供应商。用于工厂函数自动选择
/// 正确的 `ModelClient` 实现，也支持显式指定供应商。
///
/// Provider kind for automatic model client selection.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum ProviderKind {
    /// OpenAI 兼容供应商（Qwen3 等）
    Qwen3,
    /// Anthropic (Claude) 供应商
    Anthropic,
    /// Google Gemini 供应商
    Gemini,
}

impl ProviderKind {
    /// 根据模型名称推断供应商
    ///
    /// 【领域含义】通过模型名称前缀自动检测供应商种类。
    /// 规则：
    /// - 以 `"claude-"` 或 `"anthropic."` 开头 → Anthropic
    /// - 以 `"gemini-"` 开头 → Gemini
    /// - 其他 → Qwen3 (OpenAI 兼容)
    ///
    /// Detect provider kind from model name prefix.
    pub fn from_model_name(model: &str) -> Self {
        let lower = model.to_lowercase();
        if lower.starts_with("claude-") || lower.starts_with("anthropic.") {
            ProviderKind::Anthropic
        } else if lower.starts_with("gemini-") {
            ProviderKind::Gemini
        } else {
            ProviderKind::Qwen3
        }
    }
}

impl FromStr for ProviderKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "qwen3" | "openai" => Ok(ProviderKind::Qwen3),
            "anthropic" | "claude" => Ok(ProviderKind::Anthropic),
            "gemini" => Ok(ProviderKind::Gemini),
            other => Err(format!(
                "unknown provider '{other}'. Valid: qwen3, anthropic, gemini"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// ModelProvider trait — extensible provider abstraction
// ---------------------------------------------------------------------------

/// 模型供应商抽象接口
///
/// 【领域含义】每个模型供应商（Qwen3、Anthropic、Gemini 等）实现此 trait，
/// 通过 [`ProviderRegistry`] 注册后，工厂函数即可自动创建对应的
/// [`ModelClient`] 实例。新增供应商只需实现此 trait + 注册一次，
/// 无需修改工厂函数。
///
/// # Examples
///
/// ```rust,no_run
/// use code_agent_core::model::{ModelProvider, ProviderKind, ModelConfig, ModelClient};
/// use std::sync::Arc;
///
/// struct MyProvider;
///
/// impl ModelProvider for MyProvider {
///     fn provider_kind(&self) -> ProviderKind { ProviderKind::Qwen3 }
///     fn create_client(&self, config: ModelConfig) -> Arc<dyn ModelClient> {
///         // construct and return your client
///         unimplemented!()
///     }
/// }
/// ```
pub trait ModelProvider: Send + Sync {
    /// 返回此供应商对应的 [`ProviderKind`]
    fn provider_kind(&self) -> ProviderKind;

    /// 根据配置创建模型客户端实例
    ///
    /// 返回 `Arc<dyn ModelClient>` 以便在组件间共享同一个客户端。
    fn create_client(&self, config: ModelConfig) -> Arc<dyn ModelClient>;
}

// ---------------------------------------------------------------------------
// ProviderRegistry — extensible provider registration
// ---------------------------------------------------------------------------

/// 模型供应商注册中心
///
/// 【领域含义】维护 `ProviderKind → Box<dyn ModelProvider>` 映射，
/// 支持运行时注册新供应商和按种类创建客户端。
///
/// 通过 [`ProviderRegistry::builtin()`] 获取预注册了所有内置供应商的实例。
///
/// # Examples
///
/// ```rust,no_run
/// use code_agent_core::model::{ProviderRegistry, ProviderKind, ModelConfig};
///
/// let registry = ProviderRegistry::builtin();
/// // let client = registry.create_client(ProviderKind::Qwen3, config)?;
/// ```
pub struct ProviderRegistry {
    providers: HashMap<ProviderKind, Box<dyn ModelProvider>>,
}

impl ProviderRegistry {
    /// 创建空的注册中心
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    /// 注册一个模型供应商
    ///
    /// # Errors
    ///
    /// 如果同一种类的供应商已被注册，返回 [`ModelError::Config`]。
    pub fn register(&mut self, provider: Box<dyn ModelProvider>) -> Result<(), ModelError> {
        let kind = provider.provider_kind();
        if self.providers.contains_key(&kind) {
            return Err(ModelError::Config(format!(
                "provider {:?} is already registered",
                kind
            )));
        }
        self.providers.insert(kind, provider);
        Ok(())
    }

    /// 根据供应商种类和配置创建模型客户端
    ///
    /// # Errors
    ///
    /// 如果对应种类的供应商未注册，返回 [`ModelError::Config`]。
    pub fn create_client(
        &self,
        kind: ProviderKind,
        config: ModelConfig,
    ) -> ModelResult<Arc<dyn ModelClient>> {
        self.providers
            .get(&kind)
            .map(|p| p.create_client(config))
            .ok_or_else(|| {
                ModelError::Config(format!("no provider registered for {:?}", kind))
            })
    }

    /// 创建预注册了所有内置供应商的注册中心
    ///
    /// 包含：
    /// - Qwen3（OpenAI 兼容）
    /// - Anthropic（Claude）
    /// - Gemini
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        // Register built-in providers. Panics on duplicate are acceptable
        // since ProviderKind variants are statically known to be unique.
        registry
            .register(Box::new(qwen3::Qwen3Provider))
            .expect("Qwen3Provider should not conflict");
        registry
            .register(Box::new(anthropic::AnthropicProvider))
            .expect("AnthropicProvider should not conflict");
        registry
            .register(Box::new(gemini::GeminiProvider))
            .expect("GeminiProvider should not conflict");
        registry
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 创建模型客户端工厂函数
///
/// 【领域含义】根据供应商种类和配置创建对应的 `ModelClient` 实现。
/// 内部使用 [`ProviderRegistry::builtin()`] 查找已注册的供应商。
/// 返回 `Arc<dyn ModelClient>` 以便在不同组件间共享。
///
/// # Arguments
/// - `provider`: 供应商种类（可通过 `ProviderKind::from_model_name()` 自动检测）
/// - `config`: 模型配置（API密钥、端点、模型名称等）
///
/// # Examples
///
/// ```rust,no_run
/// use code_agent_core::model::{create_model_client, ProviderKind, ModelConfig};
///
/// let config = ModelConfig::from_env().expect("config");
/// let client = create_model_client(ProviderKind::from_model_name(&config.model), config);
/// ```
pub fn create_model_client(
    provider: ProviderKind,
    config: ModelConfig,
) -> Arc<dyn ModelClient> {
    // Use the builtin registry to look up the provider.
    // Since builtin() pre-registers all three providers,
    // the lookup should never fail for valid ProviderKind values.
    ProviderRegistry::builtin()
        .create_client(provider, config)
        .expect("builtin provider should always be registered")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use code_agent_protocol::{ToolCall, ToolResultMessage};

    #[test]
    fn convert_user_message() {
        let msgs = vec![Message::UserMessage {
            content: "hello".into(),
        }];
        let chat_msgs = convert_messages(&msgs);
        assert_eq!(chat_msgs.len(), 1);
        let json = serde_json::to_string(&chat_msgs[0]).expect("serialize");
        assert!(json.contains("user"));
        assert!(json.contains("hello"));
    }

    #[test]
    fn convert_tool_call_message() {
        let tc = ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "src/main.rs"}),
        };
        let msgs = vec![Message::ToolCall(tc)];
        let chat_msgs = convert_messages(&msgs);
        assert_eq!(chat_msgs.len(), 1);
        let json = serde_json::to_string(&chat_msgs[0]).expect("serialize");
        assert!(json.contains("assistant"));
        assert!(json.contains("tool_calls"));
        assert!(json.contains("read_file"));
    }

    #[test]
    fn convert_tool_result_message() {
        let tr = ToolResultMessage {
            tool_call_id: "call-1".into(),
            output: Some("file contents".into()),
            error: None,
        };
        let msgs = vec![Message::ToolResult(tr)];
        let chat_msgs = convert_messages(&msgs);
        assert_eq!(chat_msgs.len(), 1);
        let json = serde_json::to_string(&chat_msgs[0]).expect("serialize");
        assert!(json.contains("tool"));
        assert!(json.contains("file contents"));
    }

    #[test]
    fn convert_multiple_messages() {
        let msgs = vec![
            Message::UserMessage {
                content: "read main.rs".into(),
            },
            Message::AssistantMessage {
                content: "I'll read the file.".into(),
            },
        ];
        let chat_msgs = convert_messages(&msgs);
        assert_eq!(chat_msgs.len(), 2);
        let json = serde_json::to_string(&chat_msgs).expect("serialize");
        assert!(json.contains("user"));
        assert!(json.contains("assistant"));
    }

    #[test]
    fn convert_tools_empty() {
        let tools: Vec<ToolDefinition> = vec![];
        let chat_tools = convert_tools(&tools);
        assert!(chat_tools.is_empty());
    }

    #[test]
    fn convert_tools_with_definition() {
        let tools = vec![ToolDefinition::new(
            "search",
            "Search the codebase",
            serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}}),
        )];
        let chat_tools = convert_tools(&tools);
        assert_eq!(chat_tools.len(), 1);
        let json = serde_json::to_string(&chat_tools).expect("serialize");
        assert!(json.contains("function"));
        assert!(json.contains("search"));
        assert!(json.contains("Search the codebase"));
    }

    #[test]
    fn retryable_status_codes() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(502));
        assert!(is_retryable_status(503));
        assert!(is_retryable_status(504));
    }

    #[test]
    fn non_retryable_status_codes() {
        assert!(!is_retryable_status(200));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(401));
        assert!(!is_retryable_status(403));
        assert!(!is_retryable_status(404));
    }

    #[test]
    fn model_error_from_serde() {
        let err: ModelError = serde_json::from_str::<serde_json::Value>("bad json")
            .unwrap_err()
            .into();
        assert!(matches!(err, ModelError::Serde(_)));
    }

    #[test]
    fn model_error_display() {
        let err = ModelError::Api {
            status: 500,
            message: "Internal error".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("500"));
        assert!(msg.contains("Internal error"));
    }

    // ── ProviderKind tests ──

    #[test]
    fn provider_kind_from_model_name_claude() {
        assert_eq!(
            ProviderKind::from_model_name("claude-sonnet-4-20250514"),
            ProviderKind::Anthropic
        );
        assert_eq!(
            ProviderKind::from_model_name("claude-opus-4-20250514"),
            ProviderKind::Anthropic
        );
    }

    #[test]
    fn provider_kind_from_model_name_anthropic_dot() {
        assert_eq!(
            ProviderKind::from_model_name("anthropic.claude-sonnet-4-20250514"),
            ProviderKind::Anthropic
        );
    }

    #[test]
    fn provider_kind_from_model_name_gemini() {
        assert_eq!(
            ProviderKind::from_model_name("gemini-2.5-pro"),
            ProviderKind::Gemini
        );
        assert_eq!(
            ProviderKind::from_model_name("gemini-2.0-flash"),
            ProviderKind::Gemini
        );
    }

    #[test]
    fn provider_kind_from_model_name_defaults_to_qwen3() {
        assert_eq!(
            ProviderKind::from_model_name("qwen3.6-27b"),
            ProviderKind::Qwen3
        );
        assert_eq!(
            ProviderKind::from_model_name("gpt-4o"),
            ProviderKind::Qwen3
        );
        assert_eq!(
            ProviderKind::from_model_name("unknown-model"),
            ProviderKind::Qwen3
        );
        assert_eq!(ProviderKind::from_model_name(""), ProviderKind::Qwen3);
    }

    #[test]
    fn provider_kind_case_insensitive() {
        assert_eq!(
            ProviderKind::from_model_name("CLAUDE-SONNET-4"),
            ProviderKind::Anthropic
        );
        assert_eq!(
            ProviderKind::from_model_name("GEMINI-PRO"),
            ProviderKind::Gemini
        );
    }

    #[test]
    fn provider_kind_from_str() {
        assert_eq!(
            ProviderKind::from_str("qwen3").unwrap(),
            ProviderKind::Qwen3
        );
        assert_eq!(
            ProviderKind::from_str("anthropic").unwrap(),
            ProviderKind::Anthropic
        );
        assert_eq!(
            ProviderKind::from_str("claude").unwrap(),
            ProviderKind::Anthropic
        );
        assert_eq!(
            ProviderKind::from_str("gemini").unwrap(),
            ProviderKind::Gemini
        );
    }

    #[test]
    fn provider_kind_from_str_unknown() {
        assert!(ProviderKind::from_str("unknown").is_err());
    }

    #[test]
    fn create_model_client_factory_returns_correct_type() {
        let config = ModelConfig::builder()
            .api_key("sk-test".into())
            .model("claude-sonnet-4".into())
            .build()
            .expect("config");

        let client = create_model_client(ProviderKind::Anthropic, config.clone());
        assert_eq!(client.model_name(), "claude-sonnet-4");

        let config2 = ModelConfig::builder()
            .api_key("sk-test".into())
            .model("gemini-pro".into())
            .build()
            .expect("config");
        let client2 = create_model_client(ProviderKind::Gemini, config2);
        assert_eq!(client2.model_name(), "gemini-pro");

        let config3 = ModelConfig::builder()
            .api_key("sk-test".into())
            .model("qwen3.6-27b".into())
            .build()
            .expect("config");
        let client3 = create_model_client(ProviderKind::Qwen3, config3);
        assert_eq!(client3.model_name(), "qwen3.6-27b");
    }
}
