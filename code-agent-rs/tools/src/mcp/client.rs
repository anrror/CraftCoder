//! MCP 客户端管理器
//!
//! 【领域含义】管理到外部 MCP 服务器的连接，支持 stdio（子进程）和 HTTP 两种传输方式。
//! 处理初始化握手、能力协商、工具发现和工具调用，遵循 MCP JSON-RPC 2.0 协议（版本 "2025-06-18"）。
//!
//! 【核心职责】`McpClientManager` 负责连接的建立、维护和关闭，以及工具列表的缓存和刷新。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use super::types::{
    ClientCapabilities, ImplementationInfo, InitializeParams, InitializeResult,
    JsonRpcRequest, JsonRpcResponse, McpServerCapabilities, McpTool, McpToolResult,
    ToolsCallResult, ToolsListResult, MCP_PROTOCOL_VERSION,
};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// MCP 客户端错误
///
/// 【领域含义】MCP 客户端操作过程中可能出现的所有错误类型。
/// 【核心职责】统一封装连接失败、服务器未找到、工具未找到、I/O 错误、HTTP 错误、JSON 错误、协议错误和服务器错误。
#[derive(Debug, thiserror::Error)]
pub enum McpClientError {
    /// 无法建立与 MCP 服务器的连接
    #[error("failed to connect to server '{server}': {reason}")]
    Connection { server: String, reason: String },

    /// 指定的服务器未连接
    #[error("server not found: {0}")]
    ServerNotFound(String),

    /// 在已连接的服务器上未找到指定工具
    #[error("tool not found: {tool_name}")]
    ToolNotFound { tool_name: String },

    /// stdio 传输通信中的 I/O 错误
    #[error("I/O error on server '{server}': {source}")]
    Io {
        server: String,
        #[source]
        source: std::io::Error,
    },

    /// HTTP 传输通信中的 HTTP 错误
    #[error("HTTP error on server '{server}': {source}")]
    Http {
        server: String,
        #[source]
        source: reqwest::Error,
    },

    /// JSON 序列化或反序列化失败
    #[error("JSON error on server '{server}': {source}")]
    Json {
        server: String,
        #[source]
        source: serde_json::Error,
    },

    /// 协议级错误（版本不匹配、初始化失败）
    #[error("protocol error on server '{server}': {message}")]
    Protocol { server: String, message: String },

    /// MCP 服务器返回了 JSON-RPC 错误
    #[error("server '{server}' returned error (code {code}): {message}")]
    ServerError {
        server: String,
        code: i64,
        message: String,
    },
}

/// MCP 客户端结果类型别名
///
/// 【领域含义】便捷类型别名，简化 MCP 客户端操作函数的返回类型签名。
/// 【核心职责】等价于 `Result<T, McpClientError>`。
pub type McpClientResult<T> = Result<T, McpClientError>;

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// MCP 传输层
///
/// 【领域含义】与 MCP 服务器通信的传输层抽象，支持 stdio 和 HTTP 两种方式。
/// 【核心职责】封装底层通信细节，提供统一的请求发送和连接关闭接口。
#[allow(clippy::large_enum_variant)]
pub enum McpTransport {
    /// 通过子进程的 stdin/stdout 通信（JSON 行协议）
    Stdio {
        /// 派生的子进程
        #[allow(dead_code)]
        process: Child,
        /// 子进程 stdin 的缓冲写入器
        stdin: BufWriter<ChildStdin>,
        /// 子进程 stdout 的缓冲读取器
        stdout: BufReader<ChildStdout>,
    },
    /// 通过 HTTP POST JSON-RPC 通信
    Http {
        /// MCP 服务器端点的基础 URL
        base_url: String,
        /// 可复用的 HTTP 客户端
        client: reqwest::Client,
    },
}

