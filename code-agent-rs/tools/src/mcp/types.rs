//! MCP（模型上下文协议）类型定义
//!
//! 【领域含义】定义 MCP 通信所需的数据结构，遵循 MCP 规范版本 "2025-06-18" 和 JSON-RPC 2.0。
//!
//! 核心类型：
//! - [`McpTool`]: 客户端和服务器之间交换的工具定义
//! - [`McpToolResult`]: 调用工具的结果
//! - [`McpServerCapabilities`]: 服务器支持的能力
//! - JSON-RPC 信封类型：请求、响应、通知
//!
//! 【核心职责】提供 MCP 协议所有数据结构的序列化/反序列化支持。

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// MCP protocol version
// ---------------------------------------------------------------------------

/// MCP 协议版本
///
/// 【领域含义】此实现支持的 MCP 协议版本标识。
/// 【核心职责】在初始化握手中声明客户端/服务器使用的协议版本。
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 envelope types
// ---------------------------------------------------------------------------

/// JSON-RPC 2.0 请求
///
/// 【领域含义】MCP 通信中的 JSON-RPC 请求消息，包含方法名和参数。
/// 【核心职责】携带请求 ID、方法名和可选参数，用于调用远程方法。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// JSON-RPC 版本标识（固定为 "2.0"）
    pub jsonrpc: String,
    /// 请求 ID
    pub id: u64,
    /// 要调用的方法名
    pub method: String,
    /// 方法参数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 响应（成功或错误）
///
/// 【领域含义】MCP 通信中的 JSON-RPC 响应消息，包含结果或错误。
/// 【核心职责】通过 `id` 与请求关联，携带 `result` 或 `error`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// JSON-RPC 版本标识（固定为 "2.0"）
    pub jsonrpc: String,
    /// 对应请求的 ID
    pub id: u64,
    /// 响应结果（成功时存在）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// 错误信息（失败时存在）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC 2.0 错误对象
///
/// 【领域含义】JSON-RPC 协议中的错误描述，包含错误码和消息。
/// 【核心职责】在响应中携带错误信息，供调用方诊断问题。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// 错误码
    pub code: i64,
    /// 错误描述信息
    pub message: String,
    /// 附加错误数据
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 通知（无 `id` 字段，不需要响应）
///
/// 【领域含义】不需要服务器响应的 JSON-RPC 消息，用于事件通知。
/// 【核心职责】携带方法名和参数，发送后不需要等待响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    /// JSON-RPC 版本标识（固定为 "2.0"）
    pub jsonrpc: String,
    /// 通知方法名
    pub method: String,
    /// 通知参数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Error code constants
// ---------------------------------------------------------------------------

/// 标准 JSON-RPC 错误码
///
/// 【领域含义】JSON-RPC 2.0 规范定义的预定义错误码常量。
/// 【核心职责】提供标准错误码供错误响应使用。
pub mod error_codes {
    /// 无效的请求
    pub const INVALID_REQUEST: i64 = -32600;
    /// 方法不存在或不可用
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// 无效的方法参数
    pub const INVALID_PARAMS: i64 = -32602;
    /// 内部 JSON-RPC 错误
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// 创建成功 JSON-RPC 响应
///
/// 【领域含义】构建一个包含结果的 JSON-RPC 成功响应。
/// 【核心职责】填充 `jsonrpc: "2.0"`、`id`、`result`，`error` 为 `None`。
pub fn jsonrpc_success(id: u64, result: serde_json::Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(result),
        error: None,
    }
}

/// 创建错误 JSON-RPC 响应
///
/// 【领域含义】构建一个包含错误信息的 JSON-RPC 错误响应。
/// 【核心职责】填充 `jsonrpc: "2.0"`、`id`、`error`（含 code 和 message），`result` 为 `None`。
pub fn jsonrpc_error(id: u64, code: i64, message: impl Into<String>) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
            data: None,
        }),
    }
}

// ---------------------------------------------------------------------------
// MCP tool types
// ---------------------------------------------------------------------------

/// MCP 工具定义
///
/// 【领域含义】MCP 协议中服务器暴露的工具定义，包含名称、描述和输入模式。
/// 【核心职责】描述工具的元数据和参数规范，供客户端发现和调用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpTool {
    /// 工具的唯一名称
    pub name: String,
    /// 工具功能的人类可读描述
    #[serde(default)]
    pub description: String,
    /// 描述工具输入参数的 JSON Schema
    pub input_schema: serde_json::Value,
}

/// MCP 工具调用结果
///
/// 【领域含义】调用 MCP 工具后返回的结果，包含内容项和错误标志。
/// 【核心职责】封装工具执行的输出内容，指示执行是否成功。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResult {
    /// 工具产生的一个或多个内容项
    pub content: Vec<McpContentItem>,
    /// 如果为 `true`，工具执行过程中发生了错误
    #[serde(default)]
    pub is_error: bool,
}

