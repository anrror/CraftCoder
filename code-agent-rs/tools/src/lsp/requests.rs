//! LSP 协议类型与 JSON-RPC 2.0 消息帧
//!
//! 【领域含义】定义 LSP（语言服务器协议）通信所需的数据结构和消息帧协议。
//! 每条消息以 `Content-Length: N\r\n\r\n` 头部开头，后跟 N 字节的 JSON-RPC 2.0 体。
//!
//! 【核心职责】提供 LSP 协议类型的序列化/反序列化，以及 stdin/stdout 上的消息读写。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 types
// ---------------------------------------------------------------------------

/// JSON-RPC 2.0 消息 — 请求、响应或通知
///
/// 【领域含义】LSP 通信的基本消息单元，可以是请求（含 id）或响应。
/// 通知通过 `id` 字段的缺失来识别。
/// 【核心职责】作为消息帧读写操作的顶层类型，区分请求和响应两种变体。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LspMessage {
    /// JSON-RPC 请求（含 id）或通知（不含 id）
    Request(LspRequest),
    /// JSON-RPC 响应
    Response(LspResponse),
}

/// JSON-RPC 请求
///
/// 【领域含义】从客户端发往服务器的请求消息，或从服务器发往客户端的请求。
/// 当 `id` 为 `None` 时表示通知（不需要响应）。
/// 【核心职责】携带方法名和参数，通过 `id` 字段关联请求与响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspRequest {
    /// JSON-RPC 版本标识（固定为 "2.0"）
    pub jsonrpc: String,
    /// 请求 ID（None 表示通知）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<RequestId>,
    /// 要调用的方法名
    pub method: String,
    /// 方法参数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// JSON-RPC 响应
///
/// 【领域含义】服务器对请求的响应消息，包含结果或错误。
/// 【核心职责】通过 `id` 与请求关联，携带 `result` 或 `error`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspResponse {
    /// JSON-RPC 版本标识（固定为 "2.0"）
    pub jsonrpc: String,
    /// 对应请求的 ID
    pub id: RequestId,
    /// 响应结果（成功时存在）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// 错误信息（失败时存在）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<LspError>,
}

/// JSON-RPC 错误对象
///
/// 【领域含义】LSP 协议中的错误描述，包含错误码和消息。
/// 【核心职责】在响应中携带错误信息，供调用方诊断问题。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspError {
    /// 错误码（负值表示协议级错误，正值表示应用级错误）
    pub code: i64,
    /// 错误描述信息
    pub message: String,
    /// 附加错误数据
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// JSON-RPC 请求 ID
///
/// 【领域含义】请求的唯一标识符，可以是数字或字符串。
/// 【核心职责】用于匹配请求和响应，支持 `i64` 和 `String` 两种格式。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// 数字类型 ID
    Number(i64),
    /// 字符串类型 ID
    String(String),
}

// ---------------------------------------------------------------------------
// LSP protocol — initialize
// ---------------------------------------------------------------------------

/// 初始化请求参数
///
/// 【领域含义】LSP `initialize` 请求的参数，客户端告知服务器其进程信息、工作区和能力。
/// 【核心职责】携带进程 ID、根 URI、客户端能力等初始化信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    /// 客户端进程 ID
    #[serde(rename = "processId")]
    pub process_id: Option<u64>,
    /// 工作区根 URI
    #[serde(rename = "rootUri")]
    pub root_uri: Option<String>,
    /// 工作区根路径（已弃用，保留兼容）
    #[serde(rename = "rootPath")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_path: Option<String>,
    /// 客户端能力声明
    pub capabilities: ClientCapabilities,
    /// 工作区文件夹列表
    #[serde(rename = "workspaceFolders")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_folders: Option<Vec<WorkspaceFolder>>,
}

/// 客户端能力声明
///
/// 【领域含义】客户端在初始化时向服务器声明其支持的功能。
/// 【核心职责】告知服务器客户端的工作区和文本文档能力。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientCapabilities {
    /// 工作区相关能力
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Value>,
    /// 文本文档相关能力
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "textDocument")]
    pub text_document: Option<Value>,
}

#[allow(clippy::derivable_impls)]
impl Default for ClientCapabilities {
    fn default() -> Self {
        Self {
            workspace: None,
            text_document: None,
        }
    }
}

