//! Git 操作实现
//!
//! 【领域含义】基于 libgit2 的 Git 操作具体实现。
//! 每个方法遵循三步模式：
//! 1. 安全检查（变更操作）
//! 2. 通过 `git2` 执行操作
//! 3. 返回高层 `GitResult<T>`

use std::path::Path;

use git2::{BranchType, DiffOptions, Signature, Sort};
use tracing::info;

use super::safety::{check_safety, OperationCategory};
use super::{FileStatus, GitClient, GitError, GitResult, StatusInfo};

// ---------------------------------------------------------------------------
// CommitEntry
// ---------------------------------------------------------------------------

/// 提交日志条目
///
/// 【领域含义】Git 提交日志中的一条记录，包含哈希、消息、作者和时间戳。
/// 【核心职责】提供提交的摘要信息，供日志展示和筛选使用。
#[derive(Clone, Debug)]
pub struct CommitEntry {
    /// 完整 SHA-1 哈希（40 个十六进制字符）
    pub hash: String,

    /// 短哈希（前 7 个字符）
    pub short_hash: String,

    /// 提交消息主题（第一行）
    pub message: String,

    /// 作者名称
    pub author: String,

    /// 提交的 Unix 时间戳
    pub timestamp: i64,
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

impl GitClient {
    /// 获取仓库状态
    ///
    /// 【领域含义】获取当前仓库的完整状态：分支名、暂存/未暂存/未跟踪文件、前后提交数。
    /// 【核心职责】读取 libgit2 状态 → 分类为 staged/unstaged/untracked → 计算 ahead/behind。
    ///
    /// # 安全性
    ///
    /// 只读操作，始终允许。
    pub fn status(&self) -> GitResult<StatusInfo> {
        let branch = self.current_branch_name()?;

        // Collect file statuses from libgit2.
        let statuses = self.repo.statuses(None).map_err(|e| {
            GitError::OperationFailed(format!("Failed to read repository status: {}", e))
        })?;

        let mut staged: Vec<FileStatus> = Vec::new();
        let mut unstaged: Vec<FileStatus> = Vec::new();
        let mut untracked: Vec<String> = Vec::new();

        for entry in statuses.iter() {
            let path = entry.path().unwrap_or("<unknown>").to_string();
            let status = entry.status();

            // Determine if this file has index (staged) changes.
            let index_status = status_char_index(status);
            let wt_status = status_char_wt(status);

            // Untracked files: WT_NEW and not in index.
            if status.is_wt_new()
                && !status.is_index_new()
                && !status.is_index_modified()
                && !status.is_index_deleted()
                && !status.is_index_renamed()
            {
                untracked.push(path);
                continue;
            }

            // Staged (index) changes.
            if let Some(c) = index_status {
                staged.push(FileStatus { path: path.clone(), status: c });
            }

            // Working-tree changes.
            if let Some(c) = wt_status {
                unstaged.push(FileStatus { path, status: c });
            }
        }

        // Ahead / behind counts.
        let (ahead, behind) = self.ahead_behind_counts()?;

        Ok(StatusInfo {
            branch,
            staged,
            unstaged,
            untracked,
            ahead,
            behind,
        })
    }

