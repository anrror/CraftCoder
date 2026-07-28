//! MCP 连接器 + 远程工具适配器
//!
//! 【领域含义】`core::tools::plugin::McpPlugin` 需要一个实现了
//! `McpConnector` trait 的实例来连接 MCP 服务器。本模块提供
//! 基于 `McpClientManager` 的默认实现。
//!
//! 【核心职责】
//! - `McpClientConnector`: 实现 `McpConnector`，使用 McpClientManager 连接服务器
//! - `McpRemoteTool`: 远程 MCP 工具的 `Tool` trait 包装，调用时委托给 McpClientManager
//!
//! # 使用示例
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use code_agent_tools::mcp::adapter::McpClientConnector;
//! use code_agent_core::tools::plugin::{McpPlugin, McpConnectionConfig, McpConnector};
//!
//! let connector = McpClientConnector;
//! let plugin = McpPlugin::new(
//!     "filesystem",
//!     "filesystem",
//!     McpConnectionConfig::Stdio {
//!         command: "npx".into(),
//!         args: vec!["-y", "@modelcontextprotocol/server-filesystem", "."].into_iter().map(String::from).collect(),
//!     },
//!     Box::new(connector),
//! );
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use code_agent_core::tools::plugin::{McpConnectionConfig, McpConnector};
use code_agent_core::tools::{Tool, ToolError, tool_error, tool_success};
use code_agent_protocol::{CapabilityLevel, ToolResultMessage};
use tokio::sync::RwLock;

use super::client::McpClientManager;
use super::types::McpContentItem;

// ---------------------------------------------------------------------------
// McpClientConnector
// ---------------------------------------------------------------------------

/// 基于 `McpClientManager` 的 `McpConnector` 实现。
///
/// 【领域含义】根据 `McpConnectionConfig`（stdio 或 HTTP）连接到外部 MCP 服务器，
/// 发现远程工具，将每个远程工具包装为 `McpRemoteTool` 返回。
///
/// 【核心职责】连接建立 → 工具发现 → 适配器包装。
///
/// # 错误处理
///
/// 如果连接失败（命令不存在、URL 不可达、协议版本不匹配），
/// 返回 `McpClientError` 并转换为 `Box<dyn Error>`。
pub struct McpClientConnector;

impl McpClientConnector {
    /// 创建连接器实例。
    pub fn new() -> Self {
        Self
    }
}

impl Default for McpClientConnector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl McpConnector for McpClientConnector {
    async fn connect(
        &self,
        config: &McpConnectionConfig,
    ) -> Result<Vec<Arc<dyn Tool>>, Box<dyn std::error::Error>> {
        let mut manager = McpClientManager::new();

        match config {
            McpConnectionConfig::Stdio { command, args } => {
                let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                manager.connect_stdio("mcp", command, &args_refs).await?;
            }
            McpConnectionConfig::Http { url } => {
                manager.connect_http("mcp", url).await?;
            }
        }

        // 发现远程工具
        let remote_tools = manager.all_tools().await?;

        // 连接完成，包装进 Arc（后续所有操作都只需要 &self）
        let manager = Arc::new(RwLock::new(manager));

        let mut result: Vec<Arc<dyn Tool>> = Vec::with_capacity(remote_tools.len());
        for (server_name, tool_def) in remote_tools {
            result.push(Arc::new(McpRemoteTool::new(
                server_name,
                tool_def.name,
                tool_def.description,
                tool_def.input_schema,
                Arc::clone(&manager),
            )) as Arc<dyn Tool>);
        }

        tracing::info!(
            target: "mcp_connector",
            tool_count = result.len(),
            "McpClientConnector: connected to MCP server and discovered tools"
        );

        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// McpRemoteTool
// ---------------------------------------------------------------------------

/// 远程 MCP 工具的 `Tool` trait 包装。
///
/// 【领域含义】将外部 MCP 服务器上注册的工具包装为本地 `Tool` trait 对象。
/// 每次 `execute()` 调用时，通过共享的 `McpClientManager` 委托到远程服务器。
///
/// 【核心职责】存储工具元数据和到客户端管理器的引用，转发工具调用到 MCP 服务器。
pub struct McpRemoteTool {
    /// 在 ToolRegistry 中的注册名（`mcp_{server}_{tool}`）
    name: String,
    /// MCP 服务器名称
    server_name: String,
    /// 远程工具名称
    tool_name: String,
    /// 工具功能描述
    description: String,
    /// 输入参数 JSON Schema
    input_schema: serde_json::Value,
    /// 共享的 MCP 客户端管理器（工具调用时锁定读取）
    client_manager: Arc<RwLock<McpClientManager>>,
}

impl McpRemoteTool {
    /// 创建远程工具适配器。
    pub fn new(
        server_name: String,
        tool_name: String,
        description: String,
        input_schema: serde_json::Value,
        client_manager: Arc<RwLock<McpClientManager>>,
    ) -> Self {
        Self {
            name: format!("mcp_{}_{}", server_name, tool_name),
            server_name,
            tool_name,
            description,
            input_schema,
            client_manager,
        }
    }

    /// 获取 MCP 服务器名称。
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// 获取远程工具名称（服务器上的原始名称）。
    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }
}

#[async_trait]
impl Tool for McpRemoteTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> serde_json::Value {
        self.input_schema.clone()
    }

    fn capability(&self) -> CapabilityLevel {
        // 远程 MCP 工具可能执行任意操作，使用最高风险级别
        CapabilityLevel::Exec
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let result = self
            .client_manager
            .read()
            .await
            .call_tool(&self.server_name, &self.tool_name, params)
            .await;

        match result {
            Ok(mcp_result) => {
                let output = mcp_result
                    .content
                    .iter()
                    .map(|item| match item {
                        McpContentItem::Text { text } => text.clone(),
                        McpContentItem::Resource { text, .. } => text.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                if mcp_result.is_error {
                    Ok(tool_error("", &output))
                } else {
                    Ok(tool_success("", output))
                }
            }
            Err(e) => Ok(tool_error("", e.to_string())),
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
    fn mcp_remote_tool_name_format() {
        let mgr = Arc::new(RwLock::new(McpClientManager::new()));
        let tool = McpRemoteTool::new(
            "filesystem".to_string(),
            "read_file".to_string(),
            "Read a file".to_string(),
            serde_json::json!({"type": "object"}),
            mgr,
        );

        assert_eq!(tool.name(), "mcp_filesystem_read_file");
        assert_eq!(tool.description(), "Read a file");
        assert_eq!(tool.server_name(), "filesystem");
        assert_eq!(tool.tool_name(), "read_file");
    }

    #[test]
    fn mcp_remote_tool_capability_is_exec() {
        let mgr = Arc::new(RwLock::new(McpClientManager::new()));
        let tool = McpRemoteTool::new(
            "test".to_string(),
            "echo".to_string(),
            "Echo".to_string(),
            serde_json::json!({"type": "object"}),
            mgr,
        );

        assert_eq!(tool.capability(), CapabilityLevel::Exec);
    }

    #[test]
    fn mcp_remote_tool_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<McpRemoteTool>();
    }

    #[test]
    fn mcp_client_connector_default() {
        let c = McpClientConnector::new();
        // Verify the connector can be created and is Send + Sync
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<McpClientConnector>();
        let _ = c;
    }
}