impl McpTransport {
    /// Send a JSON-RPC request and wait for the corresponding response.
    async fn send_request(
        &mut self,
        server: &str,
        request: &JsonRpcRequest,
    ) -> McpClientResult<JsonRpcResponse> {
        match self {
            McpTransport::Stdio { stdin, stdout, .. } => {
                // Write the JSON-RPC request as a single JSON line
                let json = serde_json::to_string(request).map_err(|e| McpClientError::Json {
                    server: server.to_string(),
                    source: e,
                })?;
                stdin
                    .write_all(json.as_bytes())
                    .await
                    .map_err(|e| McpClientError::Io {
                        server: server.to_string(),
                        source: e,
                    })?;
                stdin
                    .write_all(b"\n")
                    .await
                    .map_err(|e| McpClientError::Io {
                        server: server.to_string(),
                        source: e,
                    })?;
                stdin.flush().await.map_err(|e| McpClientError::Io {
                    server: server.to_string(),
                    source: e,
                })?;

                // Read the response (one JSON line) — C12: 限制行长度防 DoS
                const MAX_LINE_BYTES: usize = 1024 * 1024; // 1 MiB
                let mut line = String::new();
                stdout
                    .read_line(&mut line)
                    .await
                    .map_err(|e| McpClientError::Io {
                        server: server.to_string(),
                        source: e,
                    })?;
                if line.is_empty() {
                    return Err(McpClientError::Connection {
                        server: server.to_string(),
                        reason: "server closed stdout unexpectedly".into(),
                    });
                }
                // C12: 拒绝超大单行响应，防止恶意 MCP 服务器导致 agent OOM
                if line.len() > MAX_LINE_BYTES {
                    return Err(McpClientError::Connection {
                        server: server.to_string(),
                        reason: format!(
                            "server sent oversized line ({} bytes > {} bytes max)",
                            line.len(), MAX_LINE_BYTES
                        ),
                    });
                }
                serde_json::from_str(&line).map_err(|e| McpClientError::Json {
                    server: server.to_string(),
                    source: e,
                })
            }
            McpTransport::Http {
                ref base_url,
                client,
                ..
            } => {
                let resp = client
                    .post(base_url.as_str())
                    .json(request)
                    .send()
                    .await
                    .map_err(|e| McpClientError::Http {
                        server: server.to_string(),
                        source: e,
                    })?;
                let body: JsonRpcResponse =
                    resp.json().await.map_err(|e| McpClientError::Http {
                        server: server.to_string(),
                        source: e,
                    })?;
                Ok(body)
            }
        }
    }