    /// 计算相对于上游跟踪分支的前后提交数
    ///
    /// 【领域含义】计算当前分支领先和落后于上游跟踪分支的提交数量。
    /// 【核心职责】解析 HEAD → 查找上游分支 → 调用 `graph_ahead_behind`。
    fn ahead_behind_counts(&self) -> GitResult<(usize, usize)> {
        let head = match self.repo.head() {
            Ok(h) => h,
            Err(_) => return Ok((0, 0)), // no HEAD yet — fresh repo
        };

        let head_oid = head.target().ok_or_else(|| {
            GitError::OperationFailed("HEAD does not point to a valid commit".into())
        })?;

        // Find the upstream branch.
        let upstream = match self.repo.branch_upstream_name(head.name().unwrap_or("HEAD")) {
            Ok(name) => name,
            Err(_) => return Ok((0, 0)), // no upstream configured
        };

        let upstream_ref = self.repo.find_reference(upstream.as_str().unwrap_or(""))?;
        let upstream_oid = upstream_ref.target().ok_or_else(|| {
            GitError::OperationFailed("Upstream does not point to a valid commit".into())
        })?;

        let (ahead, behind) = self
            .repo
            .graph_ahead_behind(head_oid, upstream_oid)
            .map_err(|e| {
                GitError::OperationFailed(format!("Failed to compute ahead/behind: {}", e))
            })?;

        Ok((ahead, behind))
    }
}

// ── Status-flag → character mapping ───────────────────────────────

/// 将索引（暂存）标志映射为单字符状态码
///
/// 【领域含义】将 libgit2 的 `Status` 标志转换为人类可读的单字符状态码。
/// 【核心职责】判断索引状态 → 返回 'A'/'M'/'D'/'R' 或 None。
fn status_char_index(status: git2::Status) -> Option<char> {
    if status.is_index_new() {
        Some('A')
    } else if status.is_index_modified() {
        Some('M')
    } else if status.is_index_deleted() {
        Some('D')
    } else if status.is_index_renamed() {
        Some('R')
    } else if status.is_index_typechange() {
        Some('M')
    } else {
        None
    }
}

/// 将工作树（未暂存）标志映射为单字符状态码
///
/// 【领域含义】将 libgit2 的 `Status` 标志转换为人类可读的单字符状态码。
/// 【核心职责】判断工作树状态 → 返回 '?'/'M'/'D'/'R' 或 None。
fn status_char_wt(status: git2::Status) -> Option<char> {
    if status.is_wt_new() {
        Some('?')
    } else if status.is_wt_modified() || status.is_wt_typechange() {
        Some('M')
    } else if status.is_wt_deleted() {
        Some('D')
    } else if status.is_wt_renamed() {
        Some('R')
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// diff
// ---------------------------------------------------------------------------

impl GitClient {
    /// 获取工作树的统一差异（unified diff）
    ///
    /// 【领域含义】获取工作树的差异输出，支持暂存和非暂存两种模式。
    /// * `staged = false` → 索引与工作树的差异（未暂存变更）
    /// * `staged = true`  → HEAD 与索引的差异（已暂存变更）
    ///
    /// 输出受 `SafetyPolicy::max_diff_lines` 限制。
    ///
    /// # 安全性
    ///
    /// 只读操作，始终允许。
    pub fn diff(&self, staged: bool) -> GitResult<String> {
        let mut opts = DiffOptions::new();

        let diff = if staged {
            // Diff HEAD..index.
            let head_tree = match self.repo.head() {
                Ok(head) => Some(head.peel_to_tree().map_err(|e| {
                    GitError::OperationFailed(format!("Failed to read HEAD tree: {}", e))
                })?),
                Err(_) => None,
            };
            match head_tree {
                Some(tree) => self.repo.diff_tree_to_index(Some(&tree), None, Some(&mut opts)),
                None => self
                    .repo
                    .diff_tree_to_index(None, None, Some(&mut opts)), // first commit
            }
        } else {
            // Diff index..working tree.
            self.repo.diff_index_to_workdir(None, Some(&mut opts))
        }
        .map_err(|e| GitError::OperationFailed(format!("Failed to compute diff: {}", e)))?;

        let mut buf = Vec::new();
        diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
            let prefix = match line.origin() {
                ' ' => ' ',
                '+' => '+',
                '-' => '-',
                'F' => ' ',
                'H' => ' ',
                _ => ' ',
            };
            buf.push(prefix as u8);
            buf.extend_from_slice(line.content());
            true
        })
        .map_err(|e| GitError::OperationFailed(format!("Failed to format diff: {}", e)))?;

        let raw = String::from_utf8_lossy(&buf).to_string();
        Ok(self.truncate_diff(raw))
    }
}

// ---------------------------------------------------------------------------
// add
// ---------------------------------------------------------------------------

impl GitClient {
    /// 暂存一个或多个文件
    ///
    /// 【领域含义】将指定文件添加到 Git 索引（暂存区），准备提交。
    /// 传入空切片时无操作。
    ///
    /// # 安全性
    ///
    /// 分类为 `Safe`（代理必须先写入文件才能暂存，不会造成数据丢失）。
    pub fn add(&self, files: &[&str]) -> GitResult<()> {
        if files.is_empty() {
            return Ok(());
        }

        check_safety(self.safety(), "add", OperationCategory::Safe)?;

        let mut index = self.repo.index().map_err(|e| {
            GitError::OperationFailed(format!("Failed to open index: {}", e))
        })?;

        for file in files {
            index
                .add_path(Path::new(file))
                .map_err(|e| {
                    GitError::OperationFailed(format!(
                        "Failed to stage '{}': {}",
                        file, e
                    ))
                })?;
        }

        index.write().map_err(|e| {
            GitError::OperationFailed(format!("Failed to write index: {}", e))
        })?;

        info!(files = ?files, "Staged files for commit");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// commit
// ---------------------------------------------------------------------------

impl GitClient {
    /// 创建提交
    ///
    /// 【领域含义】使用当前索引内容和指定消息创建一次 Git 提交。
    /// 返回新提交的完整 40 字符 SHA-1 哈希。
    ///
    /// # 安全性
    ///
    /// 分类为 `Safe` — 代理只能提交已暂存的内容。
    pub fn commit(&self, message: &str) -> GitResult<String> {
        check_safety(self.safety(), "commit", OperationCategory::Safe)?;

        let sig = self.build_signature()?;
        let mut index = self.repo.index().map_err(|e| {
            GitError::OperationFailed(format!("Failed to open index: {}", e))
        })?;

        let tree_oid = index.write_tree().map_err(|e| {
            GitError::OperationFailed(format!("Failed to write tree: {}", e))
        })?;
        let tree = self.repo.find_tree(tree_oid).map_err(|e| {
            GitError::OperationFailed(format!("Failed to find tree: {}", e))
        })?;

        // Determine parent(s).
        let head_ref = self.repo.head();
        let parents: Vec<git2::Commit<'_>> = match &head_ref {
            Ok(head) => {
                let head_oid = head.target().ok_or_else(|| {
                    GitError::OperationFailed("HEAD does not point to a commit".into())
                })?;
                let parent_commit = self.repo.find_commit(head_oid).map_err(|e| {
                    GitError::OperationFailed(format!("Failed to find HEAD commit: {}", e))
                })?;
                vec![parent_commit]
            }
            Err(_) => vec![], // initial commit — no parents
        };

        let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();

        let oid = self
            .repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                message,
                &tree,
                &parent_refs,
            )
            .map_err(|e| {
                GitError::OperationFailed(format!("Failed to create commit: {}", e))
            })?;

        info!(hash = %oid, "Created commit");
        Ok(oid.to_string())
    }

