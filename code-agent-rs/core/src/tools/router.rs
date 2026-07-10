//! Tool call router.
//!
//! `ToolRouter` is the entry point for all tool invocations. It:
//! 1. Looks up the tool in the registry
//! 2. Checks permissions via `PermissionEnforcer`
//! 3. Executes the tool and returns the result
//!
//! All errors are returned as `ToolResultMessage` with the `error` field
//! populated, so the model always gets a structured response.
//!
//! # 领域描述
//!
//! 工具路由器是 AI Agent 工具调用管道的核心编排器。
//! 它串联了三个关键步骤：注册表查找 -> 权限检查 -> 工具执行，
//! 确保每次工具调用都经过完整的生命周期管理。
//! 所有错误都以结构化 ToolResultMessage 形式返回，保证模型始终收到可解析的响应。

use std::sync::Arc;

use code_agent_protocol::ToolCall;

use super::permission::PermissionEnforcer;
use super::registry::ToolRegistry;
use super::{tool_error, ToolDefinition, ToolError, ToolResultMessage};

/// 工具调用路由器 —— 编排工具调用的完整生命周期
///
/// 【领域含义】ToolRouter 是 AI Agent 工具调用管道的编排核心。
/// 它封装了注册表查找、权限守卫检查、工具执行三个阶段的串联逻辑，
/// 是 Agent 与外部工具交互的唯一入口。所有结果统一以 ToolResultMessage 返回。
///
/// Routes tool calls through registry lookup -> permission check -> execution.
pub struct ToolRouter {
    registry: Arc<ToolRegistry>,
    permission: Arc<PermissionEnforcer>,
}

impl ToolRouter {
    /// 创建工具路由器实例
    ///
    /// 【领域含义】使用已初始化的 ToolRegistry 和 PermissionEnforcer 构建路由器。
    /// 两者均通过 Arc 包装以实现跨线程共享。
    ///
    /// Create a new router backed by the given registry and permission enforcer.
    ///
    /// Both are wrapped in `Arc` so they can be shared across threads.
    pub fn new(registry: Arc<ToolRegistry>, permission: Arc<PermissionEnforcer>) -> Self {
        Self {
            registry,
            permission,
        }
    }

    /// 路由工具调用 —— 执行查找 -> 权限检查 -> 执行完整管道
    ///
    /// 【领域含义】核心路由方法。按序执行三个阶段：
    /// 1. 注册表查找：按调用名称从 registry 中匹配 Tool
    /// 2. 权限检查：通过 permission 验证调用方是否有足够能力等级
    /// 3. 工具执行：调用 Tool::execute 执行具体操作
    /// 任一阶段失败即刻返回包含错误描述的 ToolResultMessage，不继续执行后续阶段。
    ///
    /// # Pipeline
    ///
    /// 1. **Lookup**: find the tool in the registry by name
    /// 2. **Permission**: verify the caller has sufficient capability
    /// 3. **Execute**: invoke `tool.execute(params)`
    ///
    /// All failures (not found, permission denied, execution error) are
    /// returned as `ToolResultMessage` with `error` set to a descriptive
    /// message. This ensures the model always gets a structured response.
    pub async fn route(&self, call: &ToolCall) -> ToolResultMessage {
        // 1. Lookup
        let tool = match self.registry.get(&call.name) {
            Some(t) => t,
            None => {
                return tool_error(&call.id, ToolError::not_found(&call.name).to_string());
            }
        };

        // 2. Permission check
        if let Err(e) = self.permission.check(tool.name(), tool.capability()) {
            return tool_error(&call.id, e.to_string());
        }

        // 3. Execute
        let mut result = match tool.execute(call.arguments.clone()).await {
            Ok(result) => result,
            Err(e) => tool_error(&call.id, e.to_string()),
        };
        // Ensure the tool_call_id matches the incoming call id
        result.tool_call_id = call.id.clone();
        result
    }

