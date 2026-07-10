//! MCP（模型上下文协议）集成层
//!
//! 【领域含义】为 AI 编码代理提供统一的工具注册和调用能力，融合本地工具和远程 MCP 服务器工具。
//!
//! 核心组件：
//! - [`McpRegistry`]: 统一注册表，合并本地工具和远程 MCP 工具
//! - [`McpClientManager`]: 连接到外部 MCP 服务器
//! - [`McpServer`]: 将本地工具暴露为 MCP 服务器
//!
//! 【核心职责】`McpRegistry` 将本地 `ToolRegistry` 中的工具与已连接 MCP 服务器发现的工具合并，
//! 提供单一统一的工具目录。

pub mod client;
pub mod server;
pub mod types;

use std::sync::Arc;

use code_agent_core::tools::registry::ToolRegistry;
use tokio::sync::RwLock;

use client::{McpClientManager, McpClientResult};
use server::{McpServer, McpServerError, McpServerResult};
use types::{McpTool, McpToolResult};

// ---------------------------------------------------------------------------
// McpRegistry
// ---------------------------------------------------------------------------

/// 统一 MCP 注册表
///
/// 【领域含义】MCP 集成层的主要入口点，合并本地工具注册表和远程 MCP 服务器工具。
/// 使用 `get_tools` 获取合并后的工具目录，使用 `call_tool` 路由工具调用到正确目标。
///
/// 【核心职责】管理本地工具注册、远程 MCP 服务器连接、工具发现和调用路由。
///
/// # 示例
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use code_agent_core::tools::registry::ToolRegistry;
/// use code_agent_tools::mcp::McpRegistry;
///
/// let local_registry = Arc::new(ToolRegistry::new());
/// let mut mcp = McpRegistry::new(local_registry);
/// mcp.connect_http("remote", "http://localhost:3000").await?;
/// let all_tools = mcp.get_tools().await?;
/// ```
pub struct McpRegistry {
    /// 本地工具注册表（来自 core crate）
    local_registry: Arc<ToolRegistry>,
    /// MCP 客户端管理器（外部服务器连接）
    client_manager: Arc<RwLock<McpClientManager>>,
    /// 可选的 MCP 服务器（用于暴露本地工具）
    server: Option<McpServer>,
}

impl McpRegistry {
    /// 创建 MCP 注册表
    ///
    /// 【领域含义】使用给定的本地工具注册表创建 MCP 注册表实例。
    /// 【核心职责】初始化本地注册表引用、客户端管理器和可选的服务器。
    pub fn new(local_registry: Arc<ToolRegistry>) -> Self {
        Self {
            local_registry,
            client_manager: Arc::new(RwLock::new(McpClientManager::new())),
            server: None,
        }
    }

    // ------------------------------------------------------------------
    // Server management
    // ------------------------------------------------------------------

    /// 构建并注册 MCP 服务器
    ///
    /// 【领域含义】创建一个 MCP 服务器实例，将本地工具注册为 MCP 工具。
    /// 调用此方法后，可通过 `start_server` 启动服务器。
    /// 【核心职责】创建 `McpServer` → 注册工具 → 保存到 `self.server`。
    pub fn create_server(&mut self) -> McpServerResult<()> {
        let mut server = McpServer::new(Arc::clone(&self.local_registry));
        server.register_tools()?;
        self.server = Some(server);
        Ok(())
    }

    /// 启动 MCP 服务器（stdio 传输）
    ///
    /// 【领域含义】通过 stdin/stdout 启动 MCP 服务器，读取 stdin 的 JSON-RPC 请求并写入 stdout 响应。
    /// 这是一个阻塞调用，将持续运行直到 stdin 关闭。
    /// 需要先调用 `create_server`。
    /// 【核心职责】检查服务器已创建 → 调用 `server.start_stdio()`。
    pub async fn start_server(&self) -> McpServerResult<()> {
        match &self.server {
            Some(server) => server.start_stdio().await,
            None => Err(McpServerError::ExecutionError(
                "no server configured; call create_server() first".into(),
            )),
        }
    }

    // ------------------------------------------------------------------
    // Client management (delegated to McpClientManager)
    // ------------------------------------------------------------------