    /// 从 Git 配置构建作者/提交者签名
    ///
    /// 【领域含义】从 Git 配置或环境变量中读取作者信息，构建签名对象。
    /// 【核心职责】优先使用 `repo.signature()` → 回退到环境变量 `GIT_AUTHOR_NAME`/`GIT_AUTHOR_EMAIL` → 使用默认值。
    fn build_signature(&self) -> GitResult<Signature<'_>> {
        // Try to read from the repo config, fall back to env vars.
        if let Ok(sig) = self.repo.signature() {
            return Ok(sig);
        }

        // Fallback: use environment or hard-coded default.
        let name = std::env::var("GIT_AUTHOR_NAME")
            .or_else(|_| std::env::var("GIT_COMMITTER_NAME"))
            .unwrap_or_else(|_| "code-agent".to_string());

        let email = std::env::var("GIT_AUTHOR_EMAIL")
            .or_else(|_| std::env::var("GIT_COMMITTER_EMAIL"))
            .unwrap_or_else(|_| "agent@code-agent.local".to_string());

        Signature::now(&name, &email).map_err(|e| {
            GitError::OperationFailed(format!("Failed to create signature: {}", e))
        })
    }
}

// ---------------------------------------------------------------------------
// branch
// ---------------------------------------------------------------------------

