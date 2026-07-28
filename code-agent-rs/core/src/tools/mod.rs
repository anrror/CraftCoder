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
//! - `ToolHook`: 工具生命周期钩子（Phase F）—— 在工具执行前后触发
//! - `Plugin`: 插件接口（Phase F）—— 动态加载工具集
//! - `Command`: 命令系统（Phase F）—— 斜杠命令注册与调度
//!
//! This module provides:
//! - [`Tool`] trait: interface all tools must implement
//! - `ToolRegistry`: central registry for tool discovery
//! - `ToolRouter`: routes tool calls through permission checks to execution
//! - `PermissionEnforcer`: gatekeeps tool calls by capability level
//! - [`builtin`]: built-in file and search tools (read_file, write_file, etc.)
//! - `ToolHook` (Phase F): lifecycle hooks wrapping tool execution
//! - `Plugin` (Phase F): plugin interface for dynamic tool loading

pub mod builtin;
pub mod command;
pub mod hook;
pub mod permission;
pub mod plugin;
pub mod registry;
pub mod router;

use async_trait::async_trait;
use code_agent_protocol::{CapabilityLevel, ToolResultMessage};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ToolDefinition – metadata about a registered tool (sent to the model)
// ---------------------------------------------------------------------------

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

    /// 解析后的路径位于工作区外部 —— 路径穿越攻击已阻止（CWE-22）
    #[error("path outside workspace: {path} (workspace root: {workspace})")]
    OutsideWorkspace {
        /// 被拒绝的路径
        path: std::path::PathBuf,
        /// 配置的工作区根目录
        workspace: std::path::PathBuf,
    },
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

// ---------------------------------------------------------------------------
// Workspace boundary enforcement (CWE-22 mitigation)
// ---------------------------------------------------------------------------

