//! JSON-RPC 2.0 protocol types for the App Server.
//!
//! Defines the request/response envelope types, standard error codes,
//! and parameter structs for each supported method.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Standard JSON-RPC 2.0 error codes
// ---------------------------------------------------------------------------

/// JSON-RPC 2.0 standard error codes and server-defined error ranges.
pub mod error_codes {
    /// Invalid JSON was received by the server.
    pub const PARSE_ERROR: i32 = -32700;
    /// The JSON sent is not a valid Request object.
    pub const INVALID_REQUEST: i32 = -32600;
    /// The method does not exist / is not available.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// Invalid method parameter(s).
    pub const INVALID_PARAMS: i32 = -32602;
    /// Internal JSON-RPC error.
    pub const INTERNAL_ERROR: i32 = -32603;

    /// Server-defined: server has not been initialized yet.
    pub const SERVER_NOT_INITIALIZED: i32 = -32002;
    /// Server-defined: thread not found.
    pub const THREAD_NOT_FOUND: i32 = -32003;
    /// Server-defined: thread already exists.
    pub const THREAD_ALREADY_EXISTS: i32 = -32004;
    /// Server-defined: max concurrent threads reached.
    pub const MAX_THREADS_REACHED: i32 = -32005;
}

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 envelope types
// ---------------------------------------------------------------------------

/// JSON-RPC 2.0 请求
///
/// 【领域含义】JSON-RPC 2.0 协议请求信封，是客户端与服务器通信的基本单元。
/// 【核心职责】封装 JSON-RPC 版本、请求 ID、方法名和参数。
/// 根据规范，通知类请求（不期望响应）可省略 id，服务器仅对包含 id 的请求发送响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// JSON-RPC 版本 — 必须为 "2.0"。
    pub jsonrpc: String,
    /// 请求标识符 — 通知类请求为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// 方法名 — 要调用的 RPC 方法名称。
    pub method: String,
    /// 方法参数 — 任意 JSON 值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 响应
///
/// 【领域含义】JSON-RPC 2.0 协议响应信封，result 和 error 有且仅有一个会被填充。
/// 【核心职责】封装 JSON-RPC 版本、请求 ID、成功结果或错误信息。
/// 如果请求是通知（无 id），则不发送响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// JSON-RPC 版本 — 必须为 "2.0"。
    pub jsonrpc: String,
    /// 对应的请求标识符。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// 成功结果。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// 错误信息。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    /// 构建成功响应
    ///
    /// 【领域含义】为指定的请求 ID 构建 JSON-RPC 成功响应。
    /// 【核心职责】设置 result 字段，error 为 None。
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// 构建错误响应
    ///
    /// 【领域含义】为指定的请求 ID 构建 JSON-RPC 错误响应。
    /// 【核心职责】设置 error 字段，包含错误码和消息，result 为 None。
    pub fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    /// 构建空 ID 错误响应
    ///
    /// 【领域含义】构建 id 为 null 的错误响应，用于无法确定原始请求 ID 的解析错误。
    /// 【核心职责】设置 id 为 Value::Null，适用于 JSON 解析失败场景。
    pub fn error_null_id(code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(Value::Null),
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    /// 构建 JSON-RPC 通知
    ///
    /// 【领域含义】构建 JSON-RPC 通知（无 id 的请求），用于事件流推送。
    /// 【核心职责】设置 id 为 None，包含方法名和参数，供流式事件通知使用。
    pub fn notification(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: None,
            result: Some(serde_json::json!({
                "method": method.into(),
                "params": params,
            })),
            error: None,
        }
    }

    /// 判断是否为成功响应
    ///
    /// 【领域含义】判断 JSON-RPC 响应是否为成功响应。
    /// 【核心职责】检查 result 存在且 error 不存在。
    pub fn is_success(&self) -> bool {
        self.result.is_some() && self.error.is_none()
    }

    /// 判断是否为错误响应
    ///
    /// 【领域含义】判断 JSON-RPC 响应是否为错误响应。
    /// 【核心职责】检查 error 存在。
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC error detail
// ---------------------------------------------------------------------------

/// JSON-RPC 错误详情
///
/// 【领域含义】JSON-RPC 错误响应中的详细错误信息值对象。
/// 【核心职责】封装错误码、人类可读的错误描述和可选附加数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// 错误码 — JSON-RPC 标准使用负数（参见 error_codes 模块）。
    pub code: i32,
    /// 错误消息 — 简短的人类可读错误描述。
    pub message: String,
    /// 附加数据 — 关于错误的可选额外信息。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// ---------------------------------------------------------------------------