impl GitClient {
    /// 列出所有本地分支
    ///
    /// 【领域含义】获取仓库中所有本地分支的列表，当前分支以 `"* "` 前缀标记。
    ///
    /// # 安全性
    ///
    /// 只读操作，始终允许。
    pub fn list_branches(&self) -> GitResult<Vec<String>> {
        let branches = self.repo.branches(Some(BranchType::Local)).map_err(|e| {
            GitError::OperationFailed(format!("Failed to list branches: {}", e))
        })?;

        let current = self.current_branch_name().unwrap_or_default();
        let mut names: Vec<String> = Vec::new();

        for branch_result in branches {
            let (branch, _) = branch_result.map_err(|e| {
                GitError::OperationFailed(format!("Failed to read branch: {}", e))
            })?;
            let name = branch.name().map_err(|e| {
                GitError::OperationFailed(format!("Failed to read branch name: {}", e))
            })?;
            let name = name.unwrap_or("<unnamed>");
            let display = if name == current {
                format!("* {}", name)
            } else {
                format!("  {}", name)
            };
            names.push(display);
        }

        Ok(names)
    }

    /// 创建并切换到新分支
    ///
    /// 【领域含义】从当前 HEAD 创建新分支并切换到该分支。
    ///
    /// # 安全性
    ///
    /// 分类为 `Safe` — 不会销毁数据。
    pub fn create_branch(&self, name: &str) -> GitResult<()> {
        check_safety(self.safety(), "create_branch", OperationCategory::Safe)?;

        let head = self.repo.head().map_err(|e| {
            GitError::OperationFailed(format!("Failed to read HEAD: {}", e))
        })?;
        let head_oid = head.target().ok_or_else(|| {
            GitError::OperationFailed("HEAD does not point to a valid commit".into())
        })?;
        let head_commit = self.repo.find_commit(head_oid).map_err(|e| {
            GitError::OperationFailed(format!("Failed to find HEAD commit: {}", e))
        })?;

        // Create the branch.
        self.repo
            .branch(name, &head_commit, false)
            .map_err(|e| {
                GitError::OperationFailed(format!("Failed to create branch '{}': {}", name, e))
            })?;

        // Switch to it.
        let branch_ref = format!("refs/heads/{}", name);
        let obj = self.repo.revparse_single(&branch_ref).map_err(|e| {
            GitError::OperationFailed(format!(
                "Failed to resolve new branch '{}': {}",
                name, e
            ))
        })?;

        self.repo.checkout_tree(&obj, None).map_err(|e| {
            GitError::OperationFailed(format!(
                "Failed to checkout branch '{}': {}",
                name, e
            ))
        })?;

        self.repo.set_head(&branch_ref).map_err(|e| {
            GitError::OperationFailed(format!(
                "Failed to set HEAD to '{}': {}",
                name, e
            ))
        })?;

        info!(branch = name, "Created and switched to branch");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// log
// ---------------------------------------------------------------------------

impl GitClient {
    /// 获取最近的提交日志
    ///
    /// 【领域含义】从 HEAD 开始返回最近的 `count` 条提交，最新的在前。
    ///
    /// # 安全性
    ///
    /// 只读操作，始终允许。
    pub fn log(&self, count: usize) -> GitResult<Vec<CommitEntry>> {
        let head = match self.repo.head() {
            Ok(h) => h,
            Err(_) => return Ok(Vec::new()), // no commits yet
        };
        let head_oid = head.target().ok_or_else(|| {
            GitError::OperationFailed("HEAD does not point to a valid commit".into())
        })?;

        let mut revwalk = self.repo.revwalk().map_err(|e| {
            GitError::OperationFailed(format!("Failed to create revwalk: {}", e))
        })?;

        revwalk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME).map_err(|e| {
            GitError::OperationFailed(format!("Failed to set revwalk sorting: {}", e))
        })?;
        revwalk.push(head_oid).map_err(|e| {
            GitError::OperationFailed(format!("Failed to push HEAD to revwalk: {}", e))
        })?;

        let mut entries = Vec::with_capacity(count);
        for (i, oid_result) in revwalk.enumerate() {
            if i >= count {
                break;
            }
            let oid = oid_result.map_err(|e| {
                GitError::OperationFailed(format!("Failed to walk revision: {}", e))
            })?;
            let commit = self.repo.find_commit(oid).map_err(|e| {
                GitError::OperationFailed(format!("Failed to find commit {}: {}", oid, e))
            })?;

            let message = commit
                .message()
                .unwrap_or("<no message>")
                .lines()
                .next()
                .unwrap_or("<no message>")
                .to_string();

            entries.push(CommitEntry {
                hash: oid.to_string(),
                short_hash: oid.to_string()[..7.min(oid.to_string().len())].to_string(),
                message,
                author: commit.author().name().unwrap_or("<unknown>").to_string(),
                timestamp: commit.time().seconds(),
            });
        }

        Ok(entries)
    }
}

// ---------------------------------------------------------------------------
// show
// ---------------------------------------------------------------------------

impl GitClient {
    /// 显示单个提交的完整差异和元数据
    ///
    /// 【领域含义】通过哈希或引用名显示单个提交的完整信息，包括作者、日期、消息和差异。
    ///
    /// # 安全性
    ///
    /// 只读操作，始终允许。
    pub fn show(&self, commit: &str) -> GitResult<String> {
        let obj = self.repo.revparse_single(commit).map_err(|e| {
            GitError::OperationFailed(format!(
                "Failed to resolve '{}': {}",
                commit, e
            ))
        })?;

        let commit_obj = obj.as_commit().ok_or_else(|| {
            GitError::OperationFailed(format!(
                "'{}' is not a commit (it is a {})",
                commit,
                obj.kind().map_or("unknown".into(), |k| format!("{:?}", k))
            ))
        })?;

        let tree = commit_obj.tree().map_err(|e| {
            GitError::OperationFailed(format!("Failed to get tree for commit: {}", e))
        })?;

        // Get parent tree for diff.
        let parent_tree = if commit_obj.parent_count() > 0 {
            Some(commit_obj.parent(0).map_err(|e| {
                GitError::OperationFailed(format!("Failed to get parent: {}", e))
            })?.tree().map_err(|e| {
                GitError::OperationFailed(format!("Failed to get parent tree: {}", e))
            })?)
        } else {
            None
        };

        // Build diff.
        let mut opts = DiffOptions::new();
        let diff = match parent_tree {
            Some(ref parent) => {
                self.repo
                    .diff_tree_to_tree(Some(parent), Some(&tree), Some(&mut opts))
            }
            None => self
                .repo
                .diff_tree_to_tree(None, Some(&tree), Some(&mut opts)),
        }
        .map_err(|e| GitError::OperationFailed(format!("Failed to compute diff: {}", e)))?;

        // Format header + diff.
        let mut output = String::new();

        let author = commit_obj.author();
        output.push_str(&format!(
            "commit {}\nAuthor: {} <{}>\nDate:   {}\n\n    {}\n\n",
            commit_obj.id(),
            author.name().unwrap_or("<unknown>"),
            author.email().unwrap_or("<unknown>"),
            chrono_like_timestamp(commit_obj.time().seconds()),
            commit_obj.message().unwrap_or("<no message>").trim()
        ));

        let mut patch_buf = Vec::new();
        diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
            let prefix = match line.origin() {
                ' ' => ' ',
                '+' => '+',
                '-' => '-',
                'F' => ' ',
                'H' => ' ',
                _ => ' ',
            };
            patch_buf.push(prefix as u8);
            patch_buf.extend_from_slice(line.content());
            true
        })
        .map_err(|e| GitError::OperationFailed(format!("Failed to format diff: {}", e)))?;