/// 初始化结果
///
/// 【领域含义】LSP `initialize` 请求的响应，包含服务器能力和信息。
/// 【核心职责】携带服务器能力声明和可选的服务器元信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    /// 服务器能力
    pub capabilities: ServerCapabilities,
    /// 服务器信息（名称、版本）
    #[serde(rename = "serverInfo")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_info: Option<ServerInfo>,
}

/// 服务器能力声明
///
/// 【领域含义】LSP 服务器在初始化时向客户端声明其支持的功能。
/// 【核心职责】告知客户端服务器支持哪些代码智能操作（悬停、补全、定义跳转等）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerCapabilities {
    /// 文本文档同步方式
    #[serde(rename = "textDocumentSync")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_document_sync: Option<Value>,
    /// 是否支持悬停提示
    #[serde(rename = "hoverProvider")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hover_provider: Option<Value>,
    /// 是否支持自动补全
    #[serde(rename = "completionProvider")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_provider: Option<Value>,
    /// 是否支持跳转到定义
    #[serde(rename = "definitionProvider")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition_provider: Option<Value>,
    /// 是否支持查找引用
    #[serde(rename = "referencesProvider")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub references_provider: Option<Value>,
    /// 是否支持文档符号
    #[serde(rename = "documentSymbolProvider")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_symbol_provider: Option<Value>,
    /// 是否支持诊断拉取
    #[serde(rename = "diagnosticProvider")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_provider: Option<Value>,
}

/// 服务器信息
///
/// 【领域含义】LSP 服务器的名称和版本信息。
/// 【核心职责】在初始化响应中标识服务器实现。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    /// 服务器名称
    pub name: String,
    /// 服务器版本（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// 工作区文件夹描述
///
/// 【领域含义】LSP 协议中的工作区文件夹，包含 URI 和名称。
/// 【核心职责】用于多根工作区场景，标识一个工作区文件夹。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceFolder {
    /// 文件夹 URI
    pub uri: String,
    /// 文件夹名称
    pub name: String,
}

// ---------------------------------------------------------------------------
// LSP protocol — text document
// ---------------------------------------------------------------------------

/// 文本文档标识符
///
/// 【领域含义】LSP 协议中标识一个文本文档的 URI。
/// 【核心职责】在请求参数中指定要操作的目标文档。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextDocumentIdentifier {
    /// 文档 URI
    pub uri: String,
}

/// 版本化文本文档标识符
///
/// 【领域含义】带版本号的文档标识符，用于 `didOpen`/`didChange` 等操作。
/// 【核心职责】标识文档及其当前版本，供服务器跟踪文档变更。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionedTextDocumentIdentifier {
    /// 文档 URI
    pub uri: String,
    /// 文档版本号
    pub version: i64,
}

/// 文本文档内容
///
/// 【领域含义】包含完整内容的文本文档，用于 `didOpen` 通知。
/// 【核心职责】携带文档 URI、语言标识、版本号和完整文本内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextDocumentItem {
    /// 文档 URI
    pub uri: String,
    /// 语言标识符
    #[serde(rename = "languageId")]
    pub language_id: String,
    /// 版本号
    pub version: i64,
    /// 文档完整文本
    pub text: String,
}

/// 文本文档位置参数
///
/// 【领域含义】指定文本文档中某个位置的参数，用于定义跳转、引用查找等操作。
/// 【核心职责】组合文档标识符和位置信息，作为 LSP 请求的通用参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextDocumentPositionParams {
    /// 目标文档
    #[serde(rename = "textDocument")]
    pub text_document: TextDocumentIdentifier,
    /// 文档中的位置
    pub position: Position,
}

/// 引用查找参数
///
/// 【领域含义】`textDocument/references` 请求的参数，包含位置和引用上下文。
/// 【核心职责】指定要查找引用的位置以及是否包含声明本身。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceParams {
    /// 目标文档
    #[serde(rename = "textDocument")]
    pub text_document: TextDocumentIdentifier,
    /// 文档中的位置
    pub position: Position,
    /// 引用上下文
    #[serde(rename = "context")]
    pub context: ReferenceContext,
}

/// 引用上下文
///
/// 【领域含义】控制引用查找行为的上下文参数。
/// 【核心职责】指定是否在结果中包含符号的声明位置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceContext {
    /// 是否包含声明
    #[serde(rename = "includeDeclaration")]
    pub include_declaration: bool,
}

