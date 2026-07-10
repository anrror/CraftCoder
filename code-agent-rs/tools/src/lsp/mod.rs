//! LSP（语言服务器协议）集成层
//!
//! 【领域含义】为 AI 编码代理提供代码智能能力，包括跳转到定义、查找引用、悬停提示、诊断和文档符号。
//! 本模块封装了 LSP 协议的完整生命周期：服务器发现 → 进程启动 → 初始化握手 → 请求/响应 → 关闭。
//!
//! 核心组件：
//! - [`LspClient`][]: 高层代码智能操作 API
//! - [`LspServerManager`][]: 服务器生命周期管理
//! - [`config`][]: 支持的语言定义
//! - [`requests`]: JSON-RPC 2.0 类型和消息帧协议
//!
//! # 使用示例
//!
//! ```no_run
//! use code_agent_tools::lsp::LspClient;
//! use std::path::Path;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let client = LspClient::new();
//!
//! // 服务器按需延迟启动 — 首次请求某语言时自动启动
//! let locations = client
//!     .goto_definition("rust", Path::new("main.rs"), 10, 5)
//!     .await?;
//!
//! let diags = client.diagnostics("rust", Path::new("main.rs")).await?;
//! # Ok(())
//! # }
//! ```

pub mod config;
pub mod requests;
pub mod server;

use self::config::detect_language;
use self::requests::{
    DocumentSymbolParams, ReferenceContext, ReferenceParams, TextDocumentIdentifier,
    TextDocumentItem, TextDocumentPositionParams,
};
use self::server::{LspServerManager, ServerStatus};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// Public types (re-exported for convenience)
// ---------------------------------------------------------------------------

pub use self::requests::{
    Diagnostic, DiagnosticSeverity, Location, Position, Range, SymbolInformation, SymbolKind,
};
pub use self::server::LspError;

// ---------------------------------------------------------------------------
// LspClient
// ---------------------------------------------------------------------------

/// LSP 客户端 — 高层代码智能操作入口
///
/// 【领域含义】封装 `LspServerManager` 于 `Arc<Mutex<>>` 中，提供线程安全的代码智能 API。
/// 服务器按需延迟启动：首次请求某语言时自动检测并启动对应 LSP 服务器。
/// 语言可显式指定，也可通过文件扩展名自动检测。
///
/// 【核心职责】提供 goto_definition / find_references / hover / diagnostics / document_symbols 等操作。
pub struct LspClient {
    /// 内部服务器管理器（线程安全）
    manager: Arc<Mutex<LspServerManager>>,
    /// 工作区根 URI（发送给 LSP 服务器用于理解项目结构）
    root_uri: String,
}

impl LspClient {
    /// 创建 LSP 客户端（无运行中的服务器）
    ///
    /// 【领域含义】初始化一个空的 LSP 客户端，服务器将在首次使用时按需启动。
    /// 【核心职责】创建 `LspServerManager` 实例，包装在 `Arc<Mutex<>>` 中。
    pub fn new() -> Self {
        Self {
            manager: Arc::new(Mutex::new(LspServerManager::new())),
            root_uri: String::new(),
        }
    }

    /// 创建 LSP 客户端（指定工作区根目录）
    ///
    /// 【领域含义】初始化客户端并设置工作区根 URI，LSP 服务器初始化时使用此 URI 理解项目布局。
    /// 【核心职责】创建管理器并设置 `root_uri`。
    pub fn with_root(root_uri: impl Into<String>) -> Self {
        Self {
            manager: Arc::new(Mutex::new(LspServerManager::new())),
            root_uri: root_uri.into(),
        }
    }

    /// 设置工作区根 URI
    ///
    /// 【领域含义】更新 LSP 客户端的工作区根路径。
    /// 【核心职责】修改 `root_uri` 字段，后续启动的服务器将使用新值。
    pub fn set_root(&mut self, root_uri: impl Into<String>) {
        self.root_uri = root_uri.into();
    }