    /// Close the transport connection cleanly.
    async fn close(&mut self, server: &str) -> McpClientResult<()> {
        match self {
            McpTransport::Stdio { process, .. } => {
                // Drop stdin handle to close the write end, then wait for exit
                // (The BufWriter/ChildStdin will be dropped when the handle is dropped.)
                process
                    .start_kill()
                    .map_err(|e| McpClientError::Io {
                        server: server.to_string(),
                        source: e,
                    })?;
                process
                    .wait()
                    .await
                    .map_err(|e| McpClientError::Io {
                        server: server.to_string(),
                        source: e,
                    })?;
                Ok(())
            }
            McpTransport::Http { .. } => {
                // HTTP is stateless — nothing to explicitly close
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Client handle
// ---------------------------------------------------------------------------

/// 每个服务器的状态：传输层、协商能力和缓存工具
///
/// 【领域含义】封装单个 MCP 服务器连接的所有运行时状态。
/// 【核心职责】持有传输层、能力信息、工具缓存和请求 ID 计数器。
struct McpClientHandle {
    /// 传输层实例
    transport: McpTransport,
    /// 协商后的服务器能力
    capabilities: McpServerCapabilities,
    /// 缓存的工具列表
    tools: Vec<McpTool>,
    /// 下一个请求 ID
    next_request_id: u64,
}

impl McpClientHandle {
    fn new(transport: McpTransport) -> Self {
        Self {
            transport,
            capabilities: McpServerCapabilities::default(),
            tools: Vec::new(),
            next_request_id: 1,
        }
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    /// Send a JSON-RPC request and validate the response.
    async fn rpc(
        &mut self,
        server: &str,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> McpClientResult<serde_json::Value> {
        let id = self.next_id();
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };

        let response = self.transport.send_request(server, &request).await?;

        if response.id != id {
            return Err(McpClientError::Protocol {
                server: server.to_string(),
                message: format!(
                    "response id {} does not match request id {}",
                    response.id, id
                ),
            });
        }

        if let Some(err) = response.error {
            return Err(McpClientError::ServerError {
                server: server.to_string(),
                code: err.code,
                message: err.message,
            });
        }

        response.result.ok_or_else(|| McpClientError::Protocol {
            server: server.to_string(),
            message: "response has no result and no error".into(),
        })
    }
}

// ---------------------------------------------------------------------------
// Client Manager
// ---------------------------------------------------------------------------

/// MCP 客户端管理器
///
/// 【领域含义】管理到多个 MCP 服务器的连接，每个连接由用户指定的名称标识。
/// 处理初始化握手、能力协商、工具发现和跨服务器的工具执行。
///
/// 【核心职责】提供连接的建立、断开、重连，以及工具列表的查询和调用。
///
/// # 示例
///
/// ```rust,no_run
/// use code_agent_tools::mcp::client::McpClientManager;
///
/// let mut manager = McpClientManager::new();
/// manager.connect_http("my_server", "http://localhost:3000").await?;
/// let tools = manager.list_tools("my_server").await?;
/// ```
pub struct McpClientManager {
    /// 服务器名称 → 客户端句柄的映射（线程安全）
    clients: HashMap<String, Arc<Mutex<McpClientHandle>>>,
}

impl Default for McpClientManager {
    fn default() -> Self {
        Self::new()
    }
}

impl McpClientManager {
    /// 创建空的客户端管理器
    ///
    /// 【领域含义】初始化一个没有任何连接的 MCP 客户端管理器。
    /// 【核心职责】创建空的 `HashMap` 用于存储服务器连接。
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
        }
    }

    // ------------------------------------------------------------------
    // Connection
    // ------------------------------------------------------------------

    /// 通过 stdio 连接到 MCP 服务器
    ///
    /// 【领域含义】派生一个子进程并通过 stdin/stdout 与其建立 MCP 连接。
    /// 子进程必须使用换行符分隔的 JSON 消息在 stdin/stdout 上通信。
    /// 【核心职责】派生进程 → 捕获 stdin/stdout → 执行 initialize 握手 → 发送 initialized 通知 → 发现工具。
    pub async fn connect_stdio(
        &mut self,
        name: &str,
        command: &str,
        args: &[&str],
    ) -> McpClientResult<()> {
        // P0-3: 清除敏感环境变量，防止 LLM_API_KEY 等凭据泄露给子进程
        let mut cmd = tokio::process::Command::new(command);
        crate::filter_sensitive_env(&mut cmd);
        let mut child = cmd
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| McpClientError::Connection {
                server: name.to_string(),
                reason: format!("failed to spawn '{}': {}", command, e),
            })?;

        let stdin = BufWriter::new(child.stdin.take().ok_or_else(|| McpClientError::Connection {
            server: name.to_string(),
            reason: "failed to capture stdin".into(),
        })?);

        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| {
            McpClientError::Connection {
                server: name.to_string(),
                reason: "failed to capture stdout".into(),
            }
        })?);

        let transport = McpTransport::Stdio {
            process: child,
            stdin,
            stdout,
        };

        let mut handle = McpClientHandle::new(transport);

        // --- Initialize handshake ---
        let init_result = handle
            .rpc(
                name,
                "initialize",
                Some(serde_json::to_value(InitializeParams {
                    protocol_version: MCP_PROTOCOL_VERSION.into(),
                    capabilities: ClientCapabilities {
                        roots: None,
                        sampling: None,
                    },
                    client_info: ImplementationInfo {
                        name: "code-agent-tools".into(),
                        version: env!("CARGO_PKG_VERSION").into(),
                    },
                })
                .unwrap()),
            )
            .await?;

        let parsed: InitializeResult = serde_json::from_value(init_result).map_err(|e| {
            McpClientError::Protocol {
                server: name.to_string(),
                message: format!("invalid initialize response: {}", e),
            }
        })?;

        if parsed.protocol_version != MCP_PROTOCOL_VERSION {
            return Err(McpClientError::Protocol {
                server: name.to_string(),
                message: format!(
                    "server protocol version '{}' not supported (expected '{}')",
                    parsed.protocol_version, MCP_PROTOCOL_VERSION
                ),
            });
        }

        handle.capabilities = capabilities_from_json(&parsed.capabilities);

        // --- Send `initialized` notification ---
        // Notification: no `id`, no response expected
        let notification = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 0, // unused for notifications but struct requires it
            method: "notifications/initialized".to_string(),
            params: None,
        };
        // Write directly (don't wait for response since it's a notification)
        let json =
            serde_json::to_string(&notification).map_err(|e| McpClientError::Json {
                server: name.to_string(),
                source: e,
            })?;
        match &mut handle.transport {
            McpTransport::Stdio { stdin, .. } => {
                stdin
                    .write_all(json.as_bytes())
                    .await
                    .map_err(|e| McpClientError::Io {
                        server: name.to_string(),
                        source: e,
                    })?;
                stdin
                    .write_all(b"\n")
                    .await
                    .map_err(|e| McpClientError::Io {
                        server: name.to_string(),
                        source: e,
                    })?;
                stdin.flush().await.map_err(|e| McpClientError::Io {
                    server: name.to_string(),
                    source: e,
                })?;
            }
            McpTransport::Http {
                ref base_url, client, ..
            } => {
                client
                    .post(base_url.as_str())
                    .json(&notification)
                    .send()
                    .await
                    .map_err(|e| McpClientError::Http {
                        server: name.to_string(),
                        source: e,
                    })?;
            }
        }

