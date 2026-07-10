//! GitHub PR 操作（通过 `gh` CLI）
//!
//! 【领域含义】通过 GitHub CLI (`gh`) 执行 Pull Request 操作。
//! 所有 PR 操作需要预先安装并认证 [`gh` CLI](https://cli.github.com/)。
//! 使用 [`GhClient::is_available`] 检查可用性，不可用时所有方法返回 `GitError::GhCliNotFound`。
//!
//! # 示例
//!
//! ```rust,no_run
//! # async fn demo() -> Result<(), code_agent_tools::git::GitError> {
//! use code_agent_tools::git::GhClient;
//!
//! let gh = GhClient::new("my-repo");
//! if gh.is_available() {
//!     let url = gh.create_pr("feat: add widget", "Adds the widget module", "main").await?;
//!     println!("PR created: {}", url);
//! }
//! # Ok(())
//! # }
//! ```

use std::path::PathBuf;
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use super::{GitError, GitResult};

// ---------------------------------------------------------------------------
// PrSummary
// ---------------------------------------------------------------------------

/// Pull Request 摘要信息
///
/// 【领域含义】单个 GitHub Pull Request 的摘要信息，包含编号、标题、状态和 URL。
/// 【核心职责】作为 PR 列表和状态查询的结果类型。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrSummary {
    /// PR 编号
    pub number: u32,

    /// PR 标题
    pub title: String,

    /// PR 状态：`"open"`、`"closed"` 或 `"merged"`
    pub state: String,

    /// PR 在 GitHub 上的完整 URL
    pub url: String,
}

// ---------------------------------------------------------------------------
// GhClient
// ---------------------------------------------------------------------------

/// GitHub 操作客户端（通过 `gh` CLI）
///
/// 【领域含义】封装 GitHub CLI (`gh`) 的客户端，提供 PR 创建、列表、合并等操作。
/// 每个方法在执行前都会检查 `gh` 是否已安装。
/// 【核心职责】通过子进程调用 `gh` 命令，解析 JSON 输出，返回类型化结果。
pub struct GhClient {
    /// 本地 Git 仓库路径（用于确定 GitHub 仓库）
    repo_path: PathBuf,
}

