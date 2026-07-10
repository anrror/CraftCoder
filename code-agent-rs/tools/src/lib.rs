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

/// 遗留占位函数
///
/// 【领域含义】早期版本的标识函数，用于验证 crate 是否正确加载。
/// 【核心职责】返回固定字符串 `"hello from code-agent-tools"` 作为健康检查标记。
pub fn hello() -> &'static str {
    "hello from code-agent-tools"
}