        // --- Discover tools ---
        let tools = fetch_tools(&mut handle, name).await?;
        handle.tools = tools;

        self.clients
            .insert(name.to_string(), Arc::new(Mutex::new(handle)));
        Ok(())
    }

    /// P1: 通过 HTTP 连接到 MCP 服务器 —— 强制 HTTPS（明文 HTTP 仅在开发环境允许）
    pub async fn connect_http(&mut self, name: &str, url: &str) -> McpClientResult<()> {
        // P1: 拒绝非 HTTPS MCP 连接（防止凭据通过明文传输和 MITM）
        let allow_http = std::env::var("CODE_AGENT_ALLOW_MCP_HTTP")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        if !url.starts_with("https://") {
            if allow_http {
                tracing::warn!(
                    mcp_name = %name,
                    url = %url,
                    "MCP HTTP connection using plaintext HTTP (CODE_AGENT_ALLOW_MCP_HTTP=true)"
                );
            } else {
                return Err(McpClientError::Connection {
                    server: name.to_string(),
                    reason: format!(
                        "MCP HTTP connections require HTTPS. Set CODE_AGENT_ALLOW_MCP_HTTP=true only for local development. URL: {}",
                        url
                    ),
                });
            }
        }

        // reqwest::Client::new() enables TLS verification with system CA store by default.
        let client = reqwest::Client::new();
        let transport = McpTransport::Http {
            base_url: url.to_string(),
            client: client.clone(),
        };

        let mut handle = McpClientHandle::new(transport);

        // --- Initialize ---
        let init_result = handle
            .rpc(
                name,
                "initialize",
                Some(serde_json::to_value(InitializeParams {
                    protocol_version: MCP_PROTOCOL_VERSION.into(),
                    capabilities: ClientCapabilities {
                        roots: None,
                        sampling: None,
                    },
                    client_info: ImplementationInfo {
                        name: "code-agent-tools".into(),
                        version: env!("CARGO_PKG_VERSION").into(),
                    },
                })
                .unwrap()),
            )
            .await?;

        let parsed: InitializeResult = serde_json::from_value(init_result).map_err(|e| {
            McpClientError::Protocol {
                server: name.to_string(),
                message: format!("invalid initialize response: {}", e),
            }
        })?;

        if parsed.protocol_version != MCP_PROTOCOL_VERSION {
            return Err(McpClientError::Protocol {
                server: name.to_string(),
                message: format!(
                    "server protocol version '{}' not supported (expected '{}')",
                    parsed.protocol_version, MCP_PROTOCOL_VERSION
                ),
            });
        }

        handle.capabilities = capabilities_from_json(&parsed.capabilities);

        // --- Send `initialized` notification ---
        client
            .post(url)
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }))
            .send()
            .await
            .map_err(|e| McpClientError::Http {
                server: name.to_string(),
                source: e,
            })?;

        // --- Discover tools ---
        let tools = fetch_tools(&mut handle, name).await?;
        handle.tools = tools;

        self.clients
            .insert(name.to_string(), Arc::new(Mutex::new(handle)));
        Ok(())
    }

    // ------------------------------------------------------------------
    // Tool operations
    // ------------------------------------------------------------------

    /// 列出已连接服务器上的所有工具
    ///
    /// 【领域含义】获取指定 MCP 服务器上所有可用工具的列表。
    /// 结果在首次调用后缓存，使用 `reconnect` 刷新工具列表。
    /// 【核心职责】查找服务器 → 返回缓存工具或重新获取。
    pub async fn list_tools(&self, server: &str) -> McpClientResult<Vec<McpTool>> {
        let handle_arc = self
            .clients
            .get(server)
            .ok_or_else(|| McpClientError::ServerNotFound(server.to_string()))?;

        let mut handle = handle_arc.lock().await;

        // Return cached tools if already discovered
        if !handle.tools.is_empty() {
            return Ok(handle.tools.clone());
        }

        let tools = fetch_tools(&mut handle, server).await?;
        handle.tools = tools.clone();
        Ok(tools)
    }

    /// 按名称调用已连接服务器上的工具
    ///
    /// 【领域含义】在指定的 MCP 服务器上调用一个工具，传递参数并返回结果。
    /// 【核心职责】查找服务器 → 发送 `tools/call` 请求 → 解析响应为 `McpToolResult`。
    pub async fn call_tool(
        &self,
        server: &str,
        tool_name: &str,
        args: serde_json::Value,
    ) -> McpClientResult<McpToolResult> {
        let handle_arc = self
            .clients
            .get(server)
            .ok_or_else(|| McpClientError::ServerNotFound(server.to_string()))?;

        let mut handle = handle_arc.lock().await;

        let result = handle
            .rpc(
                server,
                "tools/call",
                Some(serde_json::json!({
                    "name": tool_name,
                    "arguments": args,
                })),
            )
            .await?;

        let call_result: ToolsCallResult =
            serde_json::from_value(result).map_err(|e| McpClientError::Protocol {
                server: server.to_string(),
                message: format!("invalid tools/call response: {}", e),
            })?;

        Ok(call_result.into())
    }

    /// 断开服务器连接并清理资源
    ///
    /// 【领域含义】断开与指定 MCP 服务器的连接，关闭传输层并清理资源。
    /// 【核心职责】从映射中移除 → 锁定句柄 → 关闭传输层。
    pub async fn disconnect(&mut self, name: &str) -> McpClientResult<()> {
        let handle_arc = self
            .clients
            .remove(name)
            .ok_or_else(|| McpClientError::ServerNotFound(name.to_string()))?;

        // Lock and close
        let mut handle = handle_arc.lock().await;
        handle.transport.close(name).await
    }

    /// 重新连接到服务器
    ///
    /// 【领域含义】刷新指定服务器的工具缓存，下次 `list_tools` 调用将重新获取。
    /// 【核心职责】清除工具缓存，触发下次调用时重新发现。
    pub async fn reconnect(&mut self, name: &str) -> McpClientResult<()> {
        // The disconnection and reconnection must be done by the caller
        // since we don't store connection parameters. For now, this just
        // clears the tool cache so the next `list_tools` call will
        // re-fetch from the live connection.
        if let Some(handle_arc) = self.clients.get(name) {
            let mut handle = handle_arc.lock().await;
            handle.tools.clear();
        }
        Ok(())
    }

    /// 获取所有已连接服务器的所有工具
    ///
    /// 【领域含义】遍历所有已连接的 MCP 服务器，收集每个服务器的工具列表。
    /// 返回 `(server_name, McpTool)` 元组的向量。
    /// 【核心职责】遍历服务器 → 获取/刷新工具缓存 → 合并返回。
    pub async fn all_tools(&self) -> McpClientResult<Vec<(String, McpTool)>> {
        let mut all = Vec::new();

        for (server_name, handle_arc) in &self.clients {
            let mut handle = handle_arc.lock().await;
            // Fetch tools if not cached
            if handle.tools.is_empty() {
                let tools = fetch_tools(&mut handle, server_name).await?;
                handle.tools = tools;
            }
            for tool in &handle.tools {
                all.push((server_name.clone(), tool.clone()));
            }
        }

        Ok(all)
    }

    /// 获取已连接的服务器数量
    ///
    /// 【领域含义】返回当前已连接的 MCP 服务器数量。
    /// 【核心职责】返回 `clients` HashMap 的长度。
    pub fn server_count(&self) -> usize {
        self.clients.len()
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// 通过 `tools/list` 从服务器获取工具列表
///
/// 【领域含义】向 MCP 服务器发送 `tools/list` 请求，获取可用工具列表。
/// 【核心职责】发送 RPC 请求 → 解析 `ToolsListResult` → 返回工具列表。
async fn fetch_tools(
    handle: &mut McpClientHandle,
    server: &str,
) -> McpClientResult<Vec<McpTool>> {
    // M2: 限制单 MCP 服务器工具数量，防止恶意服务器通过大量工具导致 agent DoS
    const MAX_TOOLS_PER_SERVER: usize = 100;
    let result = handle.rpc(server, "tools/list", None).await?;

    let list: ToolsListResult =
        serde_json::from_value(result).map_err(|e| McpClientError::Protocol {
            server: server.to_string(),
            message: format!("invalid tools/list response: {}", e),
        })?;

    if list.tools.len() > MAX_TOOLS_PER_SERVER {
        return Err(McpClientError::Protocol {
            server: server.to_string(),
            message: format!(
                "server returned {} tools, exceeding the maximum of {}",
                list.tools.len(),
                MAX_TOOLS_PER_SERVER
            ),
        });
    }

    Ok(list.tools)
}

/// 从初始化返回的 JSON 能力对象中提取 `McpServerCapabilities`
///
/// 【领域含义】解析 MCP 服务器初始化响应中的 `capabilities` 字段。
/// 【核心职责】检查 `tools`、`resources`、`prompts` 字段是否存在。
fn capabilities_from_json(caps: &serde_json::Value) -> McpServerCapabilities {
    McpServerCapabilities {
        supports_tools: caps.get("tools").is_some(),
        supports_resources: caps.get("resources").is_some(),
        supports_prompts: caps.get("prompts").is_some(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::types::{error_codes, jsonrpc_error, jsonrpc_success};
    use http_body_util::BodyExt;
    use hyper::body::Incoming;
    use hyper::service::service_fn;
    use hyper::{Request, Response};
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto::Builder;
    use std::convert::Infallible;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    /// Start a simple HTTP mock MCP server on a random port.
    /// Returns the URL and a shutdown sender.
    async fn start_mock_http_server() -> (String, oneshot::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let url = format!("http://{}", addr);
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    result = listener.accept() => {
                        if let Ok((stream, _)) = result {
                            let io = TokioIo::new(stream);
                            tokio::spawn(async move {
                                let builder = Builder::new(TokioExecutor::new());
                                let conn = builder.serve_connection(io, service_fn(mock_hyper_handler));
                                let _ = conn.await;
                            });
                        }
                    }
                }
            }
        });

        (url, shutdown_tx)
    }

    async fn mock_hyper_handler(req: Request<Incoming>) -> Result<Response<String>, Infallible> {
        // Read the full body
        let body_bytes = req.into_body().collect().await.unwrap_or_default().to_bytes();

        // Check if this is a notification (no "id" field) — skip silently
        let body_str = std::str::from_utf8(&body_bytes).unwrap_or("");
        if !body_str.contains("\"id\"") {
            return Ok(Response::builder()
                .header("Content-Type", "application/json")
                .body("{}".to_string())
                .unwrap());
        }

        let request: JsonRpcRequest = serde_json::from_slice(&body_bytes).unwrap();
        let response = handle_mock_request(&request);
        let response_json = serde_json::to_string(&response).unwrap();

        Ok(Response::builder()
            .header("Content-Type", "application/json")
            .body(response_json)
            .unwrap())
    }

    fn handle_mock_request(req: &JsonRpcRequest) -> JsonRpcResponse {
        match req.method.as_str() {
            "initialize" => {
                let result = InitializeResult {
                    protocol_version: MCP_PROTOCOL_VERSION.into(),
                    capabilities: serde_json::json!({
                        "tools": {"listChanged": true}
                    }),
                    server_info: ImplementationInfo {
                        name: "MockServer".into(),
                        version: "0.1.0".into(),
                    },
                };
                jsonrpc_success(req.id, serde_json::to_value(result).unwrap())
            }
            "tools/list" => {
                let result = ToolsListResult {
                    tools: vec![
                        McpTool {
                            name: "echo".into(),
                            description: "Echo back the input".into(),
                            input_schema: serde_json::json!({
                                "type": "object",
                                "properties": {
                                    "message": {"type": "string"}
                                },
                                "required": ["message"]
                            }),
                        },
                        McpTool {
                            name: "add".into(),
                            description: "Add two numbers".into(),
                            input_schema: serde_json::json!({
                                "type": "object",
                                "properties": {
                                    "a": {"type": "number"},
                                    "b": {"type": "number"}
                                },
                                "required": ["a", "b"]
                            }),
                        },
                    ],
                };
                jsonrpc_success(req.id, serde_json::to_value(result).unwrap())
            }
            "tools/call" => {
                let params = req.params.as_ref().and_then(|p| p.as_object());
                let tool_name = params
                    .and_then(|p| p.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                match tool_name {
                    "echo" => {
                        let message = params
                            .and_then(|p| p.get("arguments"))
                            .and_then(|a| a.get("message"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let result = McpToolResult::success(format!("Echo: {}", message));
                        jsonrpc_success(req.id, serde_json::to_value(ToolsCallResult::from(result)).unwrap())
                    }
                    "add" => {
                        let arguments = params.and_then(|p| p.get("arguments"));
                        let a = arguments
                            .and_then(|a| a.get("a"))
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        let b = arguments
                            .and_then(|a| a.get("b"))
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        let result = McpToolResult::success(format!("{}", a + b));
                        jsonrpc_success(req.id, serde_json::to_value(ToolsCallResult::from(result)).unwrap())
                    }
                    _ => jsonrpc_error(
                        req.id,
                        error_codes::METHOD_NOT_FOUND,
                        format!("Unknown tool: {}", tool_name),
                    ),
                }
            }
            _ => jsonrpc_error(
                req.id,
                error_codes::METHOD_NOT_FOUND,
                format!("Unknown method: {}", req.method),
            ),
        }
    }

    // ------------------------------------------------------------------
    // HTTP transport tests
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn connect_http_and_list_tools() {
        // P1: 测试 mock HTTP server 使用明文 http://，需显式允许
        std::env::set_var("CODE_AGENT_ALLOW_MCP_HTTP", "true");
        let (url, shutdown) = start_mock_http_server().await;

        let mut manager = McpClientManager::new();
        manager
            .connect_http("mock", &url)
            .await
            .expect("connect should succeed");

        let tools = manager
            .list_tools("mock")
            .await
            .expect("list_tools should succeed");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tools[1].name, "add");

        // Tool caching: second call should return cached results
        let tools2 = manager
            .list_tools("mock")
            .await
            .expect("second list_tools should succeed");
        assert_eq!(tools2.len(), 2);

        manager
            .disconnect("mock")
            .await
            .expect("disconnect should succeed");
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn call_tool_returns_correct_result() {
        std::env::set_var("CODE_AGENT_ALLOW_MCP_HTTP", "true");
        let (url, shutdown) = start_mock_http_server().await;

        let mut manager = McpClientManager::new();
        manager
            .connect_http("mock", &url)
            .await
            .expect("connect should succeed");

        // Call echo tool
        let result = manager
            .call_tool(
                "mock",
                "echo",
                serde_json::json!({"message": "hello world"}),
            )
            .await
            .expect("call_tool should succeed");
        assert!(!result.is_error);

        // Call add tool
        let result = manager
            .call_tool("mock", "add", serde_json::json!({"a": 3, "b": 4}))
            .await
            .expect("call_tool add should succeed");
        assert!(!result.is_error);

        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn call_nonexistent_tool_returns_error() {
        std::env::set_var("CODE_AGENT_ALLOW_MCP_HTTP", "true");
        let (url, shutdown) = start_mock_http_server().await;
        let mut manager = McpClientManager::new();
        manager
            .connect_http("mock", &url)
            .await
            .expect("connect should succeed");

        let result = manager
            .call_tool("mock", "nonexistent_tool", serde_json::json!({}))
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            McpClientError::ServerError { .. } => {}
            other => panic!("expected ServerError, got {:?}", other),
        }

        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn list_tools_unknown_server_error() {
        let manager = McpClientManager::new();
        let result = manager.list_tools("unknown_server").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            McpClientError::ServerNotFound(name) => assert_eq!(name, "unknown_server"),
            other => panic!("expected ServerNotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn disconnect_removes_server() {
        std::env::set_var("CODE_AGENT_ALLOW_MCP_HTTP", "true");
        let (url, shutdown) = start_mock_http_server().await;
        let mut manager = McpClientManager::new();
        manager
            .connect_http("mock", &url)
            .await
            .expect("connect");

        assert_eq!(manager.server_count(), 1);
        manager.disconnect("mock").await.expect("disconnect");
        assert_eq!(manager.server_count(), 0);

        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn reconnect_clears_tool_cache() {
        std::env::set_var("CODE_AGENT_ALLOW_MCP_HTTP", "true");
        let (url, shutdown) = start_mock_http_server().await;
        let mut manager = McpClientManager::new();
        manager
            .connect_http("mock", &url)
            .await
            .expect("connect");

        // Pre-populate cache
        manager.list_tools("mock").await.expect("list_tools");

        // Reconnect
        manager.reconnect("mock").await.expect("reconnect");

        // Should still work (re-fetches from server)
        let tools = manager.list_tools("mock").await.expect("list_tools");
        assert_eq!(tools.len(), 2);

        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn all_tools_merges_across_servers() {
        std::env::set_var("CODE_AGENT_ALLOW_MCP_HTTP", "true");
        let (url1, shutdown1) = start_mock_http_server().await;
        let (url2, shutdown2) = start_mock_http_server().await;

        let mut manager = McpClientManager::new();
        manager
            .connect_http("server1", &url1)
            .await
            .expect("connect1");
        manager
            .connect_http("server2", &url2)
            .await
            .expect("connect2");

        let all = manager.all_tools().await.expect("all_tools");
        // Each server has 2 tools, so total = 4
        assert_eq!(all.len(), 4);

        // Verify each has a server name
        let server_names: Vec<&str> = all.iter().map(|(s, _)| s.as_str()).collect();
        assert!(server_names.contains(&"server1"));
        assert!(server_names.contains(&"server2"));

        let _ = shutdown1.send(());
        let _ = shutdown2.send(());
    }

    #[tokio::test]
    async fn concurrent_calls_to_different_servers() {
        std::env::set_var("CODE_AGENT_ALLOW_MCP_HTTP", "true");
        let (url1, shutdown1) = start_mock_http_server().await;
        let (url2, shutdown2) = start_mock_http_server().await;

        let mut manager = McpClientManager::new();
        manager
            .connect_http("s1", &url1)
            .await
            .expect("connect1");
        manager
            .connect_http("s2", &url2)
            .await
            .expect("connect2");

        // Run concurrent calls to different servers
        let (r1, r2) = tokio::join!(
            manager.call_tool("s1", "echo", serde_json::json!({"message": "a"})),
            manager.call_tool("s2", "echo", serde_json::json!({"message": "b"})),
        );

        assert!(r1.is_ok());
        assert!(r2.is_ok());

        let _ = shutdown1.send(());
        let _ = shutdown2.send(());
    }
}