// Method parameter structs
// ---------------------------------------------------------------------------

/// 初始化参数
///
/// 【领域含义】initialize 方法的参数值对象，包含客户端协议版本和能力声明。
/// 【核心职责】封装客户端支持的协议版本、能力和客户端标识信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    /// 协议版本 — 客户端支持的协议版本。
    #[serde(default = "default_protocol_version")]
    pub protocol_version: String,
    /// 客户端能力 — 任意 JSON 格式的能力声明。
    #[serde(default)]
    pub capabilities: Value,
    /// 客户端标识 — 可选的客户端识别信息。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_info: Option<ClientInfo>,
}

pub(crate) fn default_protocol_version() -> String {
    "0.1.0".into()
}

/// 客户端信息
///
/// 【领域含义】客户端识别信息值对象。
/// 【核心职责】封装客户端名称和可选版本号。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    /// 客户端名称
    pub name: String,
    /// 客户端版本（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// 初始化结果
///
/// 【领域含义】initialize 方法的结果值对象，包含服务器协议版本、标识和能力声明。
/// 【核心职责】封装服务器信息，供客户端了解服务器能力和版本。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    /// 协议版本 — 双方商定的协议版本。
    pub protocol_version: String,
    /// 服务器信息 — 服务器标识。
    pub server_info: ServerInfo,
    /// 服务器能力 — 能力声明。
    pub capabilities: Value,
}

/// 服务器信息
///
/// 【领域含义】服务器标识信息值对象。
/// 【核心职责】封装服务器名称和可选版本号。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    /// 服务器名称
    pub name: String,
    /// 服务器版本（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// 创建线程参数
///
/// 【领域含义】threads/create 方法的参数值对象。
/// 【核心职责】封装线程标识、系统指令和最大迭代次数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateThreadParams {
    /// 线程标识 — 期望的线程 ID。
    pub thread_id: String,
    /// 系统指令 — 线程的可选系统提示。
    #[serde(default)]
    pub system_instructions: String,
    /// 最大迭代次数 — 每轮 ReAct 循环的最大迭代次数（默认 20）。
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,
}

fn default_max_iterations() -> usize {
    20
}

/// 提交轮次参数
///
/// 【领域含义】threads/submitTurn 方法的参数值对象。
/// 【核心职责】封装目标线程 ID 和本轮次的消息列表。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitTurnParams {
    /// 线程 ID — 要提交轮次的目标线程。
    pub thread_id: String,
    /// 消息列表 — 本轮次的消息。
    #[serde(default)]
    pub messages: Vec<code_agent_protocol::Message>,
}

/// 获取线程参数
///
/// 【领域含义】threads/get 方法的参数值对象。
/// 【核心职责】封装要获取的线程 ID。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetThreadParams {
    /// 线程 ID — 要获取的线程标识。
    pub thread_id: String,
}

/// 获取线程结果
///
/// 【领域含义】threads/get 方法的结果值对象。
/// 【核心职责】封装线程 ID、会话 ID、轮次计数、状态和系统指令。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetThreadResult {
    /// 线程 ID
    pub thread_id: String,
    /// 会话 ID
    pub session_id: String,
    /// 轮次计数
    pub turn_count: u64,
    /// 状态
    pub status: String,
    /// 系统指令
    pub system_instructions: String,
}

/// Fork 线程参数
///
/// 【领域含义】threads/fork 方法的参数值对象。
/// 【核心职责】封装源线程 ID 和新线程 ID。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkThreadParams {
    /// 源线程 ID — 要 fork 的源线程。
    pub thread_id: String,
    /// 新线程 ID — fork 后的新线程标识。
    pub new_thread_id: String,
}

/// 归档线程参数
///
/// 【领域含义】threads/archive 方法的参数值对象。
/// 【核心职责】封装要归档的线程 ID。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveThreadParams {
    /// 线程 ID — 要归档的线程标识。
    pub thread_id: String,
}

// ---------------------------------------------------------------------------
// Event notification format
// ---------------------------------------------------------------------------