/// 工具结果中的单个内容项
///
/// 【领域含义】MCP 工具结果中的一条内容，可以是纯文本或资源引用。
/// 【核心职责】提供内容的多类型支持（文本或资源）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum McpContentItem {
    /// 纯文本内容
    #[serde(rename = "text")]
    Text {
        /// 文本内容
        text: String,
    },
    /// 资源引用及其内容
    #[serde(rename = "resource")]
    Resource {
        /// 资源 URI
        uri: String,
        /// 资源文本内容
        text: String,
        /// MIME 类型
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

impl McpToolResult {
    /// 创建成功的工具结果
    ///
    /// 【领域含义】创建一个包含文本内容的成功工具结果。
    /// 【核心职责】设置 `is_error = false`，内容为单条文本。
    pub fn success(text: impl Into<String>) -> Self {
        Self {
            content: vec![McpContentItem::Text {
                text: text.into(),
            }],
            is_error: false,
        }
    }

    /// 创建错误的工具结果
    ///
    /// 【领域含义】创建一个包含错误文本的工具结果。
    /// 【核心职责】设置 `is_error = true`，内容为错误描述文本。
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            content: vec![McpContentItem::Text {
                text: text.into(),
            }],
            is_error: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Server capabilities
// ---------------------------------------------------------------------------

/// MCP 服务器能力声明
///
/// 【领域含义】MCP 服务器在初始化时向客户端声明其支持的能力。
/// 【核心职责】告知客户端服务器支持的工具、资源和提示功能。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerCapabilities {
    /// 服务器是否支持 `tools` 能力
    pub supports_tools: bool,
    /// 服务器是否支持 `resources` 能力
    pub supports_resources: bool,
    /// 服务器是否支持 `prompts` 能力
    pub supports_prompts: bool,
}

#[allow(clippy::derivable_impls)]
impl Default for McpServerCapabilities {
    fn default() -> Self {
        Self {
            supports_tools: false,
            supports_resources: false,
            supports_prompts: false,
        }
    }
}

// ---------------------------------------------------------------------------
// MCP initialize types
// ---------------------------------------------------------------------------

/// 初始化请求参数
///
/// 【领域含义】客户端在 `initialize` 请求中发送的参数，声明协议版本、能力和实现信息。
/// 【核心职责】携带客户端支持的协议版本、能力和实现信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    /// 客户端支持的协议版本
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    /// 客户端能力声明
    pub capabilities: ClientCapabilities,
    /// 客户端实现信息
    #[serde(rename = "clientInfo")]
    pub client_info: ImplementationInfo,
}

/// 初始化结果
///
/// 【领域含义】服务器对 `initialize` 请求的响应，包含协商后的协议版本和服务能力。
/// 【核心职责】携带服务器将使用的协议版本、能力和实现信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    /// 服务器将使用的协议版本（协商后）
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    /// 服务器能力
    pub capabilities: serde_json::Value,
    /// 服务器实现信息
    #[serde(rename = "serverInfo")]
    pub server_info: ImplementationInfo,
}

/// MCP 实现信息（客户端或服务器）
///
/// 【领域含义】描述 MCP 实现的名称和版本。
/// 【核心职责】在初始化握手中标识通信双方的身份。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImplementationInfo {
    /// 机器可读的名称
    pub name: String,
    /// 人类可读的版本字符串
    pub version: String,
}

/// 客户端能力声明
///
/// 【领域含义】客户端在初始化时向服务器声明其支持的能力。
/// 【核心职责】告知服务器客户端支持的 roots 和 sampling 能力。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClientCapabilities {
    /// Roots 能力（客户端可提供文件系统根目录）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roots: Option<RootsCapability>,
    /// Sampling 能力（客户端可提供 LLM 采样）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<serde_json::Value>,
}

/// Roots 能力
///
/// 【领域含义】指示客户端可以提供文件系统根目录的能力。
/// 【核心职责】声明客户端是否在根目录变更时发送通知。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootsCapability {
    /// 客户端是否在根目录变更时发送通知
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

// ---------------------------------------------------------------------------
// MCP response helpers
// ---------------------------------------------------------------------------

/// `tools/list` 响应结构
///
/// 【领域含义】MCP `tools/list` 请求的响应，包含可用工具列表。
/// 【核心职责】封装工具列表供客户端发现。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsListResult {
    /// 可用工具列表
    pub tools: Vec<McpTool>,
}