    /// 获取所有已注册工具的元数据定义列表
    ///
    /// 【领域含义】用于向模型上下文构建工具列表。
    /// 返回的 ToolDefinition 描述了每个工具的名称、参数 schema 和功能描述，
    /// 模型据此生成工具调用请求。
    ///
    /// Return `ToolDefinition` metadata for all registered tools.
    ///
    /// This is used to build the tool list sent to the model context.
    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.registry.list()
    }

    /// 获取底层注册表的克隆引用（用于测试和内部检查）
    ///
    /// 【领域含义】暴露注册表 Arc 快照，方便测试代码直接操作注册表
    /// 或进行工具列表断言。
    ///
    /// Returns a clone of the underlying registry (for testing / inspection).
    pub fn registry(&self) -> Arc<ToolRegistry> {
        Arc::clone(&self.registry)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::{glob::GlobTool, read_file::ReadFileTool, write_file::WriteFileTool};
    use code_agent_protocol::PermissionMode;

    #[tokio::test]
    async fn route_valid_tool_call_success() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(GlobTool::default())).unwrap();
        let registry = Arc::new(reg);
        let permission = Arc::new(PermissionEnforcer::new(PermissionMode::Auto));
        let router = ToolRouter::new(Arc::clone(&registry), permission);

        let call = ToolCall {
            id: "call-1".into(),
            name: "glob".into(),
            arguments: serde_json::json!({"pattern": "*.rs"}),
        };
        let result = router.route(&call).await;
        assert!(result.is_success());
        assert_eq!(result.tool_call_id, "call-1");
    }

    #[tokio::test]
    async fn route_unknown_tool_name_error() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool::default())).unwrap();
        let registry = Arc::new(reg);
        let permission = Arc::new(PermissionEnforcer::new(PermissionMode::Auto));
        let router = ToolRouter::new(registry, permission);

        let call = ToolCall {
            id: "call-2".into(),
            name: "nonexistent_tool".into(),
            arguments: serde_json::json!({}),
        };
        let result = router.route(&call).await;
        assert!(result.is_error());
        assert!(result.error.unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn route_permission_denied() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(WriteFileTool::default())).unwrap();
        let registry = Arc::new(reg);
        let permission = Arc::new(PermissionEnforcer::new(PermissionMode::Block));
        let router = ToolRouter::new(registry, permission);

        let call = ToolCall {
            id: "call-3".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path": "/tmp/test.txt", "content": "hello"}),
        };
        let result = router.route(&call).await;
        assert!(result.is_error());
        let err = result.error.unwrap();
        assert!(err.contains("permission denied") || err.contains("Block"));
    }

    #[tokio::test]
    async fn tool_definitions_returns_all_registered() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool::default())).unwrap();
        reg.register(Arc::new(GlobTool::default())).unwrap();
        let registry = Arc::new(reg);
        let permission = Arc::new(PermissionEnforcer::new(PermissionMode::Auto));
        let router = ToolRouter::new(registry, permission);

        let defs = router.tool_definitions();
        assert_eq!(defs.len(), 2);
    }

    #[tokio::test]
    async fn concurrent_tool_calls_succeed() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool::default())).unwrap();
        reg.register(Arc::new(GlobTool::default())).unwrap();
        let registry = Arc::new(reg);
        let permission = Arc::new(PermissionEnforcer::new(PermissionMode::Auto));
        let router = Arc::new(ToolRouter::new(registry, permission));

        let r1 = Arc::clone(&router);
        let r2 = Arc::clone(&router);

        let call1 = ToolCall {
            id: "c1".into(),
            name: "glob".into(),
            arguments: serde_json::json!({"pattern": "*.rs"}),
        };
        let call2 = ToolCall {
            id: "c2".into(),
            name: "glob".into(),
            arguments: serde_json::json!({"pattern": "*.toml"}),
        };

        let (res1, res2) = tokio::join!(r1.route(&call1), r2.route(&call2));
        assert!(res1.is_success());
        assert!(res2.is_success());
    }
}
