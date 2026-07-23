//! 模型客户端类型 —— 模型抽象层的共享数据结构
//!
//! 【领域含义】本模块定义了所有模型供应商实现共享的数据类型，
//! 包括工具定义、Token 统计、速率限制配置、重试配置和聊天补全请求/响应格式。
//!
//! These types are used across all provider implementations and define the
//! interface between the agent core and the model backend.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Tool definition
// ---------------------------------------------------------------------------

/// 工具定义 —— 模型可以调用的工具的元数据
///
/// 【领域含义】镜像 OpenAI 的函数调用 Schema。每个工具有一个名称、
/// 一个可读描述和描述其参数的 JSON Schema。
///
/// Mirrors the OpenAI function-calling schema. Each tool has a name, a
/// human-readable description, and a JSON Schema describing its parameters.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    /// 模型用于调用此工具的名称（例如 `"read_file"`）
    pub name: String,
    /// 工具功能的描述，模型用于决定何时调用它
    pub description: String,
    /// 工具参数的 JSON Schema
    pub parameters: serde_json::Value,
}

impl ToolDefinition {
    /// 创建新的工具定义
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

// ---------------------------------------------------------------------------
// Token usage
// ---------------------------------------------------------------------------

/// Token 使用统计 —— 模型 API 调用的 Token 消耗记录
///
/// 【领域含义】记录一次模型 API 调用消耗的提示词 token 数和补全 token 数。
/// 用于成本监控和上下文窗口管理。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    /// 提示词（输入）消耗的 token 数
    pub prompt_tokens: u32,
    /// 补全（输出）生成的 token 数
    pub completion_tokens: u32,
    /// 总计 token = 提示词 + 补全
    pub total_tokens: u32,
}

impl TokenUsage {
    /// 创建新的 token 使用记录
    pub fn new(prompt_tokens: u32, completion_tokens: u32) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        }
    }
}

// ---------------------------------------------------------------------------
// Rate limiter configuration
// ---------------------------------------------------------------------------

/// 速率限制配置 —— 模型供应商的速率限制设置
///
/// 【领域含义】控制模型 API 调用的频率限制。`None` 表示无限制。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// 每分钟最大请求数。`None` 表示无限制
    pub requests_per_minute: Option<u32>,
    /// 每天最大请求数。`None` 表示无限制
    pub requests_per_day: Option<u32>,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_minute: Some(60),
            requests_per_day: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Retry configuration
// ---------------------------------------------------------------------------

/// 重试配置 —— 带指数退避的重试策略
///
/// 【领域含义】配置模型 API 调用失败时的重试行为。
/// 使用指数退避策略：每次重试的延迟 = base_delay * multiplier^(attempt)，
/// 上限为 max_delay。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetryConfig {
    /// 最大重试次数
    pub max_retries: u32,
    /// 第一次重试的基本延迟（毫秒）
    pub base_delay_ms: u64,
    /// 重试之间的最大延迟（毫秒）
    pub max_delay_ms: u64,
    /// 指数退避的乘数（例如 2.0 表示每次尝试翻倍）
    pub multiplier: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 10_000,
            multiplier: 2.0,
        }
    }
}

impl RetryConfig {
    /// 计算给定重试尝试（从 0 开始索引）的延迟
    pub fn delay_for_attempt(&self, attempt: u32) -> std::time::Duration {
        let delay = self.base_delay_ms as f64 * self.multiplier.powi(attempt as i32);
        let delay = delay.min(self.max_delay_ms as f64);
        std::time::Duration::from_millis(delay as u64)
    }
}

// ---------------------------------------------------------------------------
// Chat completion request / response helpers (OpenAI-compatible shapes)
// ---------------------------------------------------------------------------

/// 聊天补全请求体中的单条消息
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "role", rename_all = "lowercase")]
#[allow(dead_code)]
pub(crate) enum ChatMessage {
    #[serde(rename = "system")]
    System {
        content: String,
    },
    #[serde(rename = "user")]
    User {
        content: String,
    },
    #[serde(rename = "assistant")]
    Assistant {
        content: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ChatToolCall>>,
    },
    #[serde(rename = "tool")]
    Tool {
        content: String,
        tool_call_id: String,
    },
}

/// 聊天补全请求/响应中序列化的工具调用
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ChatToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ChatFunctionCall,
}

/// 工具调用的函数部分
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ChatFunctionCall {
    pub name: String,
    pub arguments: String,
}

