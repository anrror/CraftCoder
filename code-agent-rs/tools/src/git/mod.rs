//! Git 集成工具
//!
//! 【领域含义】为 AI 编码代理提供安全、可审计的 Git 操作能力。
//! 所有变更操作都经过 `SafetyPolicy` 安全检查，危险操作（`push --force`、`reset --hard`）默认禁止。
//!
//! 核心功能：
//! - 仓库状态和差异检查
//! - 暂存和提交变更
//! - 分支管理和历史导航
//! - GitHub PR 操作（通过 `gh` CLI）
//!
//! # 示例
//!
//! ```rust,no_run
//! use code_agent_tools::git::{GitClient, SafetyPolicy};
//!
//! let safety = SafetyPolicy::default();
//! let client = GitClient::with_safety("my-repo", safety).unwrap();
//!
//! let status = client.status().unwrap();
//! println!("On branch: {}", status.branch);
//!
//! let diff = client.diff(false).unwrap();
//! println!("Unstaged diff: {} lines", diff.lines().count());
//! ```

pub mod operations;
pub mod pr;
pub mod safety;

use std::path::{Path, PathBuf};

use git2::Repository;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use operations::CommitEntry;
pub use pr::{GhClient, PrSummary};
pub use safety::OperationCategory;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Git 操作错误
///
/// 【领域含义】Git 操作过程中可能出现的所有错误类型，涵盖仓库访问、安全策略、libgit2 和 gh CLI。
/// 【核心职责】统一封装非 Git 仓库、权限拒绝、操作失败、I/O 错误和 gh CLI 错误。
#[derive(Debug, Error)]
pub enum GitError {
    /// 路径不是 Git 仓库
    #[error("Not a git repository: {0}")]
    NotAGitRepo(String),

    /// 操作被安全策略阻止
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    /// libgit2 级别的操作失败
    #[error("Git operation failed: {0}")]
    OperationFailed(String),

    /// 底层 libgit2 错误
    #[error("Git2 error: {0}")]
    Git2(#[from] git2::Error),

    /// 文件系统 I/O 错误
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// `gh` CLI 不可用
    #[error("gh CLI not found: {0}")]
    GhCliNotFound(String),

    /// `gh` CLI 返回非零退出码
    #[error("gh CLI failed (exit {code}): {stderr}")]
    GhCliFailed {
        /// 退出码
        code: i32,
        /// `gh` 的标准错误输出
        stderr: String,
    },
}

/// Git 操作结果类型别名
///
/// 【领域含义】便捷类型别名，简化 Git 操作函数的返回类型签名。
/// 【核心职责】等价于 `Result<T, GitError>`。
pub type GitResult<T> = Result<T, GitError>;

// ---------------------------------------------------------------------------
// Safety policy
// ---------------------------------------------------------------------------

/// 安全策略
///
/// 【领域含义】控制 AI 代理允许执行的 Git 操作类型和输出限制。
/// 危险操作默认禁止，需要显式启用。
///
/// # 默认值
///
/// | 设置               | 默认值   |
/// |--------------------|---------|
/// | `allow_force_push` | `false` |
/// | `allow_reset_hard` | `false` |
/// | `max_diff_lines`   | `1000`  |
///
/// 【核心职责】作为所有变更操作的前置检查门禁。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SafetyPolicy {
    /// 允许 `git push --force` 和 `git push --force-with-lease`
    #[serde(default)]
    pub allow_force_push: bool,

    /// 允许 `git reset --hard`
    #[serde(default)]
    pub allow_reset_hard: bool,

    /// 单次 diff 返回的最大行数。超出部分被截断并附加截断提示。
    #[serde(default = "default_max_diff_lines")]
    pub max_diff_lines: usize,
}

const fn default_max_diff_lines() -> usize {
    1000
}

impl Default for SafetyPolicy {
    fn default() -> Self {
        Self {
            allow_force_push: false,
            allow_reset_hard: false,
            max_diff_lines: 1000,
        }
    }
}

// ---------------------------------------------------------------------------
// Status types
// ---------------------------------------------------------------------------

