//! 插件系统（Phase F）—— 动态加载工具集到 ToolRegistry
//!
//! 【领域含义】提供 `Plugin` trait 和 `PluginManager`，允许从外部
//! 来源（MCP 服务器、配置文件中的工具列表等）动态加载工具集。
//!
//! 【核心职责】
//! - `Plugin` trait: 插件必须实现的接口，返回一组工具
//! - `PluginManager`: 管理插件的注册、加载和生命周期
//!
//! # 使用示例
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use code_agent_core::tools::registry::ToolRegistry;
//! use code_agent_core::tools::plugin::{Plugin, PluginManager};
//!
//! struct MyPlugin;
//! impl Plugin for MyPlugin {
//!     fn name(&self) -> &str { "my-plugin" }
//!     async fn load(&self, registry: &mut ToolRegistry) -> Result<(), Box<dyn std::error::Error>> {
//!         // register tools into registry
//!         Ok(())
//!     }
//! }
//!
//! let mut registry = DefaultToolRegistry::new();
//! let mut manager = PluginManager::new(Arc::new(tokio::sync::Mutex::new(registry)));
//! manager.register(Box::new(MyPlugin)).await?;
//! manager.load_all().await?;
//! ```

use std::sync::Arc;

use async_trait::async_trait;

use super::command::Command;
use super::registry::ToolRegistry;
use super::Tool;

// ---------------------------------------------------------------------------
// Plugin trait
// ---------------------------------------------------------------------------

/// 插件接口 —— 每个插件可以将一组工具注册到 ToolRegistry。
///
/// 【领域含义】Plugin 是工具生态系统的扩展点。任何实现了 Plugin 的模块
/// 都可以在运行时向 ToolRegistry 注册工具，实现动态工具发现和加载。
///
/// 【核心职责】`load` 方法接收一个可变的 ToolRegistry 引用，插件在此
/// 注册其提供的工具。
///
/// 插件还可以通过 `commands()` 提供自定义斜杠命令（Phase F）。
#[async_trait]
pub trait Plugin: Send + Sync {
    /// 返回插件的唯一名称。
    fn name(&self) -> &str;

    /// 加载插件 —— 将工具注册到 ToolRegistry。
    ///
    /// 【领域含义】插件在此方法中将其实现在 ToolRegistry 中注册。
    /// 如果插件依赖外部资源（如 MCP 服务器），在此方法中建立连接。
    async fn load(&self, registry: &mut dyn ToolRegistry) -> Result<(), Box<dyn std::error::Error>>;

    /// 返回插件提供的自定义命令（Phase F）。
    ///
    /// 【领域含义】插件可以通过此方法暴露自定义斜杠命令，
    /// 这些命令将被注册到 CommandRegistry 中。
    ///
    /// 默认返回空列表。
    fn commands(&self) -> Vec<Command> {
        Vec::new()
    }

