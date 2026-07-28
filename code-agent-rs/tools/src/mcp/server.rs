//! MCP 服务器 — 将代理工具暴露给外部 MCP 客户端
//!
//! 【领域含义】`McpServer` 封装 core crate 的 `ToolRegistry`，通过 MCP JSON-RPC 2.0 协议暴露工具。
//! 启动后从 stdin 读取 JSON 行请求，路由处理，将响应写入 stdout。
//!
//! 【核心职责】实现 MCP 协议子集（initialize、tools/list、tools/call），将本地工具注册为 MCP 工具。

use std::io::BufRead;
use std::sync::Arc;

use code_agent_core::tools::registry::ToolRegistry;

use tokio::io::{AsyncWriteExt, BufWriter};

use super::types::{
    error_codes, jsonrpc_error, jsonrpc_success, JsonRpcRequest, JsonRpcResponse, McpTool,
    McpToolResult, ToolsCallResult, ToolsListResult, MCP_PROTOCOL_VERSION,
};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// MCP 服务器错误
///
/// 【领域含义】MCP 服务器操作过程中可能出现的错误类型。
/// 【核心职责】统一封装工具未找到、执行错误、JSON 错误、I/O 错误和无效请求。
#[derive(Debug, thiserror::Error)]
pub enum McpServerError {
    /// 按名称未找到工具
    #[error("tool not found: {0}")]
    ToolNotFound(String),

    /// 工具执行失败
    #[error("tool execution error: {0}")]
    ExecutionError(String),

    /// JSON 序列化或反序列化失败
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// stdio 通信中的 I/O 错误
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// 无效的 JSON-RPC 请求
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

/// MCP 服务器结果类型别名
///
/// 【领域含义】便捷类型别名，简化 MCP 服务器操作函数的返回类型签名。
/// 【核心职责】等价于 `Result<T, McpServerError>`。
pub type McpServerResult<T> = Result<T, McpServerError>;

// ---------------------------------------------------------------------------
// MCP Server
// ---------------------------------------------------------------------------

/// MCP 服务器
///
/// 【领域含义】将 `ToolRegistry` 中的工具通过 MCP 协议暴露给外部客户端。
/// 实现 MCP 规范子集：
/// - `initialize` — 能力协商
/// - `tools/list` — 列出可用工具
/// - `tools/call` — 执行工具
///
/// 通信通过 stdin/stdout 使用换行符分隔的 JSON 进行。
///
/// 【核心职责】注册工具 → 启动 stdio 服务器 → 处理 JSON-RPC 请求 → 返回响应。
///
/// # 示例
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use code_agent_core::tools::registry::ToolRegistry;
/// use code_agent_tools::mcp::server::McpServer;
///
/// let registry = Arc::new(DefaultToolRegistry::new());
/// let mut server = McpServer::new(registry);
/// server.register_tools().unwrap();
/// server.start_stdio().await.unwrap();
/// ```
pub struct McpServer {
    /// 注册的 MCP 工具列表
    tools: Vec<McpTool>,
    /// 底层工具注册表引用
    registry: Arc<dyn ToolRegistry>,
}

impl McpServer {
    /// 创建 MCP 服务器
    ///
    /// 【领域含义】使用给定的工具注册表创建 MCP 服务器实例。
    /// 启动前需要调用 `register_tools` 填充工具列表。
    /// 【核心职责】保存注册表引用，初始化空工具列表。
    pub fn new(registry: Arc<dyn ToolRegistry>) -> Self {
        Self {
            tools: Vec::new(),
            registry,
        }
    }

    /// 注册所有工具为 MCP 工具
    ///
    /// 【领域含义】将内部 `ToolRegistry` 中的所有工具定义转换为 `McpTool` 条目。
    /// 【核心职责】遍历注册表 → 转换为 `McpTool` → 存入 `self.tools`。
    pub fn register_tools(&mut self) -> McpServerResult<()> {
        self.tools = self
            .registry
            .list()
            .into_iter()
            .map(|def| McpTool {
                name: def.name,
                description: def.description,
                input_schema: def.input_schema,
            })
            .collect();
        Ok(())
    }