// 线程局部的工作区根目录。当设置后，所有 `resolve_safe_path*` 调用
// 将验证解析后的规范路径是否位于此工作区根目录内。
//
// 【安全语义】工作区根目录在设置时即被规范化（canonicalize），
// 确保符号链接被解析，`..` 组件被展开。此后所有路径检查均基于
// 前缀匹配，拒绝任何不属于此子树的操作。
thread_local! {
    static WORKSPACE_ROOT: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// 设置用于路径穿越保护的工作区根目录
///
/// 提供的路径在存储前会被规范化（解析所有符号链接和 `..` 组件）。
/// 一旦设置，所有 `resolve_safe_path` 和 `resolve_safe_path_create`
/// 调用将要求解析后的路径位于此根目录内。
///
/// 调用时传入 `None` 可清除工作区边界检查（用于测试或无沙箱环境）。
///
/// # Errors
/// 如果工作区根目录路径无法规范化，则返回 `ToolError::Io`。
pub fn set_workspace_root(root: Option<std::path::PathBuf>) -> Result<(), ToolError> {
    let canonical = match root {
        Some(path) => Some(path.canonicalize().map_err(ToolError::Io)?),
        None => None,
    };
    WORKSPACE_ROOT.with(|cell| {
        cell.replace(canonical);
    });
    Ok(())
}

/// 返回当前配置的工作区根目录（规范化形式），如果未设置则返回 `None`
pub fn workspace_root() -> Option<std::path::PathBuf> {
    WORKSPACE_ROOT.with(|cell| cell.borrow().clone())
}

/// 检查解析后的路径是否在工作区根目录内
///
/// 如果未设置工作区根目录，此检查为无操作（允许所有路径）。
/// 如果设置了工作区根目录，则要求 `resolved` 以该目录为前缀。
fn check_workspace(resolved: &std::path::Path) -> Result<(), ToolError> {
    WORKSPACE_ROOT.with(|cell| {
        if let Some(ref root) = *cell.borrow() {
            if !resolved.starts_with(root) {
                return Err(ToolError::OutsideWorkspace {
                    path: resolved.to_path_buf(),
                    workspace: root.clone(),
                });
            }
        }
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Message builders / argument extractors
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

/// 解析并验证路径 —— 防止路径穿越攻击（C2）。
///
/// 将提供的路径规范化为绝对路径。解析所有 `..`、`.` 和符号链接，
/// 防止 `../../../etc/passwd` 类型的路径穿越漏洞。
///
/// # Arguments
/// * `path_str` - 用户提供的文件路径字符串
///
/// # Errors
/// - 如果路径不存在 ⇒ `ExecutionError`
/// - 如果 I/O 错误 ⇒ `ToolError::Io`
/// - 如果路径在工作区外 ⇒ `OutsideWorkspace`
pub fn resolve_safe_path(path_str: &str) -> Result<std::path::PathBuf, ToolError> {
    let path = std::path::Path::new(path_str);
    let resolved = path.canonicalize().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ToolError::execution_error(format!("path not found: {path_str}"))
        } else {
            ToolError::Io(e)
        }
    })?;
    check_workspace(&resolved)?;
    Ok(resolved)
}

/// 解析写入操作的路径 —— 目标文件可能不存在。
///
/// 如果文件存在，直接规范化为绝对路径。如果文件不存在，则从最近存在的
/// 祖先目录开始向上查找，将能找到的第一个存在的目录规范化，再拼接
/// 后续不存在的路径组件，防止 `../` 路径穿越。
///
/// # Arguments
/// * `path_str` - 用户提供的文件路径字符串
///
/// # Errors
/// - 如果路径中没有存在的祖先目录 ⇒ `ExecutionError`
/// - 如果 I/O 错误 ⇒ `ToolError::Io`
/// - 如果路径在工作区外 ⇒ `OutsideWorkspace`
pub fn resolve_safe_path_create(path_str: &str) -> Result<std::path::PathBuf, ToolError> {
    let path = std::path::Path::new(path_str);

    let resolved = if path.exists() {
        path.canonicalize().map_err(ToolError::Io)?
    } else {
        // Walk up the parent chain to find the first existing ancestor
        let mut components: Vec<std::ffi::OsString> = Vec::new();
        let mut current = path;
        let mut found = None;
        while let Some(parent) = current.parent() {
            if parent.as_os_str().is_empty() {
                break;
            }
            if parent.exists() {
                let canonical_parent = parent.canonicalize().map_err(ToolError::Io)?;
                let mut result = canonical_parent;
                if let Some(tail) = current.file_name() {
                    result.push(tail);
                }
                for comp in components.into_iter().rev() {
                    result.push(comp);
                }
                found = Some(result);
                break;
            }
            if let Some(component) = current.file_name() {
                components.push(component.to_os_string());
            }
            current = parent;
        }

        match found {
            Some(p) => p,
            None => {
                // Fallback: file name relative to CWD
                let cwd = std::env::current_dir().map_err(ToolError::Io)?;
                let cwd_canonical = cwd.canonicalize().map_err(ToolError::Io)?;
                if let Some(name) = path.file_name() {
                    cwd_canonical.join(name)
                } else {
                    return Err(ToolError::execution_error(format!(
                        "cannot resolve path for writing: {path_str}"
                    )));
                }
            }
        }
    };

    check_workspace(&resolved)?;
    Ok(resolved)
}

// ---------------------------------------------------------------------------
// ToolHook / ToolEvent — re-export from hook module (Phase F)
// ---------------------------------------------------------------------------

pub use hook::{ToolEvent, ToolHook, NoopHook, HookRegistry};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    /// 辅助函数：创建临时工作区并在其中设置一个文件，返回 TempDir 和文件路径
    fn setup_workspace_with_file(name: &str) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().expect("failed to create temp dir");
        let file_path = dir.path().join(name);
        let parent = file_path.parent().unwrap();
        fs::create_dir_all(parent).expect("failed to create parent dir");
        fs::File::create(&file_path)
            .and_then(|mut f| f.write_all(b"test content"))
            .expect("failed to write test file");
        set_workspace_root(Some(dir.path().to_path_buf())).expect("failed to set workspace root");
        (dir, file_path)
    }

    /// 清除工作区根目录，避免污染其他测试（线程隔离下可选，但保持卫生）
    fn clear_workspace() {
        let _ = set_workspace_root(None);
    }

    // -----------------------------------------------------------------------
    // resolve_safe_path
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_within_workspace_passes() {
        let (_dir, file_path) = setup_workspace_with_file("src/main.rs");
        let result = resolve_safe_path(&file_path.to_string_lossy());
        clear_workspace();
        assert!(result.is_ok(), "path within workspace should resolve: {result:?}");
    }

    #[test]
    fn resolve_outside_workspace_rejected() {
        let (_dir, _file_path) = setup_workspace_with_file("src/main.rs");
        // Try to resolve the system temp directory (guaranteed outside our temp workspace)
        let outside = std::env::temp_dir();
        let result = resolve_safe_path(&outside.to_string_lossy());
        clear_workspace();
        match result {
            Err(ToolError::OutsideWorkspace { .. }) => {} // expected
            other => panic!("expected OutsideWorkspace, got: {other:?}"),
        }
    }

    #[test]
    fn resolve_dotdot_traversal_rejected() {
        let (dir, _file_path) = setup_workspace_with_file("src/main.rs");

        // Create a subdirectory with a file we can reference
        let subdir = dir.path().join("sub");
        fs::create_dir_all(&subdir).expect("failed to create subdir");
        let inside_file = subdir.join("inside.txt");
        fs::File::create(&inside_file)
            .and_then(|mut f| f.write_all(b"inside"))
            .expect("failed to write inside file");

        // Try relative path with `..` to escape workspace:
        // from "sub/inside.txt", use "../../.." to reach temp dir root, then outside
        // We'll navigate to an existing directory outside the workspace
        let outside = std::env::temp_dir();
        // Build a path: <workspace>/sub/../../<outside>
        let traversal = subdir.join("..").join("..").join(&outside);
        let result = resolve_safe_path(&traversal.to_string_lossy());
        clear_workspace();
        match result {
            Err(ToolError::OutsideWorkspace { .. }) => {} // expected
            other => panic!("expected OutsideWorkspace for .. traversal, got: {other:?}"),
        }
    }

    #[test]
    fn resolve_symlink_outside_workspace_rejected() {
        let (dir, _file_path) = setup_workspace_with_file("src/main.rs");

        // Create a symlink inside workspace pointing outside
        let outside_target = std::env::temp_dir();
        let symlink_path = dir.path().join("escape_link");

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside_target, &symlink_path)
                .expect("failed to create symlink");
            let result = resolve_safe_path(&symlink_path.to_string_lossy());
            clear_workspace();
            match result {
                Err(ToolError::OutsideWorkspace { .. }) => {} // expected
                other => panic!("expected OutsideWorkspace for symlink escape, got: {other:?}"),
            }
        }
        #[cfg(windows)]
        {
            // On Windows, symlink creation requires admin or Developer Mode.
            // Attempt to create a junction or symlink; if it fails due to permissions,
            // skip the test gracefully.
            match std::os::windows::fs::symlink_dir(&outside_target, &symlink_path) {
                Ok(()) => {
                    let result = resolve_safe_path(&symlink_path.to_string_lossy());
                    clear_workspace();
                    match result {
                        Err(ToolError::OutsideWorkspace { .. }) => {} // expected
                        other => panic!(
                            "expected OutsideWorkspace for symlink escape, got: {other:?}"
                        ),
                    }
                }
                Err(e) if e.raw_os_error() == Some(1314) => {
                    // ERROR_PRIVILEGE_NOT_HELD — symlink creation not allowed
                    clear_workspace();
                    eprintln!("skipping symlink test: insufficient privileges ({e})");
                }
                Err(e) => {
                    clear_workspace();
                    panic!("unexpected error creating symlink: {e}");
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // resolve_safe_path_create
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_create_within_workspace_passes() {
        let (dir, _file_path) = setup_workspace_with_file("src/main.rs");
        let new_file = dir.path().join("src").join("new_module.rs");
        // File does not exist yet — typical write scenario
        assert!(!new_file.exists());
        let result = resolve_safe_path_create(&new_file.to_string_lossy());
        clear_workspace();
        assert!(result.is_ok(), "write path within workspace should resolve: {result:?}");
    }

    #[test]
    fn resolve_create_outside_workspace_rejected() {
        let (_dir, _file_path) = setup_workspace_with_file("src/main.rs");
        let outside = std::env::temp_dir().join("should_not_create_this.txt");
        let result = resolve_safe_path_create(&outside.to_string_lossy());
        clear_workspace();
        match result {
            Err(ToolError::OutsideWorkspace { .. }) => {} // expected
            other => panic!("expected OutsideWorkspace for write outside workspace, got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // set_workspace_root / workspace_root / no-root bypass
    // -----------------------------------------------------------------------

    #[test]
    fn without_workspace_root_all_paths_allowed() {
        // No workspace root set → boundary check is a no-op
        let result = resolve_safe_path(&std::env::temp_dir().to_string_lossy());
        // Should succeed or fail with NotFound, but NOT OutsideWorkspace
        assert!(
            !matches!(result, Err(ToolError::OutsideWorkspace { .. })),
            "without workspace root, no path should be rejected as OutsideWorkspace"
        );
    }

    #[test]
    fn workspace_root_roundtrip() {
        let dir = TempDir::new().expect("failed to create temp dir");
        let canonical = dir.path().canonicalize().expect("canonicalize failed");
        set_workspace_root(Some(dir.path().to_path_buf())).expect("set_workspace_root failed");
        let stored = workspace_root().expect("workspace_root should be Some");
        assert_eq!(stored, canonical, "stored root should match canonical form");
        // Clear
        set_workspace_root(None).expect("clear failed");
        assert!(workspace_root().is_none(), "workspace_root should be None after clear");
    }

    #[test]
    fn resolve_create_dotdot_traversal_rejected() {
        let (dir, _file_path) = setup_workspace_with_file("src/main.rs");

        let subdir = dir.path().join("sub");
        fs::create_dir_all(&subdir).expect("failed to create subdir");

        let outside = std::env::temp_dir().join("should_not_create.txt");
        // Build: workspace/sub/../../../outside_file
        let traversal = subdir.join("..").join("..").join("..").join(&outside);
        let result = resolve_safe_path_create(&traversal.to_string_lossy());
        clear_workspace();
        match result {
            Err(ToolError::OutsideWorkspace { .. }) => {} // expected
            other => panic!(
                "expected OutsideWorkspace for .. traversal in create, got: {other:?}"
            ),
        }
    }
}