/// 文档符号参数
///
/// 【领域含义】`textDocument/documentSymbol` 请求的参数。
/// 【核心职责】指定要获取符号列表的目标文档。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSymbolParams {
    /// 目标文档
    #[serde(rename = "textDocument")]
    pub text_document: TextDocumentIdentifier,
}

// ---------------------------------------------------------------------------
// LSP protocol — types
// ---------------------------------------------------------------------------

/// 文档中的位置（0 基）
///
/// 【领域含义】LSP 协议中表示文本文档中的一个位置，行和列均从 0 开始计数。
/// 【核心职责】精确定位文档中的某个点，用于定义跳转、引用、悬停等操作。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Position {
    /// 行号（0 基）
    pub line: u32,
    /// 列号（0 基）
    pub character: u32,
}

/// 文档中的范围
///
/// 【领域含义】LSP 协议中表示文本文档中的一个连续范围，由起始和结束位置定义。
/// 【核心职责】标识代码片段、诊断位置、符号定义范围等。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Range {
    /// 起始位置
    pub start: Position,
    /// 结束位置
    pub end: Position,
}

/// 源码文件中的位置
///
/// 【领域含义】LSP 协议中表示一个源码位置，包含文件 URI 和范围。
/// 【核心职责】作为定义跳转、引用查找等操作的结果类型。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    /// 文件 URI
    pub uri: String,
    /// 位置范围
    pub range: Range,
}

/// 诊断信息（错误、警告、提示等）
///
/// 【领域含义】LSP 协议中的诊断信息，表示代码中的问题（错误、警告、信息、提示）。
/// 【核心职责】携带问题位置、严重级别、消息和来源。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    /// 问题位置范围
    pub range: Range,
    /// 严重级别
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<DiagnosticSeverity>,
    /// 诊断消息
    pub message: String,
    /// 诊断来源（如 "rustc"、"eslint"）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 错误码
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<Value>,
}

/// 诊断严重级别
///
/// 【领域含义】LSP 协议中诊断信息的严重程度分级。
/// 【核心职责】区分错误、警告、信息和提示四种级别。
/// LSP 规范要求 severity 为整数：1=Error, 2=Warning, 3=Information, 4=Hint
#[derive(Debug, Clone, Copy)]
pub enum DiagnosticSeverity {
    /// 错误
    Error = 1,
    /// 警告
    Warning = 2,
    /// 信息
    Information = 3,
    /// 提示
    Hint = 4,
}

impl DiagnosticSeverity {
    fn from_i32(v: i32) -> Option<Self> {
        match v {
            1 => Some(Self::Error),
            2 => Some(Self::Warning),
            3 => Some(Self::Information),
            4 => Some(Self::Hint),
            _ => None,
        }
    }
}

impl Serialize for DiagnosticSeverity {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for DiagnosticSeverity {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SeverityVisitor;
        impl<'de> serde::de::Visitor<'de> for SeverityVisitor {
            type Value = DiagnosticSeverity;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an integer between 1 and 4 or a string like \"error\"")
            }
            fn visit_i32<E: serde::de::Error>(self, v: i32) -> Result<DiagnosticSeverity, E> {
                DiagnosticSeverity::from_i32(v).ok_or_else(|| E::custom(format!("unknown DiagnosticSeverity value: {v}")))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<DiagnosticSeverity, E> {
                let v32 = i32::try_from(v).map_err(|_| E::custom(format!("DiagnosticSeverity out of range: {v}")))?;
                DiagnosticSeverity::from_i32(v32).ok_or_else(|| E::custom(format!("unknown DiagnosticSeverity value: {v}")))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<DiagnosticSeverity, E> {
                let v32 = i32::try_from(v).map_err(|_| E::custom(format!("DiagnosticSeverity out of range: {v}")))?;
                DiagnosticSeverity::from_i32(v32).ok_or_else(|| E::custom(format!("unknown DiagnosticSeverity value: {v}")))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<DiagnosticSeverity, E> {
                match v {
                    "error" | "Error" => Ok(DiagnosticSeverity::Error),
                    "warning" | "Warning" => Ok(DiagnosticSeverity::Warning),
                    "information" | "Information" => Ok(DiagnosticSeverity::Information),
                    "hint" | "Hint" => Ok(DiagnosticSeverity::Hint),
                    _ => Err(E::custom(format!("unknown DiagnosticSeverity string: {v}"))),
                }
            }
        }
        deserializer.deserialize_any(SeverityVisitor)
    }
}

/// 符号信息（函数、类、变量等）
///
/// 【领域含义】LSP 协议中的符号信息，描述文档中的命名元素。
/// 【核心职责】携带符号名称、类型、位置和容器信息，用于文档大纲。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolInformation {
    /// 符号名称
    pub name: String,
    /// 符号类型
    pub kind: SymbolKind,
    /// 符号位置
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    /// 符号范围
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
    /// 容器名称（如类名、模块名）
    #[serde(rename = "containerName")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
}