/// 工作树状态摘要
///
/// 【领域含义】Git 仓库当前工作树的状态快照，包含分支名、暂存/未暂存/未跟踪文件、前后提交数。
/// 【核心职责】提供仓库状态的完整视图，供 AI 代理决策使用。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusInfo {
    /// 当前分支名（或 `"HEAD (detached)"`）
    pub branch: String,

    /// 已暂存的文件（索引中的变更）
    pub staged: Vec<FileStatus>,

    /// 未暂存的文件（工作树 vs 索引的差异）
    pub unstaged: Vec<FileStatus>,

    /// 未跟踪的文件（不在索引中）
    pub untracked: Vec<String>,

    /// 领先上游跟踪分支的提交数
    pub ahead: usize,

    /// 落后上游跟踪分支的提交数
    pub behind: usize,
}

/// 单个文件的状态
///
/// 【领域含义】工作树中单个文件的变更状态。
/// 【核心职责】标识文件路径和变更类型（修改/新增/删除/重命名/未跟踪）。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileStatus {
    /// 相对于仓库根目录的路径
    pub path: String,

    /// 单字符状态码：
    /// - `'M'` — 已修改
    /// - `'A'` — 已新增（新文件已暂存）
    /// - `'D'` — 已删除
    /// - `'R'` — 已重命名
    /// - `'?'` — 未跟踪（仅出现在未暂存中）
    pub status: char,
}

// ---------------------------------------------------------------------------
// GitClient
// ---------------------------------------------------------------------------

/// 安全 Git 客户端
///
/// 【领域含义】封装 libgit2 的安全 Git 操作客户端，所有变更操作都经过 `SafetyPolicy` 检查。
/// 只读操作（status、diff、log、show）始终通过安全检查。
/// 【核心职责】提供仓库打开、状态查询、差异比较、暂存提交、分支管理等操作的统一入口。
pub struct GitClient {
    /// 仓库根目录路径
    repo_path: PathBuf,

    /// 安全策略
    safety: SafetyPolicy,

    /// 底层 libgit2 仓库句柄
    repo: Repository,
}

impl GitClient {
    // ── 构造函数 ──────────────────────────────────────────────

    /// 打开 Git 仓库（默认安全策略）
    ///
    /// 【领域含义】在指定路径打开 Git 仓库，使用默认的安全策略。
    /// 【核心职责】调用 `Repository::open` 打开仓库，失败时返回 `NotAGitRepo` 错误。
    ///
    /// # 错误
    ///
    /// 如果 `repo_path` 不是有效的 Git 仓库（或其祖先路径中不包含仓库），返回 `GitError::NotAGitRepo`。
    pub fn new(repo_path: impl Into<PathBuf>) -> GitResult<Self> {
        let repo_path: PathBuf = repo_path.into();
        let repo = Repository::open(&repo_path).map_err(|e| {
            GitError::NotAGitRepo(format!(
                "Cannot open '{}': {}",
                repo_path.display(),
                e
            ))
        })?;
        Ok(Self {
            repo_path,
            safety: SafetyPolicy::default(),
            repo,
        })
    }

    /// 打开 Git 仓库（自定义安全策略）
    ///
    /// 【领域含义】在指定路径打开 Git 仓库，使用自定义的安全策略。
    /// 【核心职责】与 `new` 类似，但允许调用者传入自定义的 `SafetyPolicy`。
    pub fn with_safety(repo_path: impl Into<PathBuf>, safety: SafetyPolicy) -> GitResult<Self> {
        let repo_path: PathBuf = repo_path.into();
        let repo = Repository::open(&repo_path).map_err(|e| {
            GitError::NotAGitRepo(format!(
                "Cannot open '{}': {}",
                repo_path.display(),
                e
            ))
        })?;
        Ok(Self {
            repo_path,
            safety,
            repo,
        })
    }

    // ── 访问器 ─────────────────────────────────────────────────

    /// 获取仓库根目录路径
    pub fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    /// 获取安全策略引用
    pub fn safety(&self) -> &SafetyPolicy {
        &self.safety
    }

    /// 获取底层 libgit2 仓库引用
    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    // ── 辅助方法 ───────────────────────────────────────────────────