    /// 通过 stdio 连接到外部 MCP 服务器
    ///
    /// 【领域含义】派生一个子进程并通过 stdin/stdout 与其建立 MCP 连接。
    /// 【核心职责】委托给 `McpClientManager::connect_stdio`。
    pub async fn connect_stdio(
        &mut self,
        name: &str,
        command: &str,
        args: &[&str],
    ) -> McpClientResult<()> {
        self.client_manager
            .write()
            .await
            .connect_stdio(name, command, args)
            .await
    }

    /// 通过 HTTP 连接到外部 MCP 服务器
    ///
    /// 【领域含义】通过 HTTP POST 请求与远程 MCP 服务器建立连接。
    /// 【核心职责】委托给 `McpClientManager::connect_http`。
    pub async fn connect_http(&mut self, name: &str, url: &str) -> McpClientResult<()> {
        self.client_manager
            .write()
            .await
            .connect_http(name, url)
            .await
    }

    /// 断开 MCP 服务器连接
    ///
    /// 【领域含义】断开与指定 MCP 服务器的连接并清理资源。
    /// 【核心职责】委托给 `McpClientManager::disconnect`。
    pub async fn disconnect(&mut self, name: &str) -> McpClientResult<()> {
        self.client_manager.write().await.disconnect(name).await
    }

    // ------------------------------------------------------------------
    // Unified tool operations
    // ------------------------------------------------------------------

    /// 获取所有可用工具
    ///
    /// 【领域含义】获取合并后的工具列表，包含本地工具和所有已连接 MCP 服务器的远程工具。
    /// 本地工具在前，远程工具在后。每个条目包含来源前缀：
    /// - 本地: `"tool_name"`
    /// - 远程: `"server_name::tool_name"`
    ///
    /// 【核心职责】收集本地工具 → 收集远程工具 → 合并返回。
    pub async fn get_tools(&self) -> Result<Vec<(String, McpTool)>, McpRegistryError> {
        let mut all_tools: Vec<(String, McpTool)> = Vec::new();

        // 1. Local tools
        for def in self.local_registry.list() {
            all_tools.push((
                def.name.clone(),
                McpTool {
                    name: def.name,
                    description: def.description,
                    input_schema: def.input_schema,
                },
            ));
        }

        // 2. Remote MCP tools
        let mcp_tools = self
            .client_manager
            .read()
            .await
            .all_tools()
            .await
            .map_err(|e| McpRegistryError::Client(e.to_string()))?;

        for (server_name, tool) in mcp_tools {
            let qualified_name = format!("{}::{}", server_name, tool.name);
            all_tools.push((qualified_name, tool));
        }

        Ok(all_tools)
    }

    /// 按名称调用工具
    ///
    /// 【领域含义】根据工具名称调用工具，支持本地工具和远程 MCP 工具。
    /// 名称解析规则：
    /// - `"tool_name"`: 先在本地注册表查找，未找到则搜索远程服务器
    /// - `"server_name::tool_name"`: 直接调用指定远程服务器上的工具
    ///
    /// 【核心职责】解析名称 → 本地查找 → 远程查找 → 执行调用 → 返回结果。
    pub async fn call_tool(
        &self,
        tool_name: &str,
        args: serde_json::Value,
    ) -> Result<McpToolResult, McpRegistryError> {
        // Check if this is a remote tool (has "::" prefix convention)
        if let Some(pos) = tool_name.find("::") {
            let server_name = &tool_name[..pos];
            let remote_tool = &tool_name[pos + 2..];
            return self
                .client_manager
                .read()
                .await
                .call_tool(server_name, remote_tool, args)
                .await
                .map_err(|e| McpRegistryError::Client(e.to_string()));
        }

        // Try local registry first
        if let Some(tool) = self.local_registry.get(tool_name) {
            match tool.execute(args).await {
                Ok(result) => {
                    if let Some(err) = result.error {
                        Ok(McpToolResult::error(err))
                    } else {
                        Ok(McpToolResult::success(
                            result.output.unwrap_or_default(),
                        ))
                    }
                }
                Err(e) => Ok(McpToolResult::error(e.to_string())),
            }
        } else {
            // Try remote via client manager — search all servers
            let mcp_tools = self
                .client_manager
                .read()
                .await
                .all_tools()
                .await
                .map_err(|e| McpRegistryError::Client(e.to_string()))?;

            // Find which server has this tool
            for (server_name, mcp_tool) in &mcp_tools {
                if mcp_tool.name == tool_name {
                    return self
                        .client_manager
                        .read()
                        .await
                        .call_tool(server_name, tool_name, args)
                        .await
                        .map_err(|e| McpRegistryError::Client(e.to_string()));
                }
            }

            Err(McpRegistryError::ToolNotFound(tool_name.to_string()))
        }
    }