/// 构建事件通知
///
/// 【领域含义】为 ResponseEvent 构建 JSON-RPC 通知，用于流式推送 Agent 事件。
/// 【核心职责】将 ResponseEvent 序列化为 JSON-RPC 通知格式。
///
/// 通知格式：
/// ```json
/// {"jsonrpc": "2.0", "method": "event", "params": {...}}
/// ```
/// 其中 params 为序列化的 ResponseEvent。
pub fn build_event_notification(
    event: &code_agent_protocol::ResponseEvent,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "event",
        "params": event,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Request / Response round-trip ─────────────────────────────────

    #[test]
    fn jsonrpc_request_round_trip() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(1.into())),
            method: "initialize".into(),
            params: Some(serde_json::json!({"protocolVersion": "0.1.0"})),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: JsonRpcRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.method, "initialize");
        assert_eq!(parsed.id, Some(Value::Number(1.into())));
    }

    #[test]
    fn jsonrpc_response_success() {
        let resp = JsonRpcResponse::success(
            Some(Value::Number(1.into())),
            serde_json::json!({"ok": true}),
        );
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"result\""));
        assert!(json.contains("\"ok\":true"));
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn jsonrpc_response_error() {
        let resp = JsonRpcResponse::error(
            Some(Value::Number(2.into())),
            error_codes::METHOD_NOT_FOUND,
            "Method not found",
        );
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"error\""));
        assert!(json.contains("Method not found"));
    }

    #[test]
    fn jsonrpc_response_error_null_id() {
        let resp = JsonRpcResponse::error_null_id(
            error_codes::PARSE_ERROR,
            "Parse error",
        );
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"id\":null"));
    }

    #[test]
    fn jsonrpc_notification_has_no_id() {
        let notif = JsonRpcResponse::notification(
            "event",
            serde_json::json!({"text": "hello"}),
        );
        let json = serde_json::to_string(&notif).unwrap();
        // Notification should not have an "id" field
        let value: Value = serde_json::from_str(&json).unwrap();
        assert!(value.get("id").is_none() || value.get("id").unwrap().is_null());
    }

    #[test]
    fn jsonrpc_is_success_and_is_error() {
        let success = JsonRpcResponse::success(Some(Value::Number(1.into())), Value::Bool(true));
        assert!(success.is_success());
        assert!(!success.is_error());

        let error = JsonRpcResponse::error(Some(Value::Number(1.into())), -32600, "bad");
        assert!(!error.is_success());
        assert!(error.is_error());
    }

    // ── Notification (request without id) ─────────────────────────────

    #[test]
    fn notification_request_has_no_id() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: None,
            method: "notifications/initialized".into(),
            params: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        assert!(value.get("id").is_none());
    }

    // ── Parameter structs ─────────────────────────────────────────────

    #[test]
    fn create_thread_params_defaults() {
        let params: CreateThreadParams = serde_json::from_str(
            r#"{"thread_id": "t1"}"#,
        )
        .unwrap();
        assert_eq!(params.thread_id, "t1");
        assert_eq!(params.system_instructions, "");
        assert_eq!(params.max_iterations, 20);
    }

    #[test]
    fn create_thread_params_full() {
        let params: CreateThreadParams = serde_json::from_str(
            r#"{"thread_id": "t2", "system_instructions": "You are helpful.", "max_iterations": 15}"#,
        )
        .unwrap();
        assert_eq!(params.thread_id, "t2");
        assert_eq!(params.system_instructions, "You are helpful.");
        assert_eq!(params.max_iterations, 15);
    }

    #[test]
    fn initialize_params_defaults() {
        let params: InitializeParams = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(params.protocol_version, "0.1.0");
    }

    #[test]
    fn initialize_result_round_trip() {
        let result = InitializeResult {
            protocol_version: "0.1.0".into(),
            server_info: ServerInfo {
                name: "code-agent-app-server".into(),
                version: Some("0.1.0".into()),
            },
            capabilities: serde_json::json!({
                "threads": true,
                "streaming": true,
                "forking": true,
            }),
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: InitializeResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.protocol_version, "0.1.0");
        assert_eq!(parsed.server_info.name, "code-agent-app-server");
    }

    #[test]
    fn get_thread_result_round_trip() {
        let result = GetThreadResult {
            thread_id: "t1".into(),
            session_id: "sess-1".into(),
            turn_count: 5,
            status: "active".into(),
            system_instructions: "You are helpful.".into(),
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: GetThreadResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.thread_id, "t1");
        assert_eq!(parsed.turn_count, 5);
    }

    #[test]
    fn build_event_notification_format() {
        let event = code_agent_protocol::ResponseEvent::AgentMessageDelta {
            content: "hello".into(),
        };
        let notif = build_event_notification(&event);
        assert_eq!(notif["jsonrpc"], "2.0");
        assert_eq!(notif["method"], "event");
        assert!(notif["params"]["agent_message_delta"]["content"] == "hello");
    }
}
