//! MCP（模型上下文协议）集成层
//!
//! 【领域含义】为 AI 编码代理提供统一的工具注册和调用能力，融合本地工具和远程 MCP 服务器工具。
//!
//! 核心组件：
//! - [`McpRegistry`]: 统一注册表，合并本地工具和远程 MCP 工具
//! - [`McpClientManager`]: 连接到外部 MCP 服务器
//! - [`McpServer`]: 将本地工具暴露为 MCP 服务器
//!
//! 还提供 [`register_from_env`] 函数，供各入口点（CLI、Web 服务器等）
//! 从环境变量统一注册 MCP 插件。
//!
//! 【核心职责】`McpRegistry` 将本地 `ToolRegistry` 中的工具与已连接 MCP 服务器发现的工具合并，
//! 提供单一统一的工具目录。

pub mod adapter;
pub mod client;
pub mod config;
pub mod server;
pub mod types;

use std::sync::Arc;

use code_agent_core::tools::plugin::{McpConnectionConfig, McpPlugin, PluginLoadResult, PluginManager};
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
/// let local_registry = Arc::new(DefaultToolRegistry::new());
/// let mut mcp = McpRegistry::new(local_registry);
/// mcp.connect_http("remote", "http://localhost:3000").await?;
/// let all_tools = mcp.get_tools().await?;
/// ```
pub struct McpRegistry {
    /// 本地工具注册表（来自 core crate）
    local_registry: Arc<dyn ToolRegistry>,
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
    pub fn new(local_registry: Arc<dyn ToolRegistry>) -> Self {
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
    pub fn local_registry(&self) -> Arc<dyn ToolRegistry> {
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
// Shared MCP tool registration from environment variables
// ---------------------------------------------------------------------------

/// 从环境变量注册 MCP 插件到工具注册表。
///
/// 【领域含义】供所有入口点（CLI TUI、Exec、AppServer、Web 服务器）
/// 统一调用，避免各入口点重复实现。按优先级尝试以下来源：
///
/// 1. `MCP_SERVERS` 环境变量（JSON 格式，支持多服务器）
/// 2. `MCP_SERVER_COMMAND` 环境变量（向后兼容，单服务器）
///
/// 【核心职责】读取环境变量 → parse_env_config → register_plugins。
///
/// **P0-1 安全门控**：仅当环境变量 `CODE_AGENT_ALLOW_MCP_ENV=true` 或 `CODE_AGENT_ALLOW_MCP_ENV=1`
/// 时才从环境变量加载 MCP 插件。未设置时向 stderr 输出警告并跳过。
/// 生产环境中应使用配置文件而非环境变量配置 MCP 服务器。
pub fn register_from_env(registry: &mut dyn ToolRegistry) {
    // P0-1: 拒绝未经显式 opt-in 的环境变量 MCP 注册
    let allowed = std::env::var("CODE_AGENT_ALLOW_MCP_ENV")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    if !allowed {
        tracing::warn!(
            "MCP env registration blocked: set CODE_AGENT_ALLOW_MCP_ENV=true to enable. \
             Prefer config-file-based MCP configuration for production use."
        );
        return;
    }

    tracing::info!("MCP env registration enabled via CODE_AGENT_ALLOW_MCP_ENV");
    let configs = parse_env_config();
    if !configs.is_empty() {
        register_plugins(configs, registry);
    }
}

/// 解析环境变量为 MCP 插件配置元组列表。
///
/// 按优先级读取：
/// 1. `MCP_SERVERS`（JSON 数组，多服务器）
/// 2. `MCP_SERVER_COMMAND`（空格分隔，单服务器，向后兼容）
///
/// 返回 `Vec<(name, McpConnectionConfig)>`，结果可能为空。
///
/// 【可测试性】纯函数，不依赖 tokio runtime 或网络 I/O。
fn parse_env_config() -> Vec<(String, McpConnectionConfig)> {
    // ── MCP_SERVERS JSON 格式 ───────────────────────────────────
    if let Ok(json) = std::env::var("MCP_SERVERS") {
        if !json.trim().is_empty() {
            if let Ok(configs) = serde_json::from_str::<Vec<EnvMcpEntry>>(&json) {
                let plugins: Vec<_> = configs
                    .into_iter()
                    .filter_map(|entry| {
                        let name = entry.name.unwrap_or_else(|| "mcp-server".into());
                        match (entry.command, entry.url) {
                            (Some(cmd), _) => Some((name, McpConnectionConfig::Stdio {
                                command: cmd,
                                args: entry.args.unwrap_or_default(),
                            })),
                            (None, Some(url)) => Some((name, McpConnectionConfig::Http { url })),
                            (None, None) => {
                                tracing::warn!(entry_name = %name, "MCP_SERVERS entry missing both command and url");
                                None
                            }
                        }
                    })
                    .collect();

                if !plugins.is_empty() {
                    return plugins;
                }
            }
        }
    }

    // ── MCP_SERVER_COMMAND 回退 ────────────────────────────────
    let mcp_command = match std::env::var("MCP_SERVER_COMMAND") {
        Ok(cmd) if !cmd.trim().is_empty() => cmd,
        _ => return Vec::new(),
    };

    let parts: Vec<&str> = mcp_command.split_whitespace().collect();
    if parts.is_empty() {
        return Vec::new();
    }

    vec![(
        "mcp-env".into(),
        McpConnectionConfig::Stdio {
            command: parts[0].to_string(),
            args: parts[1..].iter().map(|s| s.to_string()).collect(),
        },
    )]
}

/// `MCP_SERVERS` JSON 数组中的单个条目。
#[derive(serde::Deserialize)]
struct EnvMcpEntry {
    name: Option<String>,
    command: Option<String>,
    #[serde(default)]
    args: Option<Vec<String>>,
    url: Option<String>,
}

/// 内部：注册一组 MCP 插件到工具注册表。
fn register_plugins(
    configs: Vec<(String, McpConnectionConfig)>,
    registry: &mut dyn ToolRegistry,
) {
    let rt = match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle,
        Err(_) => {
            tracing::warn!("Cannot register MCP plugins: no tokio runtime available");
            return;
        }
    };

    rt.block_on(async {
        let mut pm = PluginManager::new();

        for (name, conn_cfg) in configs {
            let connector = adapter::McpClientConnector::new();
            let plugin = McpPlugin::new(name.clone(), name, conn_cfg, Box::new(connector));
            pm.register(Box::new(plugin));
        }

        let results = pm.load_all(registry).await;
        for result in &results {
            match result {
                PluginLoadResult::Success { name } => {
                    tracing::info!(plugin = %name, "MCP plugin registered successfully");
                }
                PluginLoadResult::Failed { name, error } => {
                    tracing::warn!(plugin = %name, error = %error, "MCP plugin failed to load");
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use code_agent_core::tools::builtin::{glob::GlobTool, read_file::ReadFileTool};
    use code_agent_core::tools::registry::DefaultToolRegistry;

    // ── helpers ────────────────────────────────────────────────────────

    /// 设置临时环境变量，在闭包结束后自动恢复。
    fn with_env_var<K, V, R>(key: K, val: Option<V>, f: impl FnOnce() -> R) -> R
    where
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let key = key.as_ref();
        let prev = std::env::var(key).ok();
        match val {
            Some(v) => std::env::set_var(key, v.as_ref()),
            None => std::env::remove_var(key),
        }
        let result = f();
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        result
    }

    fn make_registry() -> Arc<dyn ToolRegistry> {
        let mut reg = DefaultToolRegistry::new();
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

    // ── parse_env_config tests ────────────────────────────────────────

    #[test]
    fn parse_env_no_vars_returns_empty() {
        with_env_var("MCP_SERVERS", None::<&str>, || {
            with_env_var("MCP_SERVER_COMMAND", None::<&str>, || {
                let result = parse_env_config();
                assert!(result.is_empty());
            });
        });
    }

    #[test]
    fn parse_env_empty_vars_returns_empty() {
        with_env_var("MCP_SERVERS", Some(""), || {
            with_env_var("MCP_SERVER_COMMAND", Some(""), || {
                let result = parse_env_config();
                assert!(result.is_empty());
            });
        });
    }

    #[test]
    fn parse_env_server_command_single() {
        with_env_var("MCP_SERVERS", None::<&str>, || {
            with_env_var("MCP_SERVER_COMMAND", Some("npx -y @mcp/server ."), || {
                let result = parse_env_config();
                assert_eq!(result.len(), 1);
                assert_eq!(result[0].0, "mcp-env");
                match &result[0].1 {
                    McpConnectionConfig::Stdio { command, args } => {
                        assert_eq!(command, "npx");
                        assert_eq!(args, &["-y", "@mcp/server", "."]);
                    }
                    _ => panic!("expected Stdio variant"),
                }
            });
        });
    }

    #[test]
    fn parse_env_server_command_whitespace_only() {
        with_env_var("MCP_SERVERS", None::<&str>, || {
            with_env_var("MCP_SERVER_COMMAND", Some("   "), || {
                let result = parse_env_config();
                assert!(result.is_empty());
            });
        });
    }

    #[test]
    fn parse_env_mcp_servers_json_multi() {
        with_env_var("MCP_SERVER_COMMAND", None::<&str>, || {
            with_env_var(
                "MCP_SERVERS",
                Some(r#"[
                    {"name":"fs","command":"npx","args":["-y","server-fs","."]},
                    {"name":"github","url":"http://localhost:3000/mcp"}
                ]"#),
                || {
                    let result = parse_env_config();
                    assert_eq!(result.len(), 2);

                    // fs: stdio
                    assert_eq!(result[0].0, "fs");
                    match &result[0].1 {
                        McpConnectionConfig::Stdio { command, args } => {
                            assert_eq!(command, "npx");
                            assert!(args.contains(&"-y".to_string()));
                        }
                        _ => panic!("expected Stdio"),
                    }

                    // github: http
                    assert_eq!(result[1].0, "github");
                    match &result[1].1 {
                        McpConnectionConfig::Http { url } => {
                            assert_eq!(url, "http://localhost:3000/mcp");
                        }
                        _ => panic!("expected Http"),
                    }
                },
            );
        });
    }

    #[test]
    fn parse_env_mcp_servers_json_default_name() {
        with_env_var("MCP_SERVER_COMMAND", None::<&str>, || {
            with_env_var(
                "MCP_SERVERS",
                Some(r#"[{"command":"echo","args":["hello"]}]"#),
                || {
                    let result = parse_env_config();
                    assert_eq!(result.len(), 1);
                    // name 缺失时使用默认值
                    assert_eq!(result[0].0, "mcp-server");
                },
            );
        });
    }

    #[test]
    fn parse_env_mcp_servers_skips_missing_command_url() {
        with_env_var("MCP_SERVER_COMMAND", None::<&str>, || {
            with_env_var(
                "MCP_SERVERS",
                Some(r#"[
                    {"name":"valid","command":"echo","args":["ok"]},
                    {"name":"bad","args":[]}
                ]"#),
                || {
                    let result = parse_env_config();
                    // valid 应通过, bad 应被跳过
                    assert_eq!(result.len(), 1);
                    assert_eq!(result[0].0, "valid");
                },
            );
        });
    }

    #[test]
    fn parse_env_mcp_servers_invalid_json_falls_back() {
        with_env_var("MCP_SERVER_COMMAND", Some("echo hello"), || {
            with_env_var("MCP_SERVERS", Some("not json at all"), || {
                let result = parse_env_config();
                // MCP_SERVERS 解析失败 → 回退到 MCP_SERVER_COMMAND
                assert_eq!(result.len(), 1);
                assert_eq!(result[0].0, "mcp-env");
            });
        });
    }
}
