//! McpToolAdapter — MCP 远程工具适配器
//!
//! 【领域含义】将 `code_agent_tools::mcp::McpRegistry` 中发现的远程 MCP 工具
//! 包装为 `Tool` trait，使 Agent 可以通过 `ToolRegistry` 调用外部 MCP 服务器
//! 提供的工具。
//!
//! 【核心职责】每个 `McpToolAdapter` 对应一个远程 MCP 工具实例，
//! 在 `execute()` 时通过 `McpRegistry::call_tool()` 转发调用，
//! 并将 `McpToolResult` 转换为 `ToolResultMessage`。
//!
//! 工具在 `ToolRegistry` 中的名称为 `"mcp_{server_name}_{tool_name}"` 格式，
//! 例如 `"mcp_filesystem_read_file"`。

use std::sync::Arc;

use async_trait::async_trait;
use code_agent_core::tools::{Tool, ToolError, tool_error, tool_success};
use code_agent_protocol::{CapabilityLevel, ToolResultMessage};
use code_agent_tools::mcp::types::McpContentItem;
use code_agent_tools::mcp::McpRegistry;

/// MCP 远程工具适配器
///
/// 【领域含义】将一个远程 MCP 工具适配为本地 `Tool` trait 实现。
/// 【核心职责】存储工具元数据（名称、描述、输入 schema）和到 McpRegistry 的引用，
/// 在 `execute()` 时通过 `server_name::tool_name` 格式转发调用到远程服务器。
pub struct McpToolAdapter {
    /// 在 ToolRegistry 中注册的名称（`mcp_{server}_{tool}` 格式）
    name: String,
    /// MCP 服务器名称
    server_name: String,
    /// 远程工具名称（服务器上的原始名称）
    tool_name: String,
    /// 工具功能描述
    description: String,
    /// 工具输入参数的 JSON Schema
    input_schema: serde_json::Value,
    /// MCP 注册表引用（共享所有权）
    mcp: Arc<McpRegistry>,
}

#[async_trait]
impl Tool for McpToolAdapter {
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
        // MCP 协议使用 `server_name::tool_name` 格式路由调用
        let qualified = format!("{}::{}", self.server_name, self.tool_name);

        match self.mcp.call_tool(&qualified, params).await {
            Ok(result) => {
                // 将 McpToolResult 的内容项合并为单一文本输出
                let output = result
                    .content
                    .iter()
                    .filter_map(|item| match item {
                        McpContentItem::Text { text } => Some(text.clone()),
                        McpContentItem::Resource { text, .. } => Some(text.clone()),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                if result.is_error {
                    Ok(tool_error("", &output))
                } else {
                    Ok(tool_success("", output))
                }
            }
            Err(e) => Ok(tool_error("", &e.to_string())),
        }
    }
}

/// 将 McpRegistry 中所有已连接的远程 MCP 工具注册到 ToolRegistry。
///
/// 【领域含义】遍历 McpRegistry 发现的所有工具，跳过本地工具（已在 ToolRegistry 中），
/// 将每个远程工具包装为 `McpToolAdapter` 并注册到 `registry`。
///
/// 【核心职责】工具发现 → 适配器创建 → 按 `"mcp_{server}_{tool}"` 命名注册。
///
/// MCP 工具命名格式：`server_name::tool_name`（来自 McpRegistry::get_tools()）。
/// 适配器注册名：`mcp_server_name_tool_name`。
///
/// # Errors
///
/// 如果 `mcp.get_tools()` 失败则返回 `McpRegistryError`。
/// 单个工具注册失败（名称冲突）仅记录警告，不影响其他工具。
pub async fn register_mcp_tools(
    registry: &mut dyn code_agent_core::tools::registry::ToolRegistry,
    mcp: &Arc<McpRegistry>,
) -> Result<(), code_agent_tools::mcp::McpRegistryError> {
    let tools = mcp.get_tools().await?;

    for (qualified_name, tool) in tools {
        // 解析 "server_name::tool_name" 格式
        let (server_name, tool_name) = match qualified_name.find("::") {
            Some(pos) => (
                qualified_name[..pos].to_string(),
                qualified_name[pos + 2..].to_string(),
            ),
            None => {
                // 本地工具 — 跳过，它们已在 ToolRegistry 中直接注册
                continue;
            }
        };

        let adapter = McpToolAdapter {
            name: format!("mcp_{}_{}", server_name, tool_name),
            server_name,
            tool_name,
            description: tool.description,
            input_schema: tool.input_schema,
            mcp: Arc::clone(mcp),
        };

        if let Err(e) = registry.register(Arc::new(adapter)) {
            tracing::warn!(error = %e, "Failed to register MCP tool adapter");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use code_agent_core::tools::registry::DefaultToolRegistry;

    #[test]
    fn mcp_tool_adapter_name_format() {
        let registry = Arc::new(DefaultToolRegistry::new());
        let mcp = Arc::new(McpRegistry::new(registry));

        let adapter = McpToolAdapter {
            name: "mcp_filesystem_read_file".to_string(),
            server_name: "filesystem".to_string(),
            tool_name: "read_file".to_string(),
            description: "Read a file".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            mcp: Arc::clone(&mcp),
        };

        assert_eq!(adapter.name(), "mcp_filesystem_read_file");
        assert_eq!(adapter.description(), "Read a file");
        assert_eq!(adapter.capability(), CapabilityLevel::Exec);
    }

    #[test]
    fn mcp_tool_adapter_input_schema_is_valid_json() {
        let registry = Arc::new(DefaultToolRegistry::new());
        let mcp = Arc::new(McpRegistry::new(registry));

        let adapter = McpToolAdapter {
            name: "mcp_test_echo".to_string(),
            server_name: "test".to_string(),
            tool_name: "echo".to_string(),
            description: "Echo".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "message": {"type": "string"}
                },
                "required": ["message"]
            }),
            mcp: Arc::clone(&mcp),
        };

        let schema = adapter.input_schema();
        assert!(schema.is_object());
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn mcp_tool_adapter_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<McpToolAdapter>();
    }
}