/// 符号类型枚举
///
/// 【领域含义】LSP 协议定义的符号类型分类，涵盖编程语言中常见的命名元素。
/// 【核心职责】标识符号的种类（函数、类、变量、接口等）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SymbolKind {
    /// 文件
    File = 1,
    /// 模块
    Module = 2,
    /// 命名空间
    Namespace = 3,
    /// 包
    Package = 4,
    /// 类
    Class = 5,
    /// 方法
    Method = 6,
    /// 属性
    Property = 7,
    /// 字段
    Field = 8,
    /// 构造函数
    Constructor = 9,
    /// 枚举
    Enum = 10,
    /// 接口
    Interface = 11,
    /// 函数
    Function = 12,
    /// 变量
    Variable = 13,
    /// 常量
    Constant = 14,
    /// 字符串
    String = 15,
    /// 数字
    Number = 16,
    /// 布尔
    Boolean = 17,
    /// 数组
    Array = 18,
    /// 对象
    Object = 19,
    /// 键
    Key = 20,
    /// 空
    Null = 21,
    /// 枚举成员
    EnumMember = 22,
    /// 结构体
    Struct = 23,
    /// 事件
    Event = 24,
    /// 运算符
    Operator = 25,
    /// 类型参数
    TypeParameter = 26,
}

/// 发布诊断通知参数
///
/// 【领域含义】`textDocument/publishDiagnostics` 通知的参数，服务器主动推送诊断信息。
/// 【核心职责】携带文档 URI 和诊断列表。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishDiagnosticsParams {
    /// 文档 URI
    pub uri: String,
    /// 诊断列表
    pub diagnostics: Vec<Diagnostic>,
}

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 framing (Content-Length header) over byte streams
// ---------------------------------------------------------------------------

/// 从异步读取器中读取一条 LSP 消息
///
/// 【领域含义】按照 LSP 消息帧协议，先读取 `Content-Length: N\r\n\r\n` 头部，再读取 N 字节 JSON 体。
/// 【核心职责】解析头部 → 读取指定长度的 JSON 体 → 反序列化为 `LspMessage`。
pub async fn read_message<R>(reader: &mut BufReader<R>) -> io::Result<LspMessage>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let content_length = read_content_length_header(reader).await?;

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).await?;

    let msg: LspMessage =
        serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(msg)
}

/// 向异步写入器写入一条 LSP 消息
///
/// 【领域含义】按照 LSP 消息帧协议，先写入 `Content-Length: N\r\n\r\n` 头部，再写入 JSON 体。
/// 【核心职责】序列化消息 → 计算长度 → 写入头部 → 写入 JSON 体 → 刷新。
pub async fn write_message<W>(writer: &mut W, msg: &LspMessage) -> io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

/// 将原始 JSON 值作为 LSP 消息写入（便捷方法）
///
/// 【领域含义】直接写入已序列化的 JSON 值，跳过 `LspMessage` 枚举包装。
/// 【核心职责】序列化 JSON → 写入 Content-Length 头部 → 写入 JSON 体。
pub async fn write_json_message<W>(writer: &mut W, value: &Value) -> io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

/// 从读取器中读取 Content-Length 头部
///
/// 【领域含义】读取 `Content-Length: <digits>\r\n\r\n` 格式的头部，忽略其他头部（如 Content-Type）。
/// 【核心职责】逐行读取 → 匹配 `Content-Length:` 前缀 → 解析数字 → 返回内容长度。
async fn read_content_length_header<R>(reader: &mut BufReader<R>) -> io::Result<usize>
where
    R: tokio::io::AsyncRead + Unpin,
{
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unexpected EOF while reading Content-Length header",
            ));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            // Empty line after all headers → end of header block.
            // But we need to have found Content-Length by now.
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "no Content-Length header found before empty line",
            ));
        }
        if let Some(len_str) = line.strip_prefix("Content-Length: ") {
            let length = len_str.trim().parse::<usize>().map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("invalid Content-Length: {e}"))
            })?;
            // Consume the empty line (\r\n) that terminates the header block
            let mut empty_line = String::new();
            reader.read_line(&mut empty_line).await?;
            return Ok(length);
        }
        // Ignore other headers (Content-Type, etc.)
    }
}

