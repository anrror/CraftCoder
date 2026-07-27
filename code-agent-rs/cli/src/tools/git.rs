//! GitTool 系列 — Git 版本控制工具适配器
//!
//! 【领域含义】将 `code_agent_tools::git::GitClient` 包装为 `Tool` trait，
//! 提供 `git_status`、`git_diff`、`git_log`、`git_commit` 四个工具，覆盖
//! Agent 最常用的 Git 操作场景。
//!
//! 【核心职责】每个工具接收结构化的 JSON 参数，调用 GitClient 执行操作，
//! 返回格式化的文本结果或 JSON 结构化输出。

use async_trait::async_trait;
use code_agent_core::tools::{Tool, ToolDefinition, ToolError, tool_success, tool_error};
use code_agent_protocol::{CapabilityLevel, ToolResultMessage};
use code_agent_tools::git::GitClient;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// GitStatus — 仓库状态检查
// ---------------------------------------------------------------------------

/// `git_status` — 查看 Git 仓库当前状态
///
/// 返回当前分支、暂存/未暂存/未跟踪文件列表、前后提交计数。
pub struct GitStatusTool {
    repo_path: PathBuf,
}

impl GitStatusTool {
    pub fn new(repo_path: PathBuf) -> Self {
        Self { repo_path }
    }
}

#[async_trait]
impl Tool for GitStatusTool {
    fn name(&self) -> &str {
        "git_status"
    }