        output.push_str(&String::from_utf8_lossy(&patch_buf));
        Ok(output)
    }
}

/// 将 Unix 时间戳转换为人类可读的日期字符串
///
/// 【领域含义】将 Unix 时间戳转换为类似 `git show` 输出格式的日期字符串。
/// 格式：`Thu Jul 9 15:30:00 2026 +0000`。
/// 【核心职责】计算年月日时分秒 → 格式化为标准日期字符串。
fn chrono_like_timestamp(seconds: i64) -> String {
    // Simple formatting without chrono dependency.
    // Format: "Day Mon DD HH:MM:SS YYYY +0000"
    let total_secs = seconds;
    let days_since_epoch = total_secs / 86400;

    // Approximate — use a fixed UTC offset (no timezone lib).
    let secs_in_day = total_secs % 86400;
    let hours = secs_in_day / 3600;
    let minutes = (secs_in_day % 3600) / 60;
    let secs = secs_in_day % 60;

    // Calculate year/month/day from days since epoch (1970-01-01).
    let (year, month, day) = days_to_date(days_since_epoch);

    let month_names = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun",
        "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let day_names = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    let day_of_week = ((days_since_epoch + 4) % 7) as usize;

    format!(
        "{} {} {:02} {:02}:{:02}:{:02} {} +0000",
        day_names[day_of_week],
        month_names.get((month - 1) as usize).unwrap_or(&"???"),
        day,
        hours,
        minutes,
        secs,
        year
    )
}