    /// 跳转到定义
    ///
    /// 【领域含义】查找指定位置符号的定义位置。
    /// 【核心职责】解析语言 → 确保服务器运行 → 发送 `textDocument/definition` 请求 → 解析返回的位置列表。
    /// 如果 `language` 为空字符串，则从文件扩展名自动检测。
    pub async fn goto_definition(
        &self,
        language: &str,
        file: &Path,
        line: usize,
        col: usize,
    ) -> Result<Vec<Location>, LspError> {
        let lang = self.resolve_language(language, file)?;
        self.ensure_server(&lang).await?;

        let params = TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: path_to_uri(file),
            },
            position: Position {
                line: line as u32,
                character: col as u32,
            },
        };

        let result = self
            .send(&lang, "textDocument/definition", Some(serialize(params)))
            .await?;

        parse_locations(result)
    }

    /// 查找所有引用
    ///
    /// 【领域含义】查找指定位置符号的所有引用位置。
    /// 【核心职责】解析语言 → 确保服务器运行 → 发送 `textDocument/references` 请求 → 解析位置列表。
    pub async fn find_references(
        &self,
        language: &str,
        file: &Path,
        line: usize,
        col: usize,
    ) -> Result<Vec<Location>, LspError> {
        let lang = self.resolve_language(language, file)?;
        self.ensure_server(&lang).await?;

        let params = ReferenceParams {
            text_document: TextDocumentIdentifier {
                uri: path_to_uri(file),
            },
            position: Position {
                line: line as u32,
                character: col as u32,
            },
            context: ReferenceContext {
                include_declaration: true,
            },
        };

        let result = self
            .send(&lang, "textDocument/references", Some(serialize(params)))
            .await?;

        parse_locations(result)
    }

    /// 获取悬停提示信息
    ///
    /// 【领域含义】获取指定位置符号的文档提示（Markdown 或纯文本）。
    /// 【核心职责】解析语言 → 确保服务器运行 → 发送 `textDocument/hover` 请求 → 提取文档内容。
    pub async fn hover(
        &self,
        language: &str,
        file: &Path,
        line: usize,
        col: usize,
    ) -> Result<Option<String>, LspError> {
        let lang = self.resolve_language(language, file)?;
        self.ensure_server(&lang).await?;

        let params = TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: path_to_uri(file),
            },
            position: Position {
                line: line as u32,
                character: col as u32,
            },
        };

        let result = self
            .send(&lang, "textDocument/hover", Some(serialize(params)))
            .await?;

        if result.is_null() {
            return Ok(None);
        }

        // Hover result: { contents: MarkupContent | string }
        let contents = result
            .get("contents")
            .and_then(|c| c.as_str())
            .or_else(|| {
                result
                    .get("contents")
                    .and_then(|c| c.get("value"))
                    .and_then(|v| v.as_str())
            })
            .map(|s| s.to_string());

        Ok(contents)
    }

    /// 获取文件诊断信息
    ///
    /// 【领域含义】获取指定文件的错误、警告、提示等诊断信息。
    /// 【核心职责】通过 `textDocument/didOpen` 通知服务器打开文件 → 等待处理 → 发送 `textDocument/diagnostic` 拉取诊断。
    /// 注意：部分服务器通过 `textDocument/publishDiagnostics` 推送诊断，本方法使用 LSP 3.17+ 的拉取模式。
    pub async fn diagnostics(
        &self,
        language: &str,
        file: &Path,
    ) -> Result<Vec<Diagnostic>, LspError> {
        let lang = self.resolve_language(language, file)?;
        self.ensure_server(&lang).await?;

        let uri = path_to_uri(file);

        // Notify the server that we opened this file
        self.send_notification(
            &lang,
            "textDocument/didOpen",
            Some(serialize(TextDocumentItem {
                uri: uri.clone(),
                language_id: lang.clone(),
                version: 1,
                text: String::new(), // Server should read from disk if needed
            })),
        )
        .await?;

        // Wait briefly for server to process
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Pull diagnostics (LSP 3.17+)
        let params = DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri },
        };

        let result = self
            .send(&lang, "textDocument/diagnostic", Some(serialize(params)))
            .await?;

        parse_diagnostics(result)
    }

    /// 获取文档符号（大纲）
    ///
    /// 【领域含义】获取文件中的符号列表（函数、类、变量等），用于生成文档大纲。
    /// 【核心职责】发送 `textDocument/documentSymbol` 请求 → 解析符号信息列表。
    pub async fn document_symbols(
        &self,
        language: &str,
        file: &Path,
    ) -> Result<Vec<SymbolInformation>, LspError> {
        let lang = self.resolve_language(language, file)?;
        self.ensure_server(&lang).await?;

        let params = DocumentSymbolParams {
            text_document: TextDocumentIdentifier {
                uri: path_to_uri(file),
            },
        };

        let result = self
            .send(&lang, "textDocument/documentSymbol", Some(serialize(params)))
            .await?;

        parse_symbols(result)
    }

    /// 获取 LSP 服务器状态
    ///
    /// 【领域含义】查询指定语言的 LSP 服务器当前状态（未启动/初始化中/运行中/已崩溃）。
    /// 【核心职责】尝试获取锁 → 查询服务器状态 → 返回状态枚举。
    pub fn server_status(&self, language: &str) -> ServerStatus {
        // try_lock because we don't want to block — status is best-effort
        if let Ok(mgr) = self.manager.try_lock() {
            mgr.server_status(language)
        } else {
            ServerStatus::Initializing
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Resolve the language identifier from either an explicit string or
    /// auto-detection from the file extension.
    fn resolve_language(&self, language: &str, file: &Path) -> Result<String, LspError> {
        if !language.is_empty() {
            return Ok(language.to_string());
        }
        detect_language(file)
            .map(|s| s.to_string())
            .ok_or_else(|| LspError::UnsupportedLanguage(
                file.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("<unknown>")
                    .to_string(),
            ))
    }

    /// Ensure an LSP server is running for the given language. Starts one if
    /// not already running.
    async fn ensure_server(&self, language: &str) -> Result<(), LspError> {
        let mut mgr = self.manager.lock().await;

        match mgr.server_status(language) {
            ServerStatus::NotStarted | ServerStatus::Crashed => {
                let root_path = if self.root_uri.is_empty() {
                    // Use current directory as fallback
                    Path::new(".")
                } else {
                    Path::new(&self.root_uri)
                };
                mgr.start_server(language, root_path).await
            }
            ServerStatus::Running => Ok(()),
            ServerStatus::Initializing => {
                // Wait briefly for initialization to complete
                drop(mgr);
                for _ in 0..30 {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    let mgr = self.manager.lock().await;
                    if mgr.server_status(language) == ServerStatus::Running {
                        return Ok(());
                    }
                }
                Err(LspError::Timeout {
                    language: language.to_string(),
                })
            }
        }
    }

    /// Send an LSP request and return the JSON result.
    async fn send(
        &self,
        language: &str,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, LspError> {
        let mgr = self.manager.lock().await;
        mgr.send_request(language, method, params).await
    }

    /// Send an LSP notification (fire-and-forget).
    async fn send_notification(
        &self,
        language: &str,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), LspError> {
        let mgr = self.manager.lock().await;
        let handle = mgr.send_request(language, method, params).await;
        // Notifications don't expect a meaningful response
        let _ = handle;
        Ok(())
    }
}

impl Default for LspClient {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// 将文件路径转换为 `file://` URI
///
/// 【领域含义】将本地文件系统路径转换为 LSP 协议使用的 `file://` URI 格式。
/// 【核心职责】相对路径转绝对 → 规范化 → 替换反斜杠 → 添加 `file:///` 前缀。
fn path_to_uri(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| Path::new(".").to_path_buf())
            .join(path)
    };
    let canonical = abs
        .canonicalize()
        .unwrap_or(abs);
    format!("file:///{}", canonical.to_string_lossy().replace('\\', "/"))
}

/// 序列化值为 JSON
///
/// 【领域含义】将任意可序列化类型转换为 `serde_json::Value`。
/// 【核心职责】调用 `serde_json::to_value`，失败时返回 `Value::Null`。
fn serialize<T: serde::Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// 解析 LSP 响应为位置列表
///
/// 【领域含义】将 `textDocument/definition` 或 `textDocument/references` 的 JSON 响应解析为 `Vec<Location>`。
/// 【核心职责】处理 null → 数组 → 单个位置的三种响应格式。
fn parse_locations(value: Value) -> Result<Vec<Location>, LspError> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    if let Some(arr) = value.as_array() {
        return serde_json::from_value(Value::Array(arr.clone()))
            .map_err(|e| LspError::ProtocolError {
                language: "<unknown>".to_string(),
                code: -1,
                message: e.to_string(),
            });
    }
    // Single location
    match serde_json::from_value::<Location>(value) {
        Ok(loc) => Ok(vec![loc]),
        Err(_) => Ok(Vec::new()),
    }
}