    /// 启动 MCP 服务器（stdio 传输）
    ///
    /// 【领域含义】从 stdin 读取 JSON-RPC 请求，处理后写入 stdout 响应。
    /// 此方法阻塞直到 stdin 关闭或发生错误。
    /// 【核心职责】启动后台线程读取 stdin → 循环处理请求 → 写入响应到 stdout。
    pub async fn start_stdio(&self) -> McpServerResult<()> {
        // Read from stdin line-by-line using a blocking reader wrapped for async.
        // We spawn a blocking task to read lines and send them through a channel.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let stdin_handle = std::io::stdin();
        std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stdin_handle.lock());
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break; // receiver dropped
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let stdout = tokio::io::stdout();
        let mut writer = BufWriter::new(stdout);

        while let Some(line) = rx.recv().await {
            if line.trim().is_empty() {
                continue;
            }

            let request: JsonRpcRequest = serde_json::from_str(&line)
                .map_err(|e| McpServerError::InvalidRequest(format!("invalid JSON: {}", e)))?;

            let response = self.handle_request(&request).await;
            let response_json = serde_json::to_string(&response)?;

            writer.write_all(response_json.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }

        Ok(())
    }

    /// 处理单个 JSON-RPC 请求并生成响应
    ///
    /// 【领域含义】根据请求的方法名路由到对应的处理函数。
    /// 【核心职责】匹配方法名 → 调用 `handle_initialize` / `handle_tools_list` / `handle_tools_call` → 返回响应。
    async fn handle_request(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        match request.method.as_str() {
            "initialize" => self.handle_initialize(request),
            "tools/list" => self.handle_tools_list(request),
            "tools/call" => self.handle_tools_call(request).await,
            _ => jsonrpc_error(
                request.id,
                error_codes::METHOD_NOT_FOUND,
                format!("Unknown method: {}", request.method),
            ),
        }
    }

    // ------------------------------------------------------------------
    // Request handlers
    // ------------------------------------------------------------------

    /// 处理 `initialize` 请求 — 返回服务器能力
    ///
    /// 【领域含义】响应 MCP 初始化请求，返回协议版本、服务器能力和信息。
    /// 【核心职责】构建能力 JSON → 返回包含 protocolVersion、capabilities、serverInfo 的响应。
    fn handle_initialize(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let capabilities = serde_json::json!({
            "tools": {
                "listChanged": true
            }
        });

        let result = serde_json::json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": capabilities,
            "serverInfo": {
                "name": "code-agent-tools",
                "version": env!("CARGO_PKG_VERSION")
            }
        });

