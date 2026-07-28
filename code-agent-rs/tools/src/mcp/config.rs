//! MCP 服务器配置类型
//!
//! 【领域含义】描述一个外部 MCP 服务器的连接方式，支持 stdio（子进程）
//! 和 HTTP 两种模式。可通过环境变量 `MCP_SERVERS`（JSON 格式）或
//! TOML 配置文件的 `[mcp_servers.{name}]` 节配置。
//!
//! 【核心职责】提供 MCP 插件的连接参数：服务器名称、命令或 URL。
//!
//! # 环境变量示例（JSON）
//!
//! ```json
//! [
//!   {"name": "filesystem", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."]},
//!   {"name": "github", "url": "http://localhost:3000/mcp"}
//! ]
//! ```
//!
//! # TOML 配置示例
//!
//! ```toml
//! [mcp_servers.filesystem]
//! command = "npx"
//! args = ["-y", "@modelcontextprotocol/server-filesystem", "."]
//!
//! [mcp_servers.github]
//! url = "http://localhost:3000/mcp"
//! ```

use serde::{Deserialize, Serialize};

/// MCP 服务器配置。
///
/// 描述一个外部 MCP 服务器的连接方式，支持 stdio 和 HTTP 两种模式。
///
/// - `command` + `args`: stdio 模式，启动子进程并通过 stdin/stdout 通信。
/// - `url`: HTTP 模式，通过 HTTP POST 请求通信。
///
/// 两种模式至少设置一种。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// 服务器名称（用于日志和标识，env var JSON 格式必需）
    #[serde(default)]
    pub name: Option<String>,
    /// stdio 模式：可执行命令（与 url 二选一）
    pub command: Option<String>,
    /// stdio 模式：命令参数
    #[serde(default)]
    pub args: Vec<String>,
    /// HTTP 模式：服务器 URL（与 command 二选一）
    pub url: Option<String>,
}

impl McpServerConfig {
    /// 验证配置合法（command 或 url 至少设置一个）。
    pub fn is_valid(&self) -> bool {
        self.command.is_some() || self.url.is_some()
    }

    /// 返回可读的服务器标识。
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or("mcp")
    }
}
