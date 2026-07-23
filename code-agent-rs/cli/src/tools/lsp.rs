//! LspTool 系列 — 语言服务器协议工具适配器
//!
//! 【领域含义】将 `code_agent_tools::lsp::LspClient` 包装为 `Tool` trait，
//! 提供 `lsp_diagnostics`、`lsp_definition`、`lsp_references`、`lsp_hover`
//! 四个工具，使 Agent 具备 IDE 级别的代码智能能力。
//!
//! 【核心职责】每个工具接收文件路径和位置参数，通过 LSP 协议查询语言服务器，
//! 返回结构化的诊断信息、定义位置、引用列表或悬停提示。

use async_trait::async_trait;
use code_agent_core::tools::{Tool, ToolDefinition, ToolError, tool_success, tool_error};
use code_agent_protocol::{CapabilityLevel, ToolResultMessage};
use code_agent_tools::lsp::LspClient;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// LSP 工具共享的客户端包装，延迟初始化 LspClient
struct LspState {
    client: LspClient,
}

impl LspState {
    fn new() -> Self {
        Self {
            client: LspClient::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// LspDiagnosticsTool — 文件诊断
// ---------------------------------------------------------------------------

/// `lsp_diagnostics` — 获取文件的编译/语法诊断信息
///
/// 参数：
/// - `file` (string, 必需): 文件路径
/// - `language` (string, 可选): 语言名称（如 "rust", "python"），自动检测
pub struct LspDiagnosticsTool {
    inner: Arc<Mutex<LspState>>,
}

impl LspDiagnosticsTool {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(LspState::new())),
        }
    }
}

impl Default for LspDiagnosticsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for LspDiagnosticsTool {
    fn name(&self) -> &str {
        "lsp_diagnostics"
    }