// ---------------------------------------------------------------------------
// Helpers for constructing LSP messages
// ---------------------------------------------------------------------------

/// 构建 JSON-RPC 请求消息
///
/// 【领域含义】创建一个 LSP 请求消息，包含方法名和参数。
/// 【核心职责】填充 `jsonrpc: "2.0"`、`id`、`method`、`params` 字段。
pub fn build_request(id: RequestId, method: &str, params: Option<Value>) -> LspMessage {
    LspMessage::Request(LspRequest {
        jsonrpc: "2.0".into(),
        id: Some(id),
        method: method.into(),
        params,
    })
}

/// 构建 JSON-RPC 通知消息（无 `id` 字段）
///
/// 【领域含义】创建一个 LSP 通知消息，不需要服务器响应。
/// 【核心职责】设置 `id` 为 `None`，填充方法名和参数。
pub fn build_notification(method: &str, params: Option<Value>) -> LspMessage {
    LspMessage::Request(LspRequest {
        jsonrpc: "2.0".into(),
        id: None,
        method: method.into(),
        params,
    })
}

/// 构建 JSON-RPC 响应消息
///
/// 【领域含义】创建一个 LSP 响应消息，包含请求 ID 和结果。
/// 【核心职责】填充 `jsonrpc: "2.0"`、`id`、`result` 字段，`error` 为 `None`。
pub fn build_response(id: RequestId, result: Option<Value>) -> LspMessage {
    LspMessage::Response(LspResponse {
        jsonrpc: "2.0".into(),
        id,
        result,
        error: None,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip_message_framing() {
        let msg = build_request(
            RequestId::Number(1),
            "textDocument/definition",
            Some(serde_json::json!({
                "textDocument": {"uri": "file:///test.rs"},
                "position": {"line": 10, "character": 5}
            })),
        );

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let roundtripped = read_message(&mut reader).await.unwrap();

        // Check that the roundtripped message has the same structure
        match (&msg, &roundtripped) {
            (LspMessage::Request(a), LspMessage::Request(b)) => {
                assert_eq!(a.method, b.method);
                assert_eq!(a.id, b.id);
                assert_eq!(a.params, b.params);
            }
            _ => panic!("message type mismatch"),
        }
    }

    #[tokio::test]
    async fn notification_has_no_id() {
        let notif = build_notification(
            "textDocument/didOpen",
            Some(serde_json::json!({
                "textDocument": {"uri": "file:///test.rs", "languageId": "rust", "version": 1, "text": "fn main() {}"}
            })),
        );

        let mut buf = Vec::new();
        write_message(&mut buf, &notif).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let read_back = read_message(&mut reader).await.unwrap();

        match read_back {
            LspMessage::Request(ref req) => assert!(req.id.is_none()),
            _ => panic!("expected request (notification)"),
        }
    }

    #[tokio::test]
    async fn response_roundtrip() {
        let resp = LspMessage::Response(LspResponse {
            jsonrpc: "2.0".into(),
            id: RequestId::Number(42),
            result: Some(serde_json::json!([{"uri": "file:///other.rs", "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 10}}}])),
            error: None,
        });

        let mut buf = Vec::new();
        write_message(&mut buf, &resp).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let roundtripped = read_message(&mut reader).await.unwrap();

        match roundtripped {
            LspMessage::Response(ref r) => {
                assert_eq!(r.id, RequestId::Number(42));
                assert!(r.result.is_some());
            }
            _ => panic!("expected response"),
        }
    }

    #[test]
    fn serialize_diagnostic() {
        let diag = Diagnostic {
            range: Range {
                start: Position { line: 1, character: 0 },
                end: Position { line: 1, character: 10 },
            },
            severity: Some(DiagnosticSeverity::Error),
            message: "something went wrong".into(),
            source: Some("rustc".into()),
            code: Some(serde_json::json!("E0001")),
        };

        let json = serde_json::to_value(&diag).unwrap();
        assert_eq!(json["severity"], 1);
        assert_eq!(json["message"], "something went wrong");
    }
}
