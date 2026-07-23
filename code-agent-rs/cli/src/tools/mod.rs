//! Tool adapter layer — bridges `code-agent-tools` APIs to `Tool` trait.
//!
//! 【领域含义】`code-agent-core` 定义了 `Tool` trait，但 Shell、Git、LSP 等
//! 具体实现在 `code-agent-tools` crate 中。本模块提供适配器层，将后者包装为
//! 前者，使它们可以通过 `ToolRegistry` 注册给 Agent 使用。
//!
//! 【核心职责】每个适配器实现 `Tool` trait，将 JSON 输入参数转换为底层 API 调用，
//! 并将结果序列化为 `ToolResultMessage`。

pub mod shell;
pub mod git;
pub mod lsp;
pub mod mcp;