    /// 卸载插件 —— 清理资源。
    ///
    /// 【领域含义】可选的生命周期方法，在插件被移除时调用。
    /// 默认实现为空操作。
    async fn unload(&self, _registry: &mut dyn ToolRegistry) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// McpConnector — MCP 连接器抽象
// ---------------------------------------------------------------------------

/// MCP 连接器 —— 将 MCP 连接配置转化为 Tool 实例列表。
///
/// 【领域含义】这是 `core` crate 中的抽象接口，用于在插件系统中
/// 注入实际的 MCP 连接能力。具体实现在 `code-agent-tools` crate 中
/// （例如 `McpClientConnector`）。
///
/// 【核心职责】根据 `McpConnectionConfig` 配置连接到 MCP 服务器，
/// 发现远程工具，返回实现了 `Tool` trait 的工具实例列表。
#[async_trait]
pub trait McpConnector: Send + Sync {
    /// 连接到 MCP 服务器，返回发现的工具列表。
    ///
    /// 【领域含义】接收连接配置，建立 MCP 连接，执行工具发现握手，
    /// 将远程工具包装为本地 `Tool` trait 对象。
    ///
    /// 【核心职责】连接 → 发现 → 包装 → 返回。
    async fn connect(
        &self,
        config: &McpConnectionConfig,
    ) -> Result<Vec<Arc<dyn Tool>>, Box<dyn std::error::Error>>;
}

// ---------------------------------------------------------------------------
// MCP Connection Config
// ---------------------------------------------------------------------------

/// MCP 连接配置。
#[derive(Clone, Debug)]
pub enum McpConnectionConfig {
    /// 通过 stdio 连接（子进程）
    Stdio {
        command: String,
        args: Vec<String>,
    },
    /// 通过 HTTP 连接
    Http {
        url: String,
    },
}

// ---------------------------------------------------------------------------
// MCP Plugin — 将 MCP 服务器连接包装为 Plugin
// ---------------------------------------------------------------------------

/// MCP 插件 —— 将外部 MCP 服务器连接包装为 Plugin。
///
/// 【领域含义】通过 MCP 协议连接到远程工具服务器，将发现的工具
/// 动态注册到本地 ToolRegistry。
///
/// 【核心职责】建立 MCP 连接 → 发现远程工具 → 本地注册。
///
/// # 使用示例
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use code_agent_core::tools::plugin::{McpPlugin, McpConnectionConfig, McpConnector};
/// use code_agent_core::tools::registry::ToolRegistry;
///
/// // connector 由 `code-agent-tools` 提供
/// let plugin = McpPlugin::new(
///     "my-server",
///     "my-server",
///     McpConnectionConfig::Http { url: "http://localhost:3000".into() },
///     Box::new(MyConnector),
/// );
/// ```
pub struct McpPlugin {
    name: String,
    server_name: String,
    connection_config: McpConnectionConfig,
    connector: Box<dyn McpConnector>,
}

impl McpPlugin {
    /// 创建 MCP 插件。
    ///
    /// 【参数】
    /// - `name`: 插件名称
    /// - `server_name`: MCP 服务器名称
    /// - `config`: 连接配置（stdio 或 HTTP）
    /// - `connector`: 实现了 `McpConnector` 的连接器实例
    pub fn new(
        name: impl Into<String>,
        server_name: impl Into<String>,
        config: McpConnectionConfig,
        connector: Box<dyn McpConnector>,
    ) -> Self {
        Self {
            name: name.into(),
            server_name: server_name.into(),
            connection_config: config,
            connector,
        }
    }
}

#[async_trait]
impl Plugin for McpPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    async fn load(&self, registry: &mut dyn ToolRegistry) -> Result<(), Box<dyn std::error::Error>> {
        let tools = self.connector.connect(&self.connection_config).await?;
        let count = tools.len();
        for tool in tools {
            registry.register(tool)?;
        }
        tracing::info!(
            plugin = %self.name,
            server = %self.server_name,
            tool_count = count,
            "McpPlugin: connected to MCP server and registered tools"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PluginManager
// ---------------------------------------------------------------------------

/// 插件管理器 —— 管理插件的注册、加载和生命周期。
///
/// 【领域含义】PluginManager 持有 ToolRegistry 和已注册的插件列表。
/// 调用 `load_all` 加载所有插件，将它们的工具注册到 ToolRegistry。
///
/// 【核心职责】插件的注册、加载、卸载和生命周期管理。
pub struct PluginManager {
    /// 已注册的插件列表
    plugins: Vec<Box<dyn Plugin>>,
}

impl PluginManager {
    /// 创建空的插件管理器。
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }

    /// 注册一个插件。
    ///
    /// 【领域含义】将插件加入管理列表，但尚未加载（不注册工具）。
    /// 【核心职责】保存插件引用，等待 `load_all` 或 `load_plugin` 时加载。
    pub fn register(&mut self, plugin: Box<dyn Plugin>) {
        self.plugins.push(plugin);
    }

    /// 加载所有已注册的插件。
    ///
    /// 【领域含义】遍历所有已注册插件，逐个调用 `load` 方法注册工具。
    /// 如果某个插件加载失败，记录错误并继续加载后续插件（容错）。
    ///
    /// 【核心职责】批量加载插件到 ToolRegistry。
    pub async fn load_all(&self, registry: &mut dyn ToolRegistry) -> Vec<PluginLoadResult> {
        let mut results = Vec::new();
        for plugin in &self.plugins {
            let name = plugin.name().to_string();
            match plugin.load(registry).await {
                Ok(()) => {
                    tracing::info!(plugin = %name, "Plugin loaded successfully");
                    results.push(PluginLoadResult::Success { name });
                }
                Err(e) => {
                    tracing::error!(plugin = %name, error = %e, "Plugin failed to load");
                    results.push(PluginLoadResult::Failed {
                        name,
                        error: e.to_string(),
                    });
                }
            }
        }
        results
    }

    /// 获取已注册的插件数量。
    pub fn plugin_count(&self) -> usize {
        self.plugins.len()
    }

    /// 获取所有已注册插件的名称列表。
    pub fn plugin_names(&self) -> Vec<String> {
        self.plugins.iter().map(|p| p.name().to_string()).collect()
    }

    /// 收集所有插件提供的自定义命令，注册到指定的 CommandRegistry。
    ///
    /// 【领域含义】Phase F 集成点：遍历已注册插件，调用其 `commands()` 方法，
    /// 将返回的命令依次注册到 `registry` 中。跳过被占用的命令名并记录警告。
    ///
    /// 【核心职责】插件命令自动发现与注册。
    pub fn collect_commands(&self, registry: &mut crate::tools::command::CommandRegistry) {
        for plugin in &self.plugins {
            for cmd in plugin.commands() {
                let name = cmd.name.clone();
                if let Err(e) = registry.register(cmd) {
                    tracing::warn!(
                        plugin = %plugin.name(),
                        command = %name,
                        error = %e,
                        "Failed to register plugin command (name collision)"
                    );
                } else {
                    tracing::info!(
                        plugin = %plugin.name(),
                        command = %name,
                        "Plugin command registered"
                    );
                }
            }
        }
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// PluginLoadResult
// ---------------------------------------------------------------------------

/// 插件加载结果。
#[derive(Debug, Clone)]
pub enum PluginLoadResult {
    /// 加载成功
    Success {
        /// 插件名称
        name: String,
    },
    /// 加载失败
    Failed {
        /// 插件名称
        name: String,
        /// 错误信息
        error: String,
    },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::DefaultToolRegistry;
    use std::sync::Arc;
    use crate::tools::builtin::{glob::GlobTool, read_file::ReadFileTool};

    struct TestPlugin;

    #[async_trait]
    impl Plugin for TestPlugin {
        fn name(&self) -> &str {
            "test-plugin"
        }

        async fn load(&self, registry: &mut dyn ToolRegistry) -> Result<(), Box<dyn std::error::Error>> {
            registry.register(Arc::new(ReadFileTool::default()))?;
            registry.register(Arc::new(GlobTool::default()))?;
            Ok(())
        }
    }

    struct FailingPlugin;

    #[async_trait]
    impl Plugin for FailingPlugin {
        fn name(&self) -> &str {
            "failing-plugin"
        }

        async fn load(&self, _registry: &mut dyn ToolRegistry) -> Result<(), Box<dyn std::error::Error>> {
            Err("plugin failed intentionally".into())
        }
    }

    #[tokio::test]
    async fn test_plugin_registers_tools() {
        let mut registry = DefaultToolRegistry::new();
        let mut manager = PluginManager::new();

        manager.register(Box::new(TestPlugin));
        assert_eq!(manager.plugin_count(), 1);
        assert_eq!(manager.plugin_names(), vec!["test-plugin"]);

        let results = manager.load_all(&mut registry).await;
        assert_eq!(results.len(), 1);
        assert!(matches!(&results[0], PluginLoadResult::Success { name } if name == "test-plugin"));

        // Tools should be registered
        assert!(registry.get("read_file").is_some());
        assert!(registry.get("glob").is_some());
    }

    #[tokio::test]
    async fn test_failing_plugin_does_not_block_others() {
        let mut registry = DefaultToolRegistry::new();
        let mut manager = PluginManager::new();

        manager.register(Box::new(FailingPlugin));
        manager.register(Box::new(TestPlugin));

        let results = manager.load_all(&mut registry).await;
        assert_eq!(results.len(), 2);

        // First should fail
        assert!(matches!(&results[0], PluginLoadResult::Failed { name, .. } if name == "failing-plugin"));
        // Second should succeed despite first failing
        assert!(matches!(&results[1], PluginLoadResult::Success { name } if name == "test-plugin"));

        // TestPlugin's tools should be registered despite FailingPlugin
        assert!(registry.get("read_file").is_some());
        assert!(registry.get("glob").is_some());
    }

    #[tokio::test]
    async fn test_empty_plugin_manager() {
        let mut registry = DefaultToolRegistry::new();
        let manager = PluginManager::new();

        assert_eq!(manager.plugin_count(), 0);
        assert!(manager.plugin_names().is_empty());

        let results = manager.load_all(&mut registry).await;
        assert!(results.is_empty());
    }
}