        jsonrpc_success(request.id, result)
    }

    /// 处理 `tools/list` 请求 — 返回所有注册的 MCP 工具
    ///
    /// 【领域含义】响应工具列表查询，返回所有已注册的 MCP 工具定义。
    /// 【核心职责】构建 `ToolsListResult` → 序列化 → 返回成功响应。
    fn handle_tools_list(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let result = ToolsListResult {
            tools: self.tools.clone(),
        };
        jsonrpc_success(request.id, serde_json::to_value(result).unwrap())
    }

    /// 处理 `tools/call` 请求 — 按名称执行工具
    ///
    /// 【领域含义】根据请求中的工具名称和参数，在注册表中查找并执行工具。
    /// 【核心职责】提取参数 → 查找工具 → 阻塞执行 → 返回结果或错误。
    async fn handle_tools_call(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let params = match request.params.as_ref() {
            Some(p) => p,
            None => {
                return jsonrpc_error(
                    request.id,
                    error_codes::INVALID_PARAMS,
                    "missing params",
                )
            }
        };

        let tool_name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => {
                return jsonrpc_error(
                    request.id,
                    error_codes::INVALID_PARAMS,
                    "missing 'name' field",
                )
            }
        };

        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let tool = match self.registry.get(tool_name) {
            Some(t) => t,
            None => {
                let result = McpToolResult::error(format!("Tool not found: {}", tool_name));
                return jsonrpc_success(
                    request.id,
                    serde_json::to_value(ToolsCallResult::from(result)).unwrap(),
                );
            }
        };

        // Execute the tool directly (async context available)
        let result = tool.execute(arguments).await;

        match result {
            Ok(msg) => {
                let mcp_result = if let Some(err) = msg.error {
                    McpToolResult::error(err)
                } else {
                    let text = msg.output.unwrap_or_default();
                    McpToolResult::success(text)
                };
                jsonrpc_success(
                    request.id,
                    serde_json::to_value(ToolsCallResult::from(mcp_result)).unwrap(),
                )
            }
            Err(e) => {
                let mcp_result = McpToolResult::error(e.to_string());
                jsonrpc_success(
                    request.id,
                    serde_json::to_value(ToolsCallResult::from(mcp_result)).unwrap(),
                )
            }
        }
    }

    /// 获取注册的 MCP 工具数量
    ///
    /// 【领域含义】返回当前服务器中已注册的 MCP 工具数量。
    /// 【核心职责】返回 `self.tools` 的长度。
    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// 获取底层工具注册表的引用
    ///
    /// 【领域含义】返回内部 `ToolRegistry` 的引用。
    /// 【核心职责】返回 `&self.registry`。
    pub fn registry(&self) -> &Arc<dyn ToolRegistry> {
        &self.registry
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use code_agent_core::tools::builtin::{glob::GlobTool, read_file::ReadFileTool};
    use code_agent_core::tools::registry::DefaultToolRegistry;
    use serde_json::json;

    fn make_test_registry() -> Arc<dyn ToolRegistry> {
        let mut reg = DefaultToolRegistry::new();
        reg.register(Arc::new(ReadFileTool::default())).unwrap();
        reg.register(Arc::new(GlobTool::default())).unwrap();
        Arc::new(reg)
    }

    fn build_initialize_request(id: u64) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id,
            method: "initialize".into(),
            params: Some(json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0.1.0"}
            })),
        }
    }

    fn build_tools_list_request(id: u64) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id,
            method: "tools/list".into(),
            params: None,
        }
    }

    fn build_tools_call_request(id: u64, name: &str, args: serde_json::Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id,
            method: "tools/call".into(),
            params: Some(json!({
                "name": name,
                "arguments": args,
            })),
        }
    }

    // ------------------------------------------------------------------
    // Unit tests on individual handlers
    // ------------------------------------------------------------------

    #[tokio::test(flavor = "current_thread")]
    async fn initialize_returns_capabilities() {
        let registry = make_test_registry();
        let mut server = McpServer::new(registry);
        server.register_tools().unwrap();

        let req = build_initialize_request(1);
        let resp = server.handle_request(&req).await;

        assert_eq!(resp.id, 1);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert!(result["capabilities"]["tools"]["listChanged"].as_bool().unwrap());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tools_list_returns_registered_tools() {
        let registry = make_test_registry();
        let mut server = McpServer::new(registry);
        server.register_tools().unwrap();

        let req = build_tools_list_request(2);
        let resp = server.handle_request(&req).await;

        assert_eq!(resp.id, 2);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);

        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"glob"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tools_call_executes_tool_and_returns_result() {
        let registry = make_test_registry();
        let mut server = McpServer::new(registry);
        server.register_tools().unwrap();

        let req = build_tools_call_request(
            3,
            "glob",
            json!({"pattern": "*.rs"}),
        );
        let resp = server.handle_request(&req).await;

        assert_eq!(resp.id, 3);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert!(!result["isError"].as_bool().unwrap_or(true));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tools_call_nonexistent_tool_returns_error_result() {
        let registry = make_test_registry();
        let mut server = McpServer::new(registry);
        server.register_tools().unwrap();

        let req = build_tools_call_request(4, "no_such_tool", json!({}));
        let resp = server.handle_request(&req).await;

        assert_eq!(resp.id, 4);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert!(result["isError"].as_bool().unwrap_or(false));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_method_returns_error() {
        let registry = make_test_registry();
        let server = McpServer::new(registry);

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: 5,
            method: "unknown/method".into(),
            params: None,
        };
        let resp = server.handle_request(&req).await;

        assert_eq!(resp.id, 5);
        assert!(resp.result.is_none());
        assert!(resp.error.is_some());
        let err = resp.error.unwrap();
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn register_tools_before_start() {
        let registry = make_test_registry();
        let mut server = McpServer::new(Arc::clone(&registry));

        assert_eq!(server.tool_count(), 0);
        server.register_tools().unwrap();
        assert_eq!(server.tool_count(), 2);
    }

    #[test]
    fn tool_count_matches_registry() {
        let registry = make_test_registry();
        let mut server = McpServer::new(registry);
        server.register_tools().unwrap();
        assert_eq!(server.tool_count(), 2);
    }
}