    fn description(&self) -> &str {
        "查看 Git 仓库当前状态：分支名、暂存/未暂存/未跟踪文件、前后提交数。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "仓库路径（可选，默认当前工作目录）"
                }
            }
        })
    }

    fn capability(&self) -> CapabilityLevel {
        CapabilityLevel::Read
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.repo_path.clone());

        let client = GitClient::new(&path).map_err(|e| {
            ToolError::execution_error(format!("Failed to open Git repo: {e}"))
        })?;

        match client.status() {
            Ok(status) => {
                let output = serde_json::json!({
                    "branch": status.branch,
                    "ahead": status.ahead,
                    "behind": status.behind,
                    "staged": status.staged.iter().map(|f| format!("{}  {}", f.status, f.path)).collect::<Vec<_>>(),
                    "unstaged": status.unstaged.iter().map(|f| format!("{}  {}", f.status, f.path)).collect::<Vec<_>>(),
                    "untracked": status.untracked,
                });
                Ok(tool_success(
                    "",
                    serde_json::to_string_pretty(&output)
                        .unwrap_or_else(|_| "failed to serialize".to_string()),
                ))
            }
            Err(e) => Ok(tool_error("", format!("git status failed: {e}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// GitDiff — 差异对比
// ---------------------------------------------------------------------------

/// `git_diff` — 查看工作树或暂存区的差异
///
/// 参数：
/// - `cached` (bool, 可选): 是否查看已暂存的 diff（默认 false = 未暂存）
/// - `path` (string, 可选): 仓库路径
pub struct GitDiffTool {
    repo_path: PathBuf,
}

impl GitDiffTool {
    pub fn new(repo_path: PathBuf) -> Self {
        Self { repo_path }
    }
}

#[async_trait]
impl Tool for GitDiffTool {
    fn name(&self) -> &str {
        "git_diff"
    }

    fn description(&self) -> &str {
        "查看 Git 仓库差异。默认显示未暂存的变更，设置 cached=true 查看已暂存变更。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "cached": {
                    "type": "boolean",
                    "description": "是否显示已暂存的 diff（默认 false）",
                    "default": false
                },
                "path": {
                    "type": "string",
                    "description": "仓库路径（可选，默认当前工作目录）"
                }
            }
        })
    }

    fn capability(&self) -> CapabilityLevel {
        CapabilityLevel::Read
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let cached = params.get("cached").and_then(|v| v.as_bool()).unwrap_or(false);
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.repo_path.clone());

        let client = GitClient::new(&path).map_err(|e| {
            ToolError::execution_error(format!("Failed to open Git repo: {e}"))
        })?;

        match client.diff(cached) {
            Ok(diff) => Ok(tool_success("", diff)),
            Err(e) => Ok(tool_error("", format!("git diff failed: {e}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// GitLog — 提交历史查询
// ---------------------------------------------------------------------------

/// `git_log` — 查看 Git 提交历史
///
/// 参数：
/// - `max_count` (integer, 可选): 最大返回提交数（默认 10）
/// - `path` (string, 可选): 仓库路径
pub struct GitLogTool {
    repo_path: PathBuf,
}

impl GitLogTool {
    pub fn new(repo_path: PathBuf) -> Self {
        Self { repo_path }
    }
}

#[async_trait]
impl Tool for GitLogTool {
    fn name(&self) -> &str {
        "git_log"
    }

    fn description(&self) -> &str {
        "查看 Git 提交历史。返回最近 N 条提交的哈希、作者、日期和消息。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "max_count": {
                    "type": "integer",
                    "description": "最大返回提交数（可选，默认 10）",
                    "default": 10
                },
                "path": {
                    "type": "string",
                    "description": "仓库路径（可选，默认当前工作目录）"
                }
            }
        })
    }

    fn capability(&self) -> CapabilityLevel {
        CapabilityLevel::Read
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let max_count = params
            .get("max_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.repo_path.clone());

        let client = GitClient::new(&path).map_err(|e| {
            ToolError::execution_error(format!("Failed to open Git repo: {e}"))
        })?;

        match client.log(max_count) {
            Ok(commits) => {
                let output: Vec<serde_json::Value> = commits
                    .into_iter()
                    .map(|c| {
                        serde_json::json!({
                            "hash": c.hash,
                            "author": c.author,
                            "message": c.message,
                            "timestamp": c.timestamp,
                        })
                    })
                    .collect();
                Ok(tool_success(
                    "",
                    serde_json::to_string_pretty(&output)
                        .unwrap_or_else(|_| "[]".to_string()),
                ))
            }
            Err(e) => Ok(tool_error("", format!("git log failed: {e}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// GitCommit — 暂存并提交
// ---------------------------------------------------------------------------

/// `git_commit` — 暂存文件并创建提交
///
/// 参数：
/// - `message` (string, 必需): 提交消息
/// - `files` (array of string, 可选): 要暂存的文件列表（默认暂存所有变更）
/// - `path` (string, 可选): 仓库路径
pub struct GitCommitTool {
    repo_path: PathBuf,
}

impl GitCommitTool {
    pub fn new(repo_path: PathBuf) -> Self {
        Self { repo_path }
    }
}

#[async_trait]
impl Tool for GitCommitTool {
    fn name(&self) -> &str {
        "git_commit"
    }

    fn description(&self) -> &str {
        "暂存文件并创建 Git 提交。先 add 指定文件（或所有变更），然后 commit。返回新提交的 SHA 哈希。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "提交消息"
                },
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "要暂存的文件列表（可选，默认暂存所有变更即 add -A）"
                },
                "path": {
                    "type": "string",
                    "description": "仓库路径（可选，默认当前工作目录）"
                }
            },
            "required": ["message"]
        })
    }

    fn capability(&self) -> CapabilityLevel {
        CapabilityLevel::Edit
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let message = params
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::invalid_input("missing required field 'message'"))?
            .to_string();

        let files: Vec<String> = params
            .get("files")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.repo_path.clone());

        let client = GitClient::new(&path).map_err(|e| {
            ToolError::execution_error(format!("Failed to open Git repo: {e}"))
        })?;

        // Step 1: add files (or all if none specified)
        if files.is_empty() {
            // add all (git add -A equivalent via adding "." catches everything)
            if let Err(e) = client.add(&["."]) {
                return Ok(tool_error("", format!("git add failed: {e}")));
            }
        } else {
            let refs: Vec<&str> = files.iter().map(|s| s.as_str()).collect();
            if let Err(e) = client.add(&refs) {
                return Ok(tool_error("", format!("git add failed: {e}")));
            }
        }

        // Step 2: commit
        match client.commit(&message) {
            Ok(hash) => {
                let output = serde_json::json!({
                    "hash": hash,
                    "message": message,
                });
                Ok(tool_success(
                    "",
                    serde_json::to_string_pretty(&output)
                        .unwrap_or(hash),
                ))
            }
            Err(e) => Ok(tool_error("", format!("git commit failed: {e}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// 返回所有 Git 工具的元数据定义列表
pub fn git_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "git_status".to_string(),
            description: "查看 Git 仓库当前状态：分支名、暂存/未暂存/未跟踪文件、前后提交数。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "仓库路径（可选，默认当前工作目录）"
                    }
                }
            }),
        },
        ToolDefinition {
            name: "git_diff".to_string(),
            description: "查看 Git 仓库差异。默认显示未暂存的变更。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "cached": {
                        "type": "boolean",
                        "description": "是否显示已暂存的 diff",
                        "default": false
                    },
                    "path": {
                        "type": "string",
                        "description": "仓库路径（可选，默认当前工作目录）"
                    }
                }
            }),
        },
        ToolDefinition {
            name: "git_log".to_string(),
            description: "查看 Git 提交历史。返回最近 N 条提交的哈希、作者、日期和消息。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "max_count": {
                        "type": "integer",
                        "description": "最大返回提交数",
                        "default": 10
                    },
                    "path": {
                        "type": "string",
                        "description": "仓库路径（可选，默认当前工作目录）"
                    }
                }
            }),
        },
        ToolDefinition {
            name: "git_commit".to_string(),
            description: "暂存文件并创建 Git 提交。先 add 指定文件（或所有变更），然后 commit。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "提交消息"
                    },
                    "files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "要暂存的文件列表（可选，默认 add -A）"
                    },
                    "path": {
                        "type": "string",
                        "description": "仓库路径（可选，默认当前工作目录）"
                    }
                },
                "required": ["message"]
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_status_tool_metadata() {
        let tool = GitStatusTool::new(PathBuf::from("."));
        assert_eq!(tool.name(), "git_status");
        assert_eq!(tool.capability(), CapabilityLevel::Read);
    }

    #[test]
    fn git_diff_tool_metadata() {
        let tool = GitDiffTool::new(PathBuf::from("."));
        assert_eq!(tool.name(), "git_diff");
        assert_eq!(tool.capability(), CapabilityLevel::Read);
    }

    #[test]
    fn git_log_tool_metadata() {
        let tool = GitLogTool::new(PathBuf::from("."));
        assert_eq!(tool.name(), "git_log");
        assert_eq!(tool.capability(), CapabilityLevel::Read);
    }

    #[test]
    fn git_commit_tool_metadata() {
        let tool = GitCommitTool::new(PathBuf::from("."));
        assert_eq!(tool.name(), "git_commit");
        assert_eq!(tool.capability(), CapabilityLevel::Edit);
    }

    #[test]
    fn git_tool_definitions_count() {
        let defs = git_tool_definitions();
        assert_eq!(defs.len(), 4);
        for def in &defs {
            assert!(!def.name.is_empty());
            assert!(!def.description.is_empty());
            assert!(def.input_schema.is_object());
        }
    }
}
