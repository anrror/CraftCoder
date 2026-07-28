//! Code Agent 工具集 — MCP 集成、LSP 语言服务、Git 版本控制、Shell 沙箱执行。
//!
//! 【领域含义】本 crate 是 AI 编码代理的底层工具层，封装了与外部系统交互的四个核心领域：
//! - **mcp**: MCP（模型上下文协议）客户端与服务器实现，用于工具注册与远程调用
//! - **lsp**: 语言服务器协议集成，提供代码智能（跳转定义、引用查找、悬停提示、诊断）
//! - **git**: Git 源码控制操作，带安全策略的增删改查
//! - **shell**: 沙箱化 Shell 命令执行（bubblewrap / Docker / 无限制回退）
//!
//! 【核心职责】为上层 Agent 提供统一的、安全的、可审计的工具调用接口。

#[cfg(feature = "lsp")]
pub mod lsp;

#[cfg(feature = "mcp")]
pub mod mcp;

pub mod git;
pub mod shell;

use std::sync::Arc;

use code_agent_core::tools::builtin::{
    edit_file::EditFileTool, glob::GlobTool, grep::GrepTool, list_dir::ListDirTool,
    read_file::ReadFileTool, write_file::WriteFileTool,
};
use code_agent_core::tools::Tool;
use code_agent_core::tools::registry::ToolRegistry;

// ---------------------------------------------------------------------------
// P0-3: 子进程环境变量过滤 — 防止 LLM_API_KEY 等凭据泄露
// ---------------------------------------------------------------------------

/// 敏感环境变量名称列表 — 在派生子进程前从环境中清除。
///
/// 防止 MCP / LSP / shell 等外部进程继承凭据和内部配置信息。
const SENSITIVE_ENV_VARS: &[&str] = &[
    "LLM_API_KEY",
    "LLM_API_BASE",
    "LLM_API_BASE_URL",
    "LLM_MODEL",
    "LLM_CHAT_MODEL",
    "LLM_TIMEOUT",
    "LLM_MAX_TOKENS",
    "WEB_API_KEY",
    "CODE_AGENT_ALLOW_MCP_ENV",
    "CODE_AGENT_ALLOW_MCP_HTTP",
    "RUST_LOG",
    "RUST_BACKTRACE",
];

/// 从 `tokio::process::Command` 中清除敏感环境变量。
///
/// 应在调用 `.spawn()` 之前调用此函数。
/// 保留 `PATH`、`HOME`、`TEMP` 等系统变量，
/// 仅移除 `SENSITIVE_ENV_VARS` 列表中的键。
pub fn filter_sensitive_env(cmd: &mut tokio::process::Command) {
    for var in SENSITIVE_ENV_VARS {
        cmd.env_remove(var);
    }
}

// ---------------------------------------------------------------------------
// Canonical tool registration
// ---------------------------------------------------------------------------

/// Register the six built-in file-operation tools into the given registry.
///
/// This is the **single canonical** function for populating a `ToolRegistry`
/// with the core file tools. Callers can then add platform-specific or
/// feature-gated tools (Shell, Git, LSP, MCP) on top.
///
/// # Tools registered
///
/// | Tool          | Description |
/// |--------------|-------------|
/// | `read_file`  | Read a file from the local filesystem |
/// | `write_file` | Write a file to the local filesystem |
/// | `edit_file`  | Apply a string replacement to a file |
/// | `list_dir`   | List files and directories in a given path |
/// | `grep`       | Search file contents using a regex pattern |
/// | `glob`       | Find files matching a glob pattern |
///
/// # Returns
///
/// Number of tools that **failed** to register. Failures are logged via
/// `tracing::warn` and do not prevent remaining tools from being registered.
#[allow(clippy::module_name_repetitions)]
pub fn register_all_core_tools(registry: &mut dyn ToolRegistry) -> usize {
    let mut failed = 0usize;

    let file_tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ReadFileTool::default()),
        Arc::new(WriteFileTool),
        Arc::new(EditFileTool),
        Arc::new(ListDirTool),
        Arc::new(GrepTool),
        Arc::new(GlobTool),
    ];

    for tool in file_tools {
        if let Err(e) = registry.register(tool) {
            tracing::warn!(error = %e, "Failed to register built-in file tool");
            failed += 1;
        }
    }

    failed
}

/// 遗留占位函数
///
/// 【领域含义】早期版本的标识函数，用于验证 crate 是否正确加载。
/// 【核心职责】返回固定字符串 `"hello from code-agent-tools"` 作为健康检查标记。
pub fn hello() -> &'static str {
    "hello from code-agent-tools"
}