    fn description(&self) -> &str {
        "获取文件的 LSP 诊断信息（错误、警告、提示）。需要对应的语言服务器已安装。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "文件路径"
                },
                "language": {
                    "type": "string",
                    "description": "语言名称（可选，从文件扩展名自动检测）"
                }
            },
            "required": ["file"]
        })
    }

    fn capability(&self) -> CapabilityLevel {
        CapabilityLevel::Read
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let file = params
            .get("file")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::invalid_input("missing required field 'file'"))?;
        let language = params
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let state = self.inner.lock().await;
        match state.client.diagnostics(language, PathBuf::from(file).as_path()).await {
            Ok(diags) => {
                if diags.is_empty() {
                    return Ok(tool_success("", "No diagnostics found."));
                }
                    let output: Vec<serde_json::Value> = diags.into_iter().map(|d| {
                        serde_json::json!({
                            "message": d.message,
                            "severity": d.severity.map(|s| s as u8),
                            "range": {
                                "start": { "line": d.range.start.line, "character": d.range.start.character },
                                "end": { "line": d.range.end.line, "character": d.range.end.character },
                            },
                        })
                    }).collect();
                Ok(tool_success(
                    "",
                    serde_json::to_string_pretty(&output)
                        .unwrap_or_else(|_| "failed to serialize".to_string()),
                ))
            }
            Err(e) => Ok(tool_error("", &format!("LSP diagnostics failed: {e}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// LspDefinitionTool — 跳转到定义
// ---------------------------------------------------------------------------

/// `lsp_definition` — 查找符号的定义位置
///
/// 参数：
/// - `file` (string, 必需): 文件路径
/// - `line` (integer, 必需): 行号（从 0 开始）
/// - `character` (integer, 必需): 列号（从 0 开始）
/// - `language` (string, 可选): 语言名称
pub struct LspDefinitionTool {
    inner: Arc<Mutex<LspState>>,
}

impl LspDefinitionTool {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(LspState::new())),
        }
    }
}

impl Default for LspDefinitionTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for LspDefinitionTool {
    fn name(&self) -> &str {
        "lsp_definition"
    }

    fn description(&self) -> &str {
        "跳转到符号定义。返回符号定义的文件路径和行列位置。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "文件路径"
                },
                "line": {
                    "type": "integer",
                    "description": "行号（从 0 开始）"
                },
                "character": {
                    "type": "integer",
                    "description": "列号（从 0 开始）"
                },
                "language": {
                    "type": "string",
                    "description": "语言名称（可选，从文件扩展名自动检测）"
                }
            },
            "required": ["file", "line", "character"]
        })
    }

    fn capability(&self) -> CapabilityLevel {
        CapabilityLevel::Read
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let file = params
            .get("file")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::invalid_input("missing required field 'file'"))?;
        let line = params
            .get("line")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| ToolError::invalid_input("missing required field 'line'"))? as usize;
        let character = params
            .get("character")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| ToolError::invalid_input("missing required field 'character'"))? as usize;
        let language = params
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let state = self.inner.lock().await;
        match state.client.goto_definition(language, PathBuf::from(file).as_path(), line, character).await {
            Ok(locations) => {
                if locations.is_empty() {
                    return Ok(tool_success("", "No definition found."));
                }
                let output: Vec<serde_json::Value> = locations.into_iter().map(|loc| {
                    serde_json::json!({
                        "uri": loc.uri,
                        "range": {
                            "start": { "line": loc.range.start.line, "character": loc.range.start.character },
                            "end": { "line": loc.range.end.line, "character": loc.range.end.character },
                        },
                    })
                }).collect();
                Ok(tool_success(
                    "",
                    serde_json::to_string_pretty(&output)
                        .unwrap_or_else(|_| "[]".to_string()),
                ))
            }
            Err(e) => Ok(tool_error("", &format!("LSP definition failed: {e}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// LspReferencesTool — 查找引用
// ---------------------------------------------------------------------------

/// `lsp_references` — 查找符号的所有引用位置
///
/// 参数：
/// - `file` (string, 必需): 文件路径
/// - `line` (integer, 必需): 行号
/// - `character` (integer, 必需): 列号
/// - `language` (string, 可选): 语言名称
pub struct LspReferencesTool {
    inner: Arc<Mutex<LspState>>,
}

impl LspReferencesTool {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(LspState::new())),
        }
    }
}

impl Default for LspReferencesTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for LspReferencesTool {
    fn name(&self) -> &str {
        "lsp_references"
    }

    fn description(&self) -> &str {
        "查找符号的所有引用位置。返回引用列表（文件路径 + 行列位置）。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "文件路径"
                },
                "line": {
                    "type": "integer",
                    "description": "行号（从 0 开始）"
                },
                "character": {
                    "type": "integer",
                    "description": "列号（从 0 开始）"
                },
                "language": {
                    "type": "string",
                    "description": "语言名称（可选，从文件扩展名自动检测）"
                }
            },
            "required": ["file", "line", "character"]
        })
    }

    fn capability(&self) -> CapabilityLevel {
        CapabilityLevel::Read
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let file = params
            .get("file")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::invalid_input("missing required field 'file'"))?;
        let line = params
            .get("line")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| ToolError::invalid_input("missing required field 'line'"))? as usize;
        let character = params
            .get("character")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| ToolError::invalid_input("missing required field 'character'"))? as usize;
        let language = params
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let state = self.inner.lock().await;
        match state.client.find_references(language, PathBuf::from(file).as_path(), line, character).await {
            Ok(locations) => {
                if locations.is_empty() {
                    return Ok(tool_success("", "No references found."));
                }
                let output: Vec<serde_json::Value> = locations.into_iter().map(|loc| {
                    serde_json::json!({
                        "uri": loc.uri,
                        "range": {
                            "start": { "line": loc.range.start.line, "character": loc.range.start.character },
                            "end": { "line": loc.range.end.line, "character": loc.range.end.character },
                        },
                    })
                }).collect();
                Ok(tool_success(
                    "",
                    serde_json::to_string_pretty(&output)
                        .unwrap_or_else(|_| "[]".to_string()),
                ))
            }
            Err(e) => Ok(tool_error("", &format!("LSP references failed: {e}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// LspHoverTool — 悬停提示
// ---------------------------------------------------------------------------

/// `lsp_hover` — 获取符号的悬停提示信息
///
/// 参数：
/// - `file` (string, 必需): 文件路径
/// - `line` (integer, 必需): 行号
/// - `character` (integer, 必需): 列号
/// - `language` (string, 可选): 语言名称
pub struct LspHoverTool {
    inner: Arc<Mutex<LspState>>,
}

impl LspHoverTool {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(LspState::new())),
        }
    }
}

impl Default for LspHoverTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for LspHoverTool {
    fn name(&self) -> &str {
        "lsp_hover"
    }

    fn description(&self) -> &str {
        "获取符号的悬停提示信息（类型签名、文档注释等）。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "文件路径"
                },
                "line": {
                    "type": "integer",
                    "description": "行号（从 0 开始）"
                },
                "character": {
                    "type": "integer",
                    "description": "列号（从 0 开始）"
                },
                "language": {
                    "type": "string",
                    "description": "语言名称（可选，从文件扩展名自动检测）"
                }
            },
            "required": ["file", "line", "character"]
        })
    }

    fn capability(&self) -> CapabilityLevel {
        CapabilityLevel::Read
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let file = params
            .get("file")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::invalid_input("missing required field 'file'"))?;
        let line = params
            .get("line")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| ToolError::invalid_input("missing required field 'line'"))? as usize;
        let character = params
            .get("character")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| ToolError::invalid_input("missing required field 'character'"))? as usize;
        let language = params
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let state = self.inner.lock().await;
        match state.client.hover(language, PathBuf::from(file).as_path(), line, character).await {
            Ok(Some(info)) => Ok(tool_success("", info)),
            Ok(None) => Ok(tool_success("", "No hover information available.")),
            Err(e) => Ok(tool_error("", &format!("LSP hover failed: {e}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// 元数据集成
// ---------------------------------------------------------------------------

/// 返回所有 LSP 工具的元数据定义列表
pub fn lsp_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "lsp_diagnostics".to_string(),
            description: "获取文件的 LSP 诊断信息（错误、警告、提示）。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "文件路径" },
                    "language": { "type": "string", "description": "语言名称" }
                },
                "required": ["file"]
            }),
        },
        ToolDefinition {
            name: "lsp_definition".to_string(),
            description: "跳转到符号定义。返回定义的文件和行列位置。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "文件路径" },
                    "line": { "type": "integer", "description": "行号" },
                    "character": { "type": "integer", "description": "列号" },
                    "language": { "type": "string", "description": "语言名称" }
                },
                "required": ["file", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "lsp_references".to_string(),
            description: "查找符号的所有引用位置。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "文件路径" },
                    "line": { "type": "integer", "description": "行号" },
                    "character": { "type": "integer", "description": "列号" },
                    "language": { "type": "string", "description": "语言名称" }
                },
                "required": ["file", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "lsp_hover".to_string(),
            description: "获取符号的悬停提示信息（类型签名、文档注释等）。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "文件路径" },
                    "line": { "type": "integer", "description": "行号" },
                    "character": { "type": "integer", "description": "列号" },
                    "language": { "type": "string", "description": "语言名称" }
                },
                "required": ["file", "line", "character"]
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_tools_metadata() {
        let diag = LspDiagnosticsTool::new();
        assert_eq!(diag.name(), "lsp_diagnostics");
        assert_eq!(diag.capability(), CapabilityLevel::Read);

        let def = LspDefinitionTool::new();
        assert_eq!(def.name(), "lsp_definition");

        let refs = LspReferencesTool::new();
        assert_eq!(refs.name(), "lsp_references");

        let hover = LspHoverTool::new();
        assert_eq!(hover.name(), "lsp_hover");
    }

    #[test]
    fn lsp_tool_definitions_count() {
        let defs = lsp_tool_definitions();
        assert_eq!(defs.len(), 4);
        for def in &defs {
            assert!(!def.name.is_empty());
            assert!(!def.description.is_empty());
            assert!(def.input_schema.is_object());
        }
    }

    #[test]
    fn lsp_diagnostics_rejects_empty_params() {
        let tool = LspDiagnosticsTool::new();
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(serde_json::json!({})));
        assert!(result.is_err());
    }
}