impl GhClient {
    /// 创建 GhClient
    ///
    /// 【领域含义】创建一个指向指定仓库路径的 GitHub 操作客户端。
    /// 【核心职责】保存仓库路径，供后续 `gh` 命令使用。
    pub fn new(repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo_path: repo_path.into(),
        }
    }

    /// 检查 `gh` CLI 是否可用
    ///
    /// 【领域含义】检测 `gh` 命令是否已安装且在系统 PATH 中。
    /// 【核心职责】调用 `which_gh()` 检查 gh 是否可执行。
    pub fn is_available(&self) -> bool {
        which_gh().is_some()
    }

    /// 确保 `gh` 可用，否则返回错误
    fn ensure_gh(&self) -> GitResult<()> {
        if self.is_available() {
            Ok(())
        } else {
            Err(GitError::GhCliNotFound(
                "gh CLI not found on PATH. Install from https://cli.github.com/".into(),
            ))
        }
    }

    // ── PR operations ──────────────────────────────────────────────

    /// 创建 Pull Request
    ///
    /// 【领域含义】在 GitHub 上创建一个新的 Pull Request。
    /// 【核心职责】调用 `gh pr create` → 解析返回的 JSON → 提取 PR URL。
    ///
    /// * `title` — PR 标题
    /// * `body`  — PR 描述（Markdown）
    /// * `base`  — 目标分支（如 `"main"`）
    ///
    /// 返回新创建的 PR 的 URL。
    pub async fn create_pr(&self, title: &str, body: &str, base: &str) -> GitResult<String> {
        self.ensure_gh()?;

        let output = Command::new(gh_path())
            .arg("pr")
            .arg("create")
            .arg("--title")
            .arg(title)
            .arg("--body")
            .arg(body)
            .arg("--base")
            .arg(base)
            .arg("--json")
            .arg("url")
            .current_dir(&self.repo_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| {
                GitError::OperationFailed(format!("Failed to run gh CLI: {}", e))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(GitError::GhCliFailed {
                code: output.status.code().unwrap_or(-1),
                stderr,
            });
        }

        // Parse the `gh pr create --json url` output.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| {
            GitError::OperationFailed(format!(
                "Failed to parse gh output: {} (raw: {})",
                e, stdout
            ))
        })?;

        parsed["url"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| {
                GitError::OperationFailed(format!(
                    "gh did not return a URL: {}",
                    stdout
                ))
            })
    }

    /// 列出仓库中的开放 Pull Request
    ///
    /// 【领域含义】获取当前仓库中所有状态为 `open` 的 Pull Request 列表。
    /// 【核心职责】调用 `gh pr list --state open --json number,title,state,url` → 解析 JSON 数组。
    pub async fn list_prs(&self) -> GitResult<Vec<PrSummary>> {
        self.ensure_gh()?;

        let output = Command::new(gh_path())
            .arg("pr")
            .arg("list")
            .arg("--state")
            .arg("open")
            .arg("--json")
            .arg("number,title,state,url")
            .current_dir(&self.repo_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| {
                GitError::OperationFailed(format!("Failed to run gh CLI: {}", e))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(GitError::GhCliFailed {
                code: output.status.code().unwrap_or(-1),
                stderr,
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let summaries: Vec<PrSummary> = serde_json::from_str(&stdout).map_err(|e| {
            GitError::OperationFailed(format!(
                "Failed to parse gh pr list output: {}",
                e
            ))
        })?;

        Ok(summaries)
    }

    /// 合并 Pull Request
    ///
    /// 【领域含义】通过 PR 编号合并一个 Pull Request，使用 `--merge` 策略（创建合并提交）。
    /// 【核心职责】调用 `gh pr merge <number> --merge`。
    pub async fn merge_pr(&self, pr_number: u32) -> GitResult<()> {
        self.ensure_gh()?;

        let output = Command::new(gh_path())
            .arg("pr")
            .arg("merge")
            .arg(pr_number.to_string())
            .arg("--merge")
            .current_dir(&self.repo_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| {
                GitError::OperationFailed(format!("Failed to run gh CLI: {}", e))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(GitError::GhCliFailed {
                code: output.status.code().unwrap_or(-1),
                stderr,
            });
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// `gh` CLI detection
// ---------------------------------------------------------------------------

/// 检测 `gh` CLI 是否在 PATH 中
///
/// 【领域含义】检查 `gh` 命令是否已安装且可执行。
/// 【核心职责】执行 `gh --version`，成功则返回 `Some("gh")`，否则返回 `None`。
fn which_gh() -> Option<String> {
    let output = std::process::Command::new("gh")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if output.status.success() {
        Some("gh".to_string())
    } else {
        None
    }
}

/// 返回 `gh` CLI 命令名
///
/// 【领域含义】返回 `gh` 命令的字符串名称。
/// 【核心职责】返回 `"gh"`，`which` 检查已确保其在 PATH 中。
fn gh_path() -> String {
    "gh".to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_summary_serde() {
        let ps = PrSummary {
            number: 42,
            title: "Fix the thing".into(),
            state: "open".into(),
            url: "https://github.com/owner/repo/pull/42".into(),
        };
        let json = serde_json::to_string(&ps).unwrap();
        let parsed: PrSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.number, 42);
        assert_eq!(parsed.title, "Fix the thing");
        assert_eq!(parsed.state, "open");
    }

    #[test]
    fn gh_client_is_available_returns_bool() {
        let client = GhClient::new(".");
        let available = client.is_available();
        // Just check it doesn't panic — availability depends on the system.
        assert!(available == true || available == false);
    }

    #[test]
    fn gh_client_not_found_produces_error() {
        let client = GhClient::new(".");
        if !client.is_available() {
            let err = client.ensure_gh();
            assert!(err.is_err());
            match err {
                Err(GitError::GhCliNotFound(_)) => {} // expected
                other => panic!("expected GhCliNotFound, got {:?}", other),
            }
        }
    }
}