/// 聊天补全请求中序列化的工具定义
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChatToolDef {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ChatFunctionDef,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChatFunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// 完整的聊天补全请求体（OpenAI 兼容格式）
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ChatToolDef>>,
    pub stream: bool,
    pub temperature: f32,
    pub max_tokens: u32,
}

/// OpenAI 兼容聊天补全 API 的流式数据块
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ChatCompletionChunk {
    #[serde(default)]
    pub choices: Vec<ChunkChoice>,
    #[serde(default)]
    pub usage: Option<ChunkUsage>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ChunkChoice {
    #[serde(default)]
    #[allow(dead_code)]
    pub index: u32,
    #[serde(default)]
    pub delta: ChunkDelta,
    #[serde(default)]
    #[allow(dead_code)]
    pub finish_reason: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct ChunkDelta {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ChunkToolCall>>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ChunkToolCall {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<ChunkFunctionDelta>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ChunkFunctionDelta {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct ChunkUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// 非流式聊天补全响应
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ChatCompletionResponse {
    pub choices: Vec<ResponseChoice>,
    #[serde(default)]
    pub usage: Option<ChunkUsage>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ResponseChoice {
    pub message: ResponseMessage,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct ResponseMessage {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ChatToolCall>>,
}

// ---------------------------------------------------------------------------
// Embedding types
// ---------------------------------------------------------------------------

/// 嵌入请求体
#[derive(Clone, Debug, Serialize)]
pub(crate) struct EmbeddingRequest {
    pub model: String,
    pub input: serde_json::Value,
}

/// 嵌入响应体
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct EmbeddingResponse {
    pub data: Vec<EmbeddingData>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct EmbeddingData {
    pub embedding: Vec<f32>,
}

// ---------------------------------------------------------------------------
// Reranker types
// ---------------------------------------------------------------------------

/// 重排序请求体
#[derive(Clone, Debug, Serialize)]
pub(crate) struct RerankRequest {
    pub model: String,
    pub query: String,
    pub documents: Vec<String>,
}

/// 重排序响应体
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RerankResponse {
    pub results: Vec<RerankResult>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RerankResult {
    pub index: usize,
    pub relevance_score: f32,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definition_serialization() {
        let tool = ToolDefinition::new(
            "read_file",
            "Read a file from disk",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
        );
        let json = serde_json::to_string(&tool).expect("serialize");
        let parsed: ToolDefinition = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(tool, parsed);
    }

    #[test]
    fn token_usage_total_auto_calculated() {
        let usage = TokenUsage::new(100, 50);
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.total_tokens, 150);
    }

    #[test]
    fn retry_config_delay_grows_exponentially() {
        let config = RetryConfig::default();
        let d0 = config.delay_for_attempt(0);
        let d1 = config.delay_for_attempt(1);
        let d2 = config.delay_for_attempt(2);
        assert_eq!(d0.as_millis(), 1000);
        assert_eq!(d1.as_millis(), 2000);
        assert_eq!(d2.as_millis(), 4000);
    }

    #[test]
    fn retry_config_clamps_to_max() {
        let config = RetryConfig {
            max_retries: 5,
            base_delay_ms: 10_000,
            max_delay_ms: 30_000,
            multiplier: 2.0,
        };
        let d = config.delay_for_attempt(2); // 10s * 4 = 40s → clamped to 30s
        assert_eq!(d.as_millis(), 30_000);
    }

    #[test]
    fn chat_message_system_serialization() {
        let msg = ChatMessage::System {
            content: "You are a helpful assistant.".into(),
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        assert!(json.contains("\"role\":\"system\""));
        assert!(json.contains("You are a helpful assistant."));
    }

    #[test]
    fn chat_message_user_serialization() {
        let msg = ChatMessage::User {
            content: "Hello!".into(),
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("Hello!"));
    }

    #[test]
    fn chat_completion_request_serialization() {
        let req = ChatCompletionRequest {
            model: "test-model".into(),
            messages: vec![
                ChatMessage::User {
                    content: "hi".into(),
                },
            ],
            tools: None,
            stream: true,
            temperature: 0.2,
            max_tokens: 4096,
        };
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(json.contains("\"model\":\"test-model\""));
        assert!(json.contains("\"stream\":true"));
        assert!(json.contains("\"temperature\":0.2"));
    }
}