/// 解析 LSP 响应为诊断列表
///
/// 【领域含义】将 `textDocument/diagnostic` 的 JSON 响应解析为 `Vec<Diagnostic>`。
/// 【核心职责】处理 null → `{ kind, items }` 格式 → 直接数组格式。
fn parse_diagnostics(value: Value) -> Result<Vec<Diagnostic>, LspError> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    // textDocument/diagnostic returns { kind: "full", items: [...] }
    if let Some(items) = value.get("items").and_then(|i| i.as_array()) {
        return serde_json::from_value(Value::Array(items.clone())).map_err(|e| {
            LspError::ProtocolError {
                language: "<unknown>".to_string(),
                code: -1,
                message: e.to_string(),
            }
        });
    }
    // Some servers return diagnostics directly as an array
    if let Some(arr) = value.as_array() {
        return serde_json::from_value(Value::Array(arr.clone())).map_err(|e| {
            LspError::ProtocolError {
                language: "<unknown>".to_string(),
                code: -1,
                message: e.to_string(),
            }
        });
    }
    Ok(Vec::new())
}

/// 解析 LSP 响应为符号信息列表
///
/// 【领域含义】将 `textDocument/documentSymbol` 的 JSON 响应解析为 `Vec<SymbolInformation>`。
/// 【核心职责】处理 null → 数组 → 单个符号的三种响应格式。
fn parse_symbols(value: Value) -> Result<Vec<SymbolInformation>, LspError> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    if let Some(arr) = value.as_array() {
        return serde_json::from_value(Value::Array(arr.clone())).map_err(|e| {
            LspError::ProtocolError {
                language: "<unknown>".to_string(),
                code: -1,
                message: e.to_string(),
            }
        });
    }
    // Single symbol
    match serde_json::from_value::<SymbolInformation>(value) {
        Ok(sym) => Ok(vec![sym]),
        Err(_) => Ok(Vec::new()),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_to_uri_converts_windows_paths() {
        let uri = path_to_uri(Path::new("src/main.rs"));
        assert!(uri.starts_with("file:///"));
        assert!(uri.ends_with("src/main.rs"));
    }

    #[test]
    fn parse_locations_null_returns_empty() {
        let result = parse_locations(Value::Null).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_locations_empty_array() {
        let result = parse_locations(serde_json::json!([])).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_locations_single() {
        let result = parse_locations(serde_json::json!({
            "uri": "file:///test.rs",
            "range": {
                "start": {"line": 1, "character": 0},
                "end": {"line": 1, "character": 10}
            }
        }))
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].uri, "file:///test.rs");
    }

    #[test]
    fn parse_diagnostics_null_returns_empty() {
        let result = parse_diagnostics(Value::Null).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_diagnostics_items_field() {
        let result = parse_diagnostics(serde_json::json!({
            "kind": "full",
            "items": [
                {
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 5}
                    },
                    "severity": 1,
                    "message": "expected `;`"
                }
            ]
        }))
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].message, "expected `;`");
    }

    #[test]
    fn parse_symbols_null_returns_empty() {
        let result = parse_symbols(Value::Null).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn resolve_language_from_extension() {
        let client = LspClient::new();
        let lang = client
            .resolve_language("", Path::new("foo.rs"))
            .unwrap();
        assert_eq!(lang, "rust");
    }

    #[test]
    fn resolve_language_explicit_overrides_extension() {
        let client = LspClient::new();
        let lang = client
            .resolve_language("python", Path::new("foo.rs"))
            .unwrap();
        assert_eq!(lang, "python");
    }

    #[test]
    fn resolve_language_unknown_extension() {
        let client = LspClient::new();
        let result = client.resolve_language("", Path::new("README.md"));
        assert!(result.is_err());
    }
}