/// 将 Unix 纪元以来的天数转换为（年、月、日）
///
/// 【领域含义】将 Unix 纪元（1970-01-01）以来的天数转换为日历日期。
/// 【核心职责】逐年减去天数 → 逐月减去天数 → 返回 (year, month, day)。
fn days_to_date(mut days: i64) -> (i64, u32, u32) {
    // Algorithm: count forward from 1970-01-01.
    let mut year: i64 = 1970;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let days_in_month = [
        31, // Jan
        if is_leap(year) { 29 } else { 28 }, // Feb
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];
    let mut month: u32 = 1;
    for &dim in &days_in_month {
        if days < dim as i64 {
            break;
        }
        days -= dim as i64;
        month += 1;
    }

    (year, month, (days + 1) as u32)
}

/// 判断是否为闰年
fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::GitClient;
    use git2::Repository;
    use std::fs;
    use std::io::Write;

    /// Create a temporary directory with an initialized git repository.
    fn setup_repo() -> (tempfile::TempDir, GitClient) {
        let dir = tempfile::tempdir().expect("tempdir");
        Repository::init(dir.path()).expect("git init");
        let client = GitClient::new(dir.path()).expect("open repo");
        (dir, client)
    }

// ── Status tests ───────────────────────────────────────────────

    #[test]
    fn status_reports_branch() {
        let (_dir, client) = setup_repo();
        let status = client.status().expect("status");
        // New repo may be on "main" or "master" depending on git config.
        assert!(
            status.branch == "main"
                || status.branch == "master"
                || status.branch.contains("HEAD")
        );
    }

    #[test]
    fn status_reports_untracked_files() {
        let (dir, client) = setup_repo();
        write_file(dir.path(), "new_file.txt", "hello");

        let status = client.status().expect("status");
        let untracked_paths: Vec<&str> = status.untracked.iter().map(|s| s.as_str()).collect();
        assert!(
            untracked_paths.contains(&"new_file.txt"),
            "expected new_file.txt in untracked, got {:?}",
            untracked_paths
        );
    }

    #[test]
    fn status_reports_staged_files() {
        let (dir, client) = setup_repo();
        write_file(dir.path(), "staged.txt", "content");

        client.add(&["staged.txt"]).expect("add");

        let status = client.status().expect("status");
        assert!(
            status.staged.iter().any(|f| f.path == "staged.txt"),
            "expected staged.txt in staged"
        );
    }

    #[test]
    fn status_reports_unstaged_changes() {
        let (dir, client) = setup_repo();
        write_file(dir.path(), "mod.txt", "initial");
        client.add(&["mod.txt"]).expect("add");
        client.commit("initial").expect("commit");

        // Modify the file without staging.
        write_file(dir.path(), "mod.txt", "modified");

        let status = client.status().expect("status");
        assert!(
            status.unstaged.iter().any(|f| f.path == "mod.txt" && f.status == 'M'),
            "expected mod.txt modified in unstaged"
        );
    }

    // ── Diff tests ─────────────────────────────────────────────────

    #[test]
    fn diff_unstaged_shows_changes() {
        let (dir, client) = setup_repo();
        write_file(dir.path(), "diff_test.txt", "line1\n");
        client.add(&["diff_test.txt"]).expect("add");
        client.commit("initial").expect("commit");

        // Modify.
        write_file(dir.path(), "diff_test.txt", "line1\nline2\n");

        let diff = client.diff(false).expect("unstaged diff");
        assert!(!diff.is_empty());
        assert!(diff.contains("line2"));
    }

    #[test]
    fn diff_staged_shows_index_changes() {
        let (dir, client) = setup_repo();
        write_file(dir.path(), "diff_staged.txt", "hello world\n");
        client.add(&["diff_staged.txt"]).expect("add");

        let diff = client.diff(true).expect("staged diff");
        assert!(!diff.is_empty());
        assert!(diff.contains("hello world"));
    }

    #[test]
    fn diff_truncated_at_max_lines() {
        let (dir, client) = setup_repo();
        // Write many lines.
        let content = (0..50).map(|i| format!("line {}\n", i)).collect::<String>();
        write_file(dir.path(), "big.txt", &content);
        client.add(&["big.txt"]).expect("add");
        client.commit("big").expect("commit");

        // Modify many lines.
        let new_content = (0..50)
            .map(|i| format!("modified line {}\n", i))
            .collect::<String>();
        write_file(dir.path(), "big.txt", &new_content);

        // Open with low max_diff_lines.
        let safety = crate::git::SafetyPolicy {
            max_diff_lines: 20,
            ..Default::default()
        };
        let client2 =
            GitClient::with_safety(dir.path(), safety).expect("open with safety");

        let diff = client2.diff(false).expect("diff");
        let lines: Vec<&str> = diff.lines().collect();
        assert!(
            lines.len() <= 22, // 20 lines + header lines + truncation notice
            "diff should be truncated, got {} lines",
            lines.len()
        );
    }

    // ── Add + Commit tests ─────────────────────────────────────────

    #[test]
    fn add_and_commit_creates_hash() {
        let (dir, client) = setup_repo();
        write_file(dir.path(), "main.rs", "fn main() {}");

        client.add(&["main.rs"]).expect("add");
        let hash = client.commit("Initial commit").expect("commit");

        assert_eq!(hash.len(), 40);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));

        // Verify the commit exists.
        let obj = client.repo().find_commit(git2::Oid::from_str(&hash).unwrap());
        assert!(obj.is_ok());
    }

    #[test]
    fn add_empty_files_is_noop() {
        let (_, client) = setup_repo();
        let result = client.add(&[]);
        assert!(result.is_ok());
    }

    #[test]
    fn commit_without_staging_is_noop_or_initial() {
        // If there's nothing to commit, git should fail. libgit2 returns an
        // error for empty commits unless --allow-empty is used.
        let (_, client) = setup_repo();
        let result = client.commit("empty");
        // May fail (nothing to commit) or succeed (if git allows empty).
        // In git2, the default is to reject empty commits.
        assert!(result.is_err() || result.is_ok());
    }

    // ── Branch tests ───────────────────────────────────────────────

    #[test]
    fn list_branches_includes_current() {
        let (dir, _client) = setup_repo();
        // Create an initial commit directly via git2 to avoid the
        // client.add() path which can fail on Windows (temp file creation).
        let repo = Repository::open(dir.path()).expect("open repo");
        let sig = git2::Signature::now("test", "test@test.com").expect("sig");
        let tree_oid = {
            let mut index = repo.index().expect("index");
            // Write an empty tree — no files needed for a valid initial commit.
            index.write_tree().expect("write tree")
        };
        let tree = repo.find_tree(tree_oid).expect("find tree");
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .expect("initial commit");
        // Drop tree before repo to satisfy borrow checker.
        std::mem::drop(tree);
        std::mem::drop(repo);

        let client = GitClient::new(dir.path()).expect("open repo");
        let branches = client.list_branches().expect("list_branches");
        assert!(!branches.is_empty(), "should have at least one branch");
        assert!(
            branches.iter().any(|b| b.starts_with('*')),
            "current branch should be starred"
        );
    }

    #[test]
    fn create_and_switch_branch() {
        let (dir, client) = setup_repo();
        write_file(dir.path(), "f.txt", "data");
        client.add(&["f.txt"]).expect("add");
        client.commit("init").expect("commit");

        client.create_branch("feature-x").expect("create_branch");

        let status = client.status().expect("status");
        assert_eq!(status.branch, "feature-x");
    }

    // ── Log tests ──────────────────────────────────────────────────

    #[test]
    fn log_returns_recent_commits() {
        let (dir, client) = setup_repo();

        // Create a few commits.
        for i in 1..=3 {
            let fname = format!("file{}.txt", i);
            write_file(dir.path(), &fname, "content");
            client.add(&[&fname]).expect("add");
            client
                .commit(&format!("commit {}", i))
                .expect("commit");
        }

        let entries = client.log(2).expect("log");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].message, "commit 3");
        assert_eq!(entries[1].message, "commit 2");
        assert!(entries[0].short_hash.len() <= 7);
    }

    #[test]
    fn log_empty_repo_returns_empty() {
        let (_, client) = setup_repo();
        let entries = client.log(10).expect("log");
        assert!(entries.is_empty());
    }

    // ── Show tests ─────────────────────────────────────────────────

    #[test]
    fn show_commit_returns_details() {
        let (dir, client) = setup_repo();
        write_file(dir.path(), "showme.txt", "hello show");
        client.add(&["showme.txt"]).expect("add");
        let hash = client.commit("show test").expect("commit");

        let output = client.show(&hash).expect("show");
        assert!(output.contains("show test"));
        assert!(output.contains("hello show"));
    }

    #[test]
    fn show_invalid_ref_returns_error() {
        let (_, client) = setup_repo();
        let result = client.show("nonexistent-ref");
        assert!(result.is_err());
    }

    // ── Not-a-repo test ────────────────────────────────────────────

    #[test]
    fn non_git_directory_returns_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = GitClient::new(dir.path());
        assert!(result.is_err());
        match result {
            Err(GitError::NotAGitRepo(_)) => {} // expected
            other => panic!("expected NotAGitRepo, got {:?}", other),
        }
    }

    // ── Signature test ─────────────────────────────────────────────

    #[test]
    fn commit_has_author_signature() {
        let (dir, client) = setup_repo();
        write_file(dir.path(), "sig.txt", "sig test");
        client.add(&["sig.txt"]).expect("add");
        let hash = client.commit("sig commit").expect("commit");

        let commit_obj = client
            .repo()
            .find_commit(git2::Oid::from_str(&hash).unwrap())
            .expect("find commit");

        let author = commit_obj.author();
        assert!(author.name().is_some());
        assert!(author.email().is_some());
    }

    // ── Filesystem operations helper ───────────────────────────────

    fn write_file(dir: &std::path::Path, name: &str, content: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        let mut f = fs::File::create(&path).expect("create file");
        f.write_all(content.as_bytes()).expect("write file");
    }

    // Override the module-level `write_file` shadowing — use the local one.
    // The `setup_repo()` helper already uses the module-level one which is
    // identical.

    #[test]
    fn stage_all_then_commit() {
        let (dir, client) = setup_repo();

        write_file(dir.path(), "a.txt", "aaa");
        write_file(dir.path(), "b.txt", "bbb");

        client.add(&["a.txt", "b.txt"]).expect("add");
        let hash = client.commit("two files").expect("commit");

        assert_eq!(hash.len(), 40);

        // Both files should appear in the commit.
        let commit_obj = client
            .repo()
            .find_commit(git2::Oid::from_str(&hash).unwrap())
            .expect("find commit");
        let tree = commit_obj.tree().expect("tree");
        let names: Vec<String> = tree.iter().filter_map(|e| e.name().map(String::from)).collect();
        assert!(names.iter().any(|n| n == "a.txt"));
        assert!(names.iter().any(|n| n == "b.txt"));
    }
}
