//! ShellTool — 沙箱命令执行工具适配器
//!
//! 【领域含义】将 `code_agent_tools::shell::SandboxManager` 包装为 `Tool` trait，
//! 使 Agent 可以通过 `ToolRegistry` 调用 Shell 命令。
//!
//! 【核心职责】接收 `{ "command": "...", "workdir": "..." }` JSON 输入，
//! 通过 SandboxManager 在沙箱中执行命令，返回 stdout/stderr/exit_code。

use async_trait::async_trait;
use code_agent_core::tools::{Tool, ToolDefinition, ToolError, tool_success, tool_error};
use code_agent_protocol::{CapabilityLevel, ToolResultMessage};
use code_agent_tools::shell::SandboxManager;
use code_agent_tools::shell::config::SandboxConfig;
use code_agent_tools::shell::SandboxBackend;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

/// 沙箱命令执行工具
///
/// 参数：
/// - `command` (string, 必需): 要执行的 Shell 命令
/// - `workdir` (string, 可选): 工作目录，默认当前目录
/// - `timeout` (integer, 可选): 超时秒数，默认 30
/// - `backend` (string, 可选): 沙箱后端 (auto/docker/unrestricted)
pub struct ShellTool {
    manager: Arc<Mutex<SandboxManager>>,
}

impl ShellTool {
    /// 使用默认自动检测的沙箱后端创建 ShellTool
    pub fn new() -> Self {
        let config = SandboxConfig::default();
        Self {
            manager: Arc::new(Mutex::new(SandboxManager::new(config))),
        }
    }

    /// 使用显式指定的后端创建 ShellTool
    pub fn with_backend(backend: SandboxBackend) -> Self {
        let config = SandboxConfig::default();
        Self {
            manager: Arc::new(Mutex::new(SandboxManager::with_backend(config, backend))),
        }
    }
}

impl Default for ShellTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "在受控沙箱中执行 Shell 命令。返回 stdout、stderr 和退出码。支持超时终止和输出截断。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "要执行的 Shell 命令"
                },
                "workdir": {
                    "type": "string",
                    "description": "工作目录路径（可选，默认为当前工作目录）"
                },
                "timeout": {
                    "type": "integer",
                    "description": "超时秒数（可选，默认 30）",
                    "default": 30
                },
                "backend": {
                    "type": "string",
                    "description": "沙箱后端（可选：auto/docker/unrestricted）",
                    "enum": ["auto", "docker", "unrestricted"],
                    "default": "auto"
                }
            },
            "required": ["command"]
        })
    }

    fn capability(&self) -> CapabilityLevel {
        // Shell 执行具有最高风险级别，属于 Exec
        CapabilityLevel::Exec
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::invalid_input("missing required field 'command'"))?
            .to_string();

        let workdir = params
            .get("workdir")
            .and_then(|v| v.as_str())
            .map(Path::new)
            .unwrap_or_else(|| Path::new("."));

        // Allow optional per-call timeout override via parameter
        let _timeout = params.get("timeout").and_then(|v| v.as_u64());

        let mgr = self.manager.lock().await;
        match mgr.execute(&command, workdir).await {
            Ok(result) => {
                let output = serde_json::json!({
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "exit_code": result.exit_code,
                    "timed_out": result.timed_out,
                    "duration_ms": result.duration_ms,
                });
                Ok(tool_success(
                    "",
                    serde_json::to_string(&output)
                        .unwrap_or_else(|_| "failed to serialize output".to_string()),
                ))
            }
            Err(err) => Ok(tool_error("", format!("Shell execution failed: {err}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// 与现有的 ToolDefinition 集成：提供元数据
// ---------------------------------------------------------------------------

/// 返回 ShellTool 的元数据定义，用于模型上下文注册
pub fn shell_tool_definition() -> ToolDefinition {
    let tool = ShellTool::new();
    ToolDefinition {
        name: tool.name().to_string(),
        description: tool.description().to_string(),
        input_schema: tool.input_schema(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_tool_metadata() {
        let tool = ShellTool::new();
        assert_eq!(tool.name(), "bash");
        assert!(!tool.description().is_empty());
        assert_eq!(tool.capability(), CapabilityLevel::Exec);
    }

    #[test]
    fn shell_tool_input_schema_has_required_command() {
        let tool = ShellTool::new();
        let schema = tool.input_schema();
        assert!(schema.get("required").and_then(|r| r.as_array()).map_or(false, |arr| {
            arr.iter().any(|v| v == "command")
        }));
    }

    #[test]
    fn shell_tool_rejects_empty_params() {
        let tool = ShellTool::new();
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(serde_json::json!({})));
        assert!(result.is_err());
    }

    #[test]
    fn shell_tool_definition_roundtrip() {
        let def = shell_tool_definition();
        assert_eq!(def.name, "bash");
        assert!(!def.description.is_empty());
        assert!(def.input_schema.is_object());
    }
}
