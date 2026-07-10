//! 工具注册与路由系统 —— 工具执行限界上下文
//!
//! 【领域含义】本模块是 Coding Agent 的工具系统核心。定义了所有工具必须
//! 实现的 `Tool` trait 接口，以及工具注册、发现、权限检查和路由的完整体系。
//!
//! 【核心领域概念】
//! - `Tool` trait: 所有工具的契约接口（名称、描述、参数 Schema、执行）
//! - `ToolRegistry`: 工具注册中心 —— 工具发现和查找的唯一真相源
//! - `ToolRouter`: 工具路由器 —— 将工具调用通过权限检查路由到执行
//! - `PermissionEnforcer`: 权限执行器 —— 按能力级别门控工具调用
//! - `ToolDefinition`: 工具元数据定义 —— 发送给模型了解可用工具
//!
//! This module provides:
//! - [`Tool`] trait: interface all tools must implement
//! - `ToolRegistry`: central registry for tool discovery
//! - `ToolRouter`: routes tool calls through permission checks to execution
//! - `PermissionEnforcer`: gatekeeps tool calls by capability level
//! - [`builtin`]: built-in file and search tools (read_file, write_file, etc.)

pub mod builtin;
pub mod permission;
pub mod registry;
pub mod router;

use async_trait::async_trait;
use code_agent_protocol::{CapabilityLevel, ToolResultMessage};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ToolDefinition – metadata about a registered tool (sent to the model)
// ---------------------------------------------------------------------------

/// 工具定义 —— 工具的静态元数据（发送给模型上下文）
///
/// 【领域含义】`ToolDefinition` 是工具的值对象描述符，包含工具名称、
/// 功能描述和 JSON Schema 参数定义。这是发送给模型上下文的格式，
/// 让模型知道有哪些工具可用以及如何调用它们。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    /// 工具的唯一名称（例如 `"read_file"`）
    pub name: String,
    /// 工具功能的可读描述
    pub description: String,
    /// 描述工具输入参数的 JSON Schema
    pub input_schema: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Tool trait
// ---------------------------------------------------------------------------

/// 工具执行接口 —— 所有工具必须实现的领域契约
///
/// 【领域含义】`Tool` trait 定义了 Coding Agent 工具的统一接口。
/// 工具是 `Send + Sync` 的，因此可以通过 `Arc` 跨线程共享。
/// `execute` 方法是异步的，支持 I/O 操作（文件访问、网络、子进程）。
///
/// 【实现规范】
/// - `name()`: 返回工具的唯一名称
/// - `description()`: 返回工具的可读描述
/// - `input_schema()`: 返回 JSON Schema 参数定义
/// - `capability()`: 返回使用此工具所需的能力级别
/// - `execute()`: 执行工具，返回 `ToolResultMessage`
#[async_trait]
pub trait Tool: Send + Sync {
    /// 返回工具的唯一名称
    fn name(&self) -> &str;

    /// 返回工具功能的可读描述
    fn description(&self) -> &str;

    /// 返回工具输入参数的 JSON Schema
    fn input_schema(&self) -> serde_json::Value;

    /// 返回使用此工具所需的能力级别
    fn capability(&self) -> CapabilityLevel;

    /// 使用给定的（已验证的）参数执行工具
    ///
    /// # Errors
    ///
    /// 执行失败返回 [`ToolError`]。错误信息将通过 `ToolResultMessage` 的
    /// `error` 字段转发给模型。
    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError>;
}

// ---------------------------------------------------------------------------
// ToolError
// ---------------------------------------------------------------------------

/// 工具操作期间可能发生的错误
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// 请求的工具名称未注册
    #[error("tool not found: {0}")]
    NotFound(String),

    /// 此名称的工具已注册
    #[error("tool already registered: {0}")]
    AlreadyRegistered(String),

    /// 调用者缺乏此工具的足够权限
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// 工具的输入参数无效或缺少必需字段
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// 工具在执行期间遇到错误
    #[error("execution error: {0}")]
    ExecutionError(String),

    /// 工具执行期间发生 I/O 错误
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Convenience constructors
// ---------------------------------------------------------------------------

impl ToolError {
    /// 为给定工具名称创建 `NotFound` 错误
    pub fn not_found(name: impl Into<String>) -> Self {
        Self::NotFound(name.into())
    }

    /// 为给定工具名称创建 `AlreadyRegistered` 错误
    pub fn already_registered(name: impl Into<String>) -> Self {
        Self::AlreadyRegistered(name.into())
    }

    /// 以给定原因创建 `PermissionDenied` 错误
    pub fn permission_denied(reason: impl Into<String>) -> Self {
        Self::PermissionDenied(reason.into())
    }

    /// 以给定原因创建 `InvalidInput` 错误
    pub fn invalid_input(reason: impl Into<String>) -> Self {
        Self::InvalidInput(reason.into())
    }

    /// 以给定消息创建 `ExecutionError` 错误
    pub fn execution_error(msg: impl Into<String>) -> Self {
        Self::ExecutionError(msg.into())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// 为成功工具执行构建 `ToolResultMessage`
pub fn tool_success(tool_call_id: &str, output: impl Into<String>) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: tool_call_id.to_owned(),
        output: Some(output.into()),
        error: None,
    }
}

/// 为失败工具执行构建 `ToolResultMessage`
pub fn tool_error(tool_call_id: &str, error: impl Into<String>) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: tool_call_id.to_owned(),
        output: None,
        error: Some(error.into()),
    }
}

/// 从 JSON Value 对象中提取必需的字符串参数。
/// 如果键缺失或不是字符串，返回 `ToolError::InvalidInput`。
pub fn require_string(params: &serde_json::Value, key: &str) -> Result<String, ToolError> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| ToolError::invalid_input(format!("missing required field '{key}'")))
}

/// 从 JSON Value 对象中提取可选的字符串参数
pub fn optional_string(params: &serde_json::Value, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str()).map(|s| s.to_owned())
}

/// 从 JSON Value 对象中提取可选的整数参数
pub fn optional_u64(params: &serde_json::Value, key: &str) -> Option<u64> {
    params.get(key).and_then(|v| v.as_u64())
}