/// `tools/call` 响应结构
///
/// 【领域含义】MCP `tools/call` 请求的响应，封装 `McpToolResult`。
/// 【核心职责】携带工具调用的内容结果和错误标志。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsCallResult {
    /// 工具调用产生的内容
    pub content: Vec<McpContentItem>,
    /// 工具调用是否出错
    #[serde(default)]
    #[serde(rename = "isError")]
    pub is_error: bool,
}

impl From<McpToolResult> for ToolsCallResult {
    fn from(r: McpToolResult) -> Self {
        Self {
            content: r.content,
            is_error: r.is_error,
        }
    }
}

impl From<ToolsCallResult> for McpToolResult {
    fn from(r: ToolsCallResult) -> Self {
        Self {
            content: r.content,
            is_error: r.is_error,
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
    fn mcp_tool_serialization() {
        let tool = McpTool {
            name: "get_weather".into(),
            description: "Get weather for a location".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "location": {"type": "string"}
                },
                "required": ["location"]
            }),
        };
        let json = serde_json::to_string(&tool).expect("serialize");
        let parsed: McpTool = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.name, "get_weather");
        assert_eq!(parsed.description, "Get weather for a location");
    }

    #[test]
    fn mcp_tool_result_success() {
        let result = McpToolResult::success("weather: 72°F");
        let json = serde_json::to_string(&result).expect("serialize");
        let parsed: McpToolResult = serde_json::from_str(&json).expect("deserialize");
        assert!(!parsed.is_error);
        assert_eq!(parsed.content.len(), 1);
    }

    #[test]
    fn mcp_tool_result_error() {
        let result = McpToolResult::error("API rate limit exceeded");
        let json = serde_json::to_string(&result).expect("serialize");
        let parsed: McpToolResult = serde_json::from_str(&json).expect("deserialize");
        assert!(parsed.is_error);
    }

    #[test]
    fn jsonrpc_request_round_trip() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: 1,
            method: "tools/list".into(),
            params: None,
        };
        let json = serde_json::to_string(&req).expect("serialize");
        let parsed: JsonRpcRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.id, 1);
        assert_eq!(parsed.method, "tools/list");
    }

    #[test]
    fn jsonrpc_response_success() {
        let resp = jsonrpc_success(42, serde_json::json!({"tools": []}));
        let json = serde_json::to_string(&resp).expect("serialize");
        let parsed: JsonRpcResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.id, 42);
        assert!(parsed.error.is_none());
        assert!(parsed.result.is_some());
    }

    #[test]
    fn jsonrpc_response_error() {
        let resp = jsonrpc_error(7, error_codes::METHOD_NOT_FOUND, "Unknown method");
        let json = serde_json::to_string(&resp).expect("serialize");
        let parsed: JsonRpcResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.id, 7);
        assert!(parsed.error.is_some());
        assert!(parsed.result.is_none());
        let err = parsed.error.unwrap();
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn initialize_params_serialization() {
        let params = InitializeParams {
            protocol_version: MCP_PROTOCOL_VERSION.into(),
            capabilities: ClientCapabilities {
                roots: Some(RootsCapability {
                    list_changed: true,
                }),
                sampling: None,
            },
            client_info: ImplementationInfo {
                name: "TestClient".into(),
                version: "1.0.0".into(),
            },
        };
        let json = serde_json::to_string(&params).expect("serialize");
        assert!(json.contains("2025-06-18"));
        assert!(json.contains("TestClient"));

        let parsed: InitializeParams = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.protocol_version, "2025-06-18");
    }

    #[test]
    fn mcp_content_item_text() {
        let item = McpContentItem::Text {
            text: "hello world".into(),
        };
        let json = serde_json::to_string(&item).expect("serialize");
        assert!(json.contains("\"type\":\"text\""));
        let parsed: McpContentItem = serde_json::from_str(&json).expect("deserialize");
        match parsed {
            McpContentItem::Text { text } => assert_eq!(text, "hello world"),
            _ => panic!("expected Text variant"),
        }
    }

    #[test]
    fn mcp_content_item_resource() {
        let item = McpContentItem::Resource {
            uri: "file:///src/main.rs".into(),
            text: "fn main() {}".into(),
            mime_type: "text/x-rust".into(),
        };
        let json = serde_json::to_string(&item).expect("serialize");
        assert!(json.contains("\"type\":\"resource\""));
        let parsed: McpContentItem = serde_json::from_str(&json).expect("deserialize");
        match parsed {
            McpContentItem::Resource {
                uri,
                text,
                mime_type,
            } => {
                assert_eq!(uri, "file:///src/main.rs");
                assert_eq!(text, "fn main() {}");
                assert_eq!(mime_type, "text/x-rust");
            }
            _ => panic!("expected Resource variant"),
        }
    }
}