    /// 截断 diff 输出到 `max_diff_lines` 行
    ///
    /// 【领域含义】当 diff 输出超过安全策略限制时，截断并附加截断提示。
    /// 【核心职责】计算行数 → 超过限制则截断 → 追加 `"... (truncated: N/M lines shown)"`。
    pub(crate) fn truncate_diff(&self, diff: String) -> String {
        let max = self.safety.max_diff_lines;
        let lines: Vec<&str> = diff.lines().collect();
        if lines.len() <= max {
            return diff;
        }
        let head = lines[..max].join("\n");
        format!(
            "{}\n... (truncated: {}/{} lines shown)",
            head,
            max,
            lines.len()
        )
    }

    /// 获取当前分支名（或 "HEAD (detached)"）
    ///
    /// 【领域含义】解析 HEAD 引用，返回当前分支名称或分离 HEAD 状态。
    /// 【核心职责】读取 HEAD → 判断是否为分支 → 返回分支名或 `"HEAD (detached)"`。
    pub(crate) fn current_branch_name(&self) -> GitResult<String> {
        // Try to resolve HEAD as a branch.
        let head = self.repo.head().map_err(|e| {
            GitError::OperationFailed(format!("Failed to read HEAD: {}", e))
        })?;
        if head.is_branch() {
            Ok(head.shorthand().unwrap_or("HEAD").to_string())
        } else {
            Ok("HEAD (detached)".to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_policy_defaults() {
        let s = SafetyPolicy::default();
        assert!(!s.allow_force_push);
        assert!(!s.allow_reset_hard);
        assert_eq!(s.max_diff_lines, 1000);
    }

    #[test]
    fn safety_policy_serde_defaults() {
        let json = r#"{}"#;
        let s: SafetyPolicy = serde_json::from_str(json).unwrap();
        assert!(!s.allow_force_push);
        assert!(!s.allow_reset_hard);
        assert_eq!(s.max_diff_lines, 1000);
    }

    #[test]
    fn safety_policy_custom() {
        let s = SafetyPolicy {
            allow_force_push: true,
            allow_reset_hard: false,
            max_diff_lines: 500,
        };
        assert!(s.allow_force_push);
        assert!(!s.allow_reset_hard);
        assert_eq!(s.max_diff_lines, 500);
    }

    #[test]
    fn git_client_not_a_repo() {
        let result = GitClient::new("E:\\non\\existent\\path\\nope");
        assert!(result.is_err());
        match result {
            Err(GitError::NotAGitRepo(_)) => {} // expected
            other => panic!("expected NotAGitRepo, got {:?}", other),
        }
    }

    #[test]
    fn diff_truncate_no_truncation() {
        let safety = SafetyPolicy {
            max_diff_lines: 100,
            ..Default::default()
        };
        let client = GitClient::with_safety(".", safety);
        // Even on an invalid repo, truncate_diff only needs the policy.
        if let Ok(client) = client {
            let short = "line1\nline2\nline3".to_string();
            let out = client.truncate_diff(short.clone());
            assert_eq!(out, short);
        }
    }

    #[test]
    fn diff_truncate_at_limit() {
        let safety = SafetyPolicy {
            max_diff_lines: 3,
            ..Default::default()
        };
        let client = GitClient::with_safety(".", safety);
        if let Ok(client) = client {
            let diff = "a\nb\nc\nd\ne\nf".to_string();
            let out = client.truncate_diff(diff);
            assert!(out.contains("truncated"));
            assert!(out.contains("3/6 lines shown"));
        }
    }

    #[test]
    fn file_status_serde() {
        let fs = FileStatus {
            path: "src/main.rs".into(),
            status: 'M',
        };
        let json = serde_json::to_string(&fs).unwrap();
        let parsed: FileStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.path, "src/main.rs");
        assert_eq!(parsed.status, 'M');
    }

    #[test]
    fn git_error_display() {
        let e = GitError::PermissionDenied("force push".into());
        assert!(e.to_string().contains("force push"));

        let e = GitError::GhCliFailed {
            code: 1,
            stderr: "not found".into(),
        };
        assert!(e.to_string().contains("1"));
        assert!(e.to_string().contains("not found"));
    }
}