    /// 获取已连接的 MCP 服务器数量
    ///
    /// 【领域含义】返回当前已连接的外部 MCP 服务器数量。
    /// 【核心职责】通过 tokio runtime 异步查询客户端管理器中的服务器数量。
    pub fn connected_server_count(&self) -> usize {
        // Can't easily call async on a sync method; return 0 by default.
        // Use `rt.block_on` if a handle is available, otherwise return 0.
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => rt.block_on(async { self.client_manager.read().await.server_count() }),
            Err(_) => 0,
        }
    }

    /// 获取本地工具注册表的克隆
    ///
    /// 【领域含义】返回本地 `ToolRegistry` 的 `Arc` 克隆。
    /// 【核心职责】增加引用计数，返回共享的注册表引用。
    pub fn local_registry(&self) -> Arc<ToolRegistry> {
        Arc::clone(&self.local_registry)
    }
}

// ---------------------------------------------------------------------------
// McpRegistryError
// ---------------------------------------------------------------------------

/// MCP 注册表错误
///
/// 【领域含义】`McpRegistry` 操作过程中可能出现的错误类型。
/// 【核心职责】统一封装工具未找到、客户端错误、服务器错误、执行错误和 JSON 错误。
#[derive(Debug, thiserror::Error)]
pub enum McpRegistryError {
    /// 本地或远程未找到指定工具
    #[error("tool not found: {0}")]
    ToolNotFound(String),

    /// MCP 客户端层错误
    #[error("MCP client error: {0}")]
    Client(String),

    /// MCP 服务器层错误
    #[error("MCP server error: {0}")]
    Server(String),

    /// 本地工具执行错误
    #[error("execution error: {0}")]
    Execution(String),

    /// JSON 序列化或反序列化错误
    #[error("JSON error: {0}")]
    Json(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use code_agent_core::tools::builtin::{glob::GlobTool, read_file::ReadFileTool};

    fn make_registry() -> Arc<ToolRegistry> {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool::default())).unwrap();
        reg.register(Arc::new(GlobTool::default())).unwrap();
        Arc::new(reg)
    }

    #[tokio::test]
    async fn get_tools_returns_local_tools() {
        let local = make_registry();
        let mcp = McpRegistry::new(local);

        let tools = mcp.get_tools().await.expect("get_tools should succeed");
        assert_eq!(tools.len(), 2);
        let names: Vec<&str> = tools.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"glob"));
    }

    #[tokio::test]
    async fn call_tool_local_executes_successfully() {
        let local = make_registry();
        let mcp = McpRegistry::new(local);

        let result = mcp
            .call_tool("glob", serde_json::json!({"pattern": "*.rs"}))
            .await
            .expect("call_tool should succeed");
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn call_tool_unknown_returns_error() {
        let local = make_registry();
        let mcp = McpRegistry::new(local);

        let result = mcp.call_tool("no_such_tool", serde_json::json!({})).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            McpRegistryError::ToolNotFound(name) => assert_eq!(name, "no_such_tool"),
            other => panic!("expected ToolNotFound, got {:?}", other),
        }
    }

    #[test]
    fn create_server_registers_tools() {
        let local = make_registry();
        let mut mcp = McpRegistry::new(local);

        mcp.create_server().expect("create_server should succeed");
        assert!(mcp.server.is_some());
        assert_eq!(mcp.server.as_ref().unwrap().tool_count(), 2);
    }
}
