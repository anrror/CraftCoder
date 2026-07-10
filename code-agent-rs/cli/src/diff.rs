//! Diff visualization and edit acceptance.
//!
//! Provides:
//! - [`DiffHunk`] / [`DiffBlock`]: parsed unified-diff data structures
//! - [`DiffView`]: ratatui widget for inline and side-by-side rendering
//! - [`EditManager`]: tracks pending edits with accept/reject/undo via backups
//! - [`EditMode`]: auto, per-edit, per-file acceptance modes
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────┐
//! │ EditManager                              │
//! │  ├─ pending: Vec<DiffBlock>              │
//! │  ├─ accepted: Vec<AcceptedEdit>          │
//! │  ├─ backups: HashMap<Path, String>       │
//! │  └─ mode: EditMode                       │
//! ├──────────────────────────────────────────┤
//! │ DiffView (ratatui Widget)                │
//! │  ├─ render_inline(block, area, buf)      │
//! │  └─ render_side_by_side(block, area,buf) │
//! └──────────────────────────────────────────┘
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Widget},
    Frame,
};
use similar::{ChangeTag, TextDiff};

// ---------------------------------------------------------------------------
// Atomic ID generator
// ---------------------------------------------------------------------------

static NEXT_DIFF_ID: AtomicU64 = AtomicU64::new(1);

fn next_diff_id() -> u64 {
    NEXT_DIFF_ID.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Diff 块类型
///
/// 【领域含义】表示单个 diff hunk 中变更类型的领域枚举。
/// 【核心职责】区分新增、删除、修改和上下文四种行变更类型，供 DiffView 渲染使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffHunkKind {
    /// 新增 — 新文件中存在，旧文件中不存在。
    Added,
    /// 删除 — 旧文件中存在，新文件中不存在。
    Removed,
    /// 修改 — 新旧文件内容发生变化。
    Modified,
    /// 上下文 — 未变更的行，用于定位。
    Context,
}

/// Diff 块行
///
/// 【领域含义】表示 diff block 中单个变更行的值对象。
/// 【核心职责】记录行号、变更类型和前后内容，供 DiffView 渲染和 EditManager 应用。
#[derive(Debug, Clone)]
pub struct DiffHunk {
    /// 旧行号（从 1 开始），纯新增行为 0。
    pub old_line: usize,
    /// 新行号（从 1 开始），纯删除行为 0。
    pub new_line: usize,
    /// 变更类型
    pub kind: DiffHunkKind,
    /// 旧内容（新增行为空）
    pub old_content: String,
    /// 新内容（删除行为空）
    pub new_content: String,
}

/// Diff 块（单个文件）
///
/// 【领域含义】表示单个文件的完整 diff 块聚合，包含解析后的所有 hunk。
/// 【核心职责】封装文件路径、原始内容、新内容和解析后的 hunk 列表，供 EditManager 管理。
#[derive(Debug, Clone)]
pub struct DiffBlock {
    /// 唯一标识符
    pub id: u64,
    /// 文件路径 — 被变更的文件路径。
    pub file_path: PathBuf,
    /// 原始内容 — 变更前的文件内容。
    pub original_content: String,
    /// 新内容 — 变更后的文件内容。
    pub new_content: String,
    /// 解析后的 hunk 列表
    pub hunks: Vec<DiffHunk>,
    /// 是否已接受
    pub accepted: bool,
    /// 是否已拒绝
    pub rejected: bool,
}

impl DiffBlock {
    /// 解析统一 diff 字符串
    ///
    /// 【领域含义】将标准 unified diff 格式的字符串解析为 DiffBlock。
    /// 【核心职责】解析 diff 文本，提取 hunk 列表，重建原始和新内容。
    ///
    /// diff 必须为标准 unified 格式：
    /// ```diff
    /// --- a/file.txt
    /// +++ b/file.txt
    /// @@ -1,3 +1,4 @@
    ///  context
    /// -old line
    /// +new line
    /// ```
    pub fn parse(diff_text: &str, file_path: PathBuf) -> Self {
        let id = next_diff_id();
        let hunks = parse_unified_diff_manual(diff_text);

        // Reconstruct original and new content from hunks
        let original_content: String = hunks
            .iter()
            .filter(|h| h.kind != DiffHunkKind::Added)
            .map(|h| {
                if h.old_content.is_empty() && h.kind == DiffHunkKind::Context {
                    "\n".to_string()
                } else if h.old_content.is_empty() {
                    String::new()
                } else {
                    h.old_content.clone() + "\n"
                }
            })
            .collect();

        let new_content: String = hunks
            .iter()
            .filter(|h| h.kind != DiffHunkKind::Removed)
            .map(|h| {
                if h.new_content.is_empty() && h.kind == DiffHunkKind::Context {
                    h.old_content.clone() + "\n"
                } else if h.new_content.is_empty() {
                    String::new()
                } else {
                    h.new_content.clone() + "\n"
                }
            })
            .collect();

        Self {
            id,
            file_path,
            original_content,
            new_content,
            hunks,
            accepted: false,
            rejected: false,
        }
    }

    /// 从文件内容创建 DiffBlock
    ///
    /// 【领域含义】通过比较原始内容和新内容生成 DiffBlock。
    /// 【核心职责】使用 similar crate 的 TextDiff 算法计算差异，生成 hunk 列表。
    pub fn from_contents(
        file_path: PathBuf,
        original: &str,
        new: &str,
    ) -> Self {
        let id = next_diff_id();
        let diff = TextDiff::from_lines(original, new);
        let mut hunks = Vec::new();

        for change in diff.iter_all_changes() {
            let (old_line, new_line) = match change.tag() {
                ChangeTag::Equal => (change.old_index().unwrap_or(0) + 1, change.new_index().unwrap_or(0) + 1),
                ChangeTag::Delete => (change.old_index().unwrap_or(0) + 1, 0),
                ChangeTag::Insert => (0, change.new_index().unwrap_or(0) + 1),
            };

            let kind = match change.tag() {
                ChangeTag::Equal => DiffHunkKind::Context,
                ChangeTag::Delete => DiffHunkKind::Removed,
                ChangeTag::Insert => DiffHunkKind::Added,
            };

            let content = change.value().to_string();
            let content_stripped = if content.ends_with('\n') {
                content[..content.len() - 1].to_string()
            } else {
                content
            };

            let (old_content, new_content) = match change.tag() {
                ChangeTag::Equal | ChangeTag::Delete => (content_stripped.clone(), String::new()),
                ChangeTag::Insert => (String::new(), content_stripped),
            };

            hunks.push(DiffHunk {
                old_line,
                new_line,
                kind,
                old_content,
                new_content,
            });
        }

        Self {
            id,
            file_path,
            original_content: original.to_string(),
            new_content: new.to_string(),
            hunks,
            accepted: false,
            rejected: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Manual unified diff parser (fallback)
// ---------------------------------------------------------------------------

fn parse_unified_diff_manual(diff_text: &str) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();
    let mut old_line = 0usize;
    let mut new_line = 0usize;
    let mut in_hunk = false;

    for line in diff_text.lines() {
        if line.starts_with("@@") {
            // Parse hunk header: @@ -old_start,count +new_start,count @@
            in_hunk = true;
            if let Some(rest) = line.strip_prefix("@@ ") {
                if let Some((old_part, rest)) = rest.split_once(' ') {
                    if let Some((new_part, _)) = rest.split_once(" @@") {
                        old_line = parse_hunk_start(old_part).saturating_sub(1);
                        new_line = parse_hunk_start(new_part).saturating_sub(1);
                        continue;
                    }
                }
            }
            // Fallback: reset counters
            old_line = 0;
            new_line = 0;
            continue;
        }

        if !in_hunk {
            continue;
        }

        if line.is_empty() {
            continue;
        }

        let (kind, content) = match line.chars().next() {
            Some(' ') => (DiffHunkKind::Context, &line[1..]),
            Some('-') => (DiffHunkKind::Removed, &line[1..]),
            Some('+') => (DiffHunkKind::Added, &line[1..]),
            _ => continue,
        };

        match kind {
            DiffHunkKind::Context => {
                old_line += 1;
                new_line += 1;
                hunks.push(DiffHunk {
                    old_line,
                    new_line,
                    kind,
                    old_content: content.to_string(),
                    new_content: String::new(),
                });
            }
            DiffHunkKind::Removed => {
                old_line += 1;
                hunks.push(DiffHunk {
                    old_line,
                    new_line: 0,
                    kind,
                    old_content: content.to_string(),
                    new_content: String::new(),
                });
            }
            DiffHunkKind::Added => {
                new_line += 1;
                hunks.push(DiffHunk {
                    old_line: 0,
                    new_line,
                    kind,
                    old_content: String::new(),
                    new_content: content.to_string(),
                });
            }
            DiffHunkKind::Modified => {
                // Modified lines are represented as a delete + insert pair
                // in unified diff format, so this shouldn't appear directly.
                old_line += 1;
                new_line += 1;
                hunks.push(DiffHunk {
                    old_line,
                    new_line,
                    kind,
                    old_content: content.to_string(),
                    new_content: content.to_string(),
                });
            }
        }
    }

    hunks
}

/// Parse a hunk start like "-3" or "-3,4" into the starting line number.
fn parse_hunk_start(s: &str) -> usize {
    let s = if s.starts_with('-') || s.starts_with('+') {
        &s[1..]
    } else {
        s
    };
    if let Some((num, _)) = s.split_once(',') {
        num.parse().unwrap_or(1)
    } else {
        s.parse().unwrap_or(1)
    }
}

// ---------------------------------------------------------------------------
// Edit mode
// ---------------------------------------------------------------------------

/// 编辑模式
///
/// 【领域含义】表示编辑接受策略的领域枚举，控制 diff 的审批流程。
/// 【核心职责】区分自动接受、逐 hunk 审批和逐文件审批三种模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditMode {
    /// 自动 — 自动接受所有编辑。
    Auto,
    /// 逐编辑 — 逐个 hunk 接受/拒绝。
    #[default]
    PerEdit,
    /// 逐文件 — 一次性接受/拒绝整个文件。
    PerFile,
}

// ---------------------------------------------------------------------------
// EditManager
// ---------------------------------------------------------------------------

/// 已接受的编辑记录
///
/// 【领域含义】表示已接受编辑的审计记录，支持撤销操作。
/// 【核心职责】记录被接受的 DiffBlock 和接受时间戳，供 undo 功能使用。
#[derive(Debug, Clone)]
pub struct AcceptedEdit {
    /// 被接受的 diff block
    pub block: DiffBlock,
    /// 接受时间戳（单调递增计数器）
    pub accepted_at: u64,
}

/// 编辑管理器
///
/// 【领域含义】管理待处理编辑的领域服务，支持接受/拒绝/撤销操作，通过文件备份实现安全回滚。
/// 【核心职责】维护待处理 diff 列表、已接受编辑历史和文件备份，提供完整的编辑生命周期管理。
///
/// # 示例
///
/// ```rust,ignore
/// let mut mgr = EditManager::new(EditMode::PerEdit);
/// let block = DiffBlock::parse(diff_text, "src/main.rs".into());
/// mgr.add_pending(block);
/// mgr.accept(block_id)?;
/// mgr.undo()?;
/// ```
#[derive(Debug)]
pub struct EditManager {
    /// 待处理的 diff block 列表
    pending: Vec<DiffBlock>,
    /// 已接受编辑的历史记录（用于撤销）
    history: Vec<AcceptedEdit>,
    /// 文件备份 — 路径 → 编辑前的原始内容
    backups: HashMap<PathBuf, String>,
    /// 当前编辑模式
    mode: EditMode,
    /// 单调递增计数器（用于排序）
    counter: u64,
}

impl EditManager {
    /// 创建编辑管理器
    ///
    /// 【领域含义】构造 EditManager 实例，指定编辑模式。
    /// 【核心职责】初始化待处理列表、历史记录和备份映射。
    pub fn new(mode: EditMode) -> Self {
        Self {
            pending: Vec::new(),
            history: Vec::new(),
            backups: HashMap::new(),
            mode,
            counter: 0,
        }
    }

    /// 获取当前编辑模式
    ///
    /// 【领域含义】返回当前编辑管理器使用的编辑模式。
    /// 【核心职责】供外部查询当前审批策略。
    pub fn mode(&self) -> EditMode {
        self.mode
    }

    /// 设置编辑模式
    ///
    /// 【领域含义】设置编辑管理器的审批策略。
    /// 【核心职责】允许运行时切换编辑模式。
    pub fn set_mode(&mut self, mode: EditMode) {
        self.mode = mode;
    }

    /// 添加待处理的 diff block
    ///
    /// 【领域含义】将一个新的 diff block 加入待处理列表。
    /// 【核心职责】自动创建文件备份（如尚未备份），将 block 加入待处理队列。
    pub fn add_pending(&mut self, block: DiffBlock) {
        // Create a backup of the original file if we haven't already
        self.backups
            .entry(block.file_path.clone())
            .or_insert_with(|| block.original_content.clone());

        self.pending.push(block);
    }

    /// 获取所有待处理的 diff block
    ///
    /// 【领域含义】返回当前所有待处理的 diff block 切片。
    /// 【核心职责】供外部遍历和展示待处理编辑。
    pub fn pending(&self) -> &[DiffBlock] {
        &self.pending
    }

    /// 按 ID 获取待处理的 block
    ///
    /// 【领域含义】根据唯一标识符查找待处理的 diff block。
    /// 【核心职责】供外部按 ID 操作特定 block。
    pub fn get_pending(&self, id: u64) -> Option<&DiffBlock> {
        self.pending.iter().find(|b| b.id == id)
    }

    /// 按 ID 获取可变引用的待处理 block
    ///
    /// 【领域含义】根据唯一标识符获取可变引用的待处理 diff block。
    /// 【核心职责】供外部修改特定 block 的状态。
    pub fn get_pending_mut(&mut self, id: u64) -> Option<&mut DiffBlock> {
        self.pending.iter_mut().find(|b| b.id == id)
    }

    /// 接受编辑并应用到磁盘
    ///
    /// 【领域含义】接受一个待处理的 diff block，将变更写入磁盘文件。
    /// 【核心职责】从待处理列表移除 block，将新内容写入文件，记录到历史。
    /// 如果 block 未找到或文件写入失败则返回错误。
    pub fn accept(&mut self, id: u64) -> Result<(), EditError> {
        let idx = self
            .pending
            .iter()
            .position(|b| b.id == id)
            .ok_or(EditError::NotFound(id))?;

        let mut block = self.pending.remove(idx);
        block.accepted = true;

        // Write the new content to disk
        std::fs::write(&block.file_path, &block.new_content)
            .map_err(|e| EditError::Io(block.file_path.clone(), e))?;

        self.counter += 1;
        self.history.push(AcceptedEdit {
            accepted_at: self.counter,
            block,
        });

        Ok(())
    }

    /// 接受指定文件的所有待处理编辑
    ///
    /// 【领域含义】接受指定文件路径的所有待处理 diff block。
    /// 【核心职责】批量接受同一文件的所有编辑。
    pub fn accept_file(&mut self, file_path: &Path) -> Result<usize, EditError> {
        let ids: Vec<u64> = self
            .pending
            .iter()
            .filter(|b| b.file_path == file_path)
            .map(|b| b.id)
            .collect();

        if ids.is_empty() {
            return Err(EditError::NoPendingForFile(file_path.to_path_buf()));
        }

        let mut count = 0;
        for id in ids {
            self.accept(id)?;
            count += 1;
        }

        Ok(count)
    }

    /// 接受所有待处理编辑
    ///
    /// 【领域含义】接受所有待处理的 diff block。
    /// 【核心职责】批量接受全部编辑，返回接受数量。
    pub fn accept_all(&mut self) -> Result<usize, EditError> {
        let ids: Vec<u64> = self.pending.iter().map(|b| b.id).collect();
        let mut count = 0;
        for id in ids {
            self.accept(id)?;
            count += 1;
        }
        Ok(count)
    }

    /// 拒绝编辑
    ///
    /// 【领域含义】拒绝一个待处理的 diff block，不应用变更。
    /// 【核心职责】从待处理列表移除 block，如无其他 block 引用该文件则清理备份。
    pub fn reject(&mut self, id: u64) -> Result<(), EditError> {
        let idx = self
            .pending
            .iter()
            .position(|b| b.id == id)
            .ok_or(EditError::NotFound(id))?;

        let mut block = self.pending.remove(idx);
        block.rejected = true;

        // If no more pending blocks reference this file, remove the backup
        let file_path = block.file_path.clone();
        if !self.pending.iter().any(|b| b.file_path == file_path) {
            self.backups.remove(&file_path);
        }

        Ok(())
    }

    /// 拒绝指定文件的所有待处理编辑
    ///
    /// 【领域含义】拒绝指定文件路径的所有待处理 diff block。
    /// 【核心职责】批量拒绝同一文件的所有编辑。
    pub fn reject_file(&mut self, file_path: &Path) -> Result<usize, EditError> {
        let ids: Vec<u64> = self
            .pending
            .iter()
            .filter(|b| b.file_path == file_path)
            .map(|b| b.id)
            .collect();

        if ids.is_empty() {
            return Err(EditError::NoPendingForFile(file_path.to_path_buf()));
        }

        let mut count = 0;
        for id in ids {
            self.reject(id)?;
            count += 1;
        }

        Ok(count)
    }

    /// 拒绝所有待处理编辑
    ///
    /// 【领域含义】拒绝所有待处理的 diff block。
    /// 【核心职责】清空待处理列表和备份。
    pub fn reject_all(&mut self) -> Result<usize, EditError> {
        let count = self.pending.len();
        self.pending.clear();
        self.backups.clear();
        Ok(count)
    }

    /// 撤销最近一次接受的编辑
    ///
    /// 【领域含义】通过恢复备份文件撤销最近一次接受的编辑。
    /// 【核心职责】从历史记录弹出最近条目，从备份恢复原始文件内容。
    /// 返回撤销的编辑数（0 或 1）。
    pub fn undo(&mut self) -> Result<usize, EditError> {
        let Some(accepted) = self.history.pop() else {
            return Ok(0);
        };

        let file_path = accepted.block.file_path.clone();

        // Restore from backup if available
        if let Some(original) = self.backups.get(&file_path) {
            std::fs::write(&file_path, original)
                .map_err(|e| EditError::Io(file_path.clone(), e))?;
        }

        Ok(1)
    }

    /// 撤销最近 N 次接受的编辑
    ///
    /// 【领域含义】批量撤销最近 N 次接受的编辑。
    /// 【核心职责】循环调用 undo，返回实际撤销的编辑数。
    pub fn undo_n(&mut self, n: usize) -> Result<usize, EditError> {
        let mut count = 0;
        for _ in 0..n {
            if self.undo()? == 0 {
                break;
            }
            count += 1;
        }
        Ok(count)
    }

    /// 获取待处理 block 数量
    ///
    /// 【领域含义】返回当前待处理的 diff block 数量。
    /// 【核心职责】供外部查询待处理编辑数量。
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// 获取已接受编辑数量
    ///
    /// 【领域含义】返回已接受编辑的历史记录数量。
    /// 【核心职责】供外部查询可撤销的编辑数量。
    pub fn history_count(&self) -> usize {
        self.history.len()
    }

    /// 检查是否有待处理编辑
    ///
    /// 【领域含义】判断当前是否存在待处理的 diff block。
    /// 【核心职责】供外部快速判断是否需要处理编辑。
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// 获取有待处理编辑的文件路径列表
    ///
    /// 【领域含义】返回所有有待处理编辑的文件路径（去重排序）。
    /// 【核心职责】供外部展示哪些文件有待处理的变更。
    pub fn pending_files(&self) -> Vec<&Path> {
        let mut files: Vec<&Path> = self.pending.iter().map(|b| b.file_path.as_path()).collect();
        files.sort();
        files.dedup();
        files
    }

    /// 获取文件备份内容
    ///
    /// 【领域含义】返回指定文件路径的备份原始内容。
    /// 【核心职责】供外部查看或恢复文件备份。
    pub fn get_backup(&self, path: &Path) -> Option<&str> {
        self.backups.get(path).map(|s| s.as_str())
    }
}

// ---------------------------------------------------------------------------
// EditError
// ---------------------------------------------------------------------------

/// 编辑错误
///
/// 【领域含义】表示编辑管理过程中可能发生的领域错误。
/// 【核心职责】封装 block 未找到、文件无待处理编辑、I/O 错误等异常场景。
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    /// 未找到指定 ID 的待处理 block
    #[error("no pending diff block with id {0}")]
    NotFound(u64),

    /// 指定文件无待处理编辑
    #[error("no pending edits for file: {0}")]
    NoPendingForFile(PathBuf),

    /// 应用编辑时发生 I/O 错误
    #[error("I/O error writing {0}: {1}")]
    Io(PathBuf, std::io::Error),
}

// ---------------------------------------------------------------------------
// DiffView — ratatui widget
// ---------------------------------------------------------------------------

/// Diff 渲染模式
///
/// 【领域含义】表示 diff 视图的渲染方式枚举。
/// 【核心职责】区分内联视图和并排视图两种渲染模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffRenderMode {
    /// 内联视图 — 新增/删除在同一列中显示。
    #[default]
    Inline,
    /// 并排视图 — 旧内容和新内容分两列显示。
    SideBySide,
}

/// Diff 视图组件
///
/// 【领域含义】基于 ratatui 的 diff block 渲染组件，提供内联和并排两种渲染方式。
/// 【核心职责】将 DiffBlock 渲染为终端 UI 中的可视化 diff 视图。
///
/// # 示例
///
/// ```rust,ignore
/// let block = DiffBlock::parse(diff_text, "src/main.rs".into());
/// let view = DiffView::new(&block, DiffRenderMode::Inline);
/// frame.render_widget(view, area);
/// ```
pub struct DiffView<'a> {
    /// 要渲染的 diff block
    block: &'a DiffBlock,
    /// 渲染模式
    mode: DiffRenderMode,
    /// 是否显示行号
    show_line_numbers: bool,
    /// 是否高亮上下文行
    show_context: bool,
    /// 内容区域最大宽度
    max_width: Option<u16>,
}

impl<'a> DiffView<'a> {
    /// 创建 DiffView
    ///
    /// 【领域含义】为指定的 DiffBlock 创建渲染视图。
    /// 【核心职责】初始化视图组件，默认使用内联模式、显示行号和上下文。
    pub fn new(block: &'a DiffBlock) -> Self {
        Self {
            block,
            mode: DiffRenderMode::default(),
            show_line_numbers: true,
            show_context: true,
            max_width: None,
        }
    }

    /// 设置渲染模式
    ///
    /// 【领域含义】设置 diff 视图的渲染方式。
    /// 【核心职责】切换内联/并排渲染模式。
    pub fn mode(mut self, mode: DiffRenderMode) -> Self {
        self.mode = mode;
        self
    }

    /// 设置是否显示行号
    ///
    /// 【领域含义】控制 diff 视图中行号的显示。
    /// 【核心职责】启用或禁用行号显示。
    pub fn show_line_numbers(mut self, show: bool) -> Self {
        self.show_line_numbers = show;
        self
    }

    /// 设置是否显示上下文行
    ///
    /// 【领域含义】控制 diff 视图中上下文行的显示。
    /// 【核心职责】启用或禁用上下文行高亮。
    pub fn show_context(mut self, show: bool) -> Self {
        self.show_context = show;
        self
    }

    /// 设置最大内容宽度
    ///
    /// 【领域含义】设置 diff 视图内容区域的最大宽度。
    /// 【核心职责】限制渲染宽度，防止内容溢出。
    pub fn max_width(mut self, width: u16) -> Self {
        self.max_width = Some(width);
        self
    }

    /// 渲染 diff block
    ///
    /// 【领域含义】将 diff block 渲染到指定的终端区域。
    /// 【核心职责】根据当前渲染模式（内联/并排）执行渲染。
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        match self.mode {
            DiffRenderMode::Inline => self.render_inline(frame, area),
            DiffRenderMode::SideBySide => self.render_side_by_side(frame, area),
        }
    }

    /// Render in inline mode.
    fn render_inline(&self, frame: &mut Frame, area: Rect) {
        let lines = self.build_inline_lines();
        let _max_w = self.max_width.unwrap_or(area.width.saturating_sub(2));

        let text = Text::from(lines);
        let paragraph = Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", self.block.file_path.display())),
        );

        frame.render_widget(paragraph, area);
    }

    /// Render in side-by-side mode.
    fn render_side_by_side(&self, frame: &mut Frame, area: Rect) {
        let (left_lines, right_lines) = self.build_side_by_side_lines();

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);

        let left_text = Text::from(left_lines);
        let left_para = Paragraph::new(left_text).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" old "),
        );

        let right_text = Text::from(right_lines);
        let right_para = Paragraph::new(right_text).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" new "),
        );

        frame.render_widget(left_para, chunks[0]);
        frame.render_widget(right_para, chunks[1]);
    }

    /// Build lines for inline rendering.
    fn build_inline_lines(&self) -> Vec<Line<'a>> {
        let mut lines: Vec<Line<'a>> = Vec::new();
        let _ln_width = if self.show_line_numbers { 5 } else { 0 };

        // File header
        lines.push(
            Line::from(Span::styled(
                format!("--- {}", self.block.file_path.display()),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
        );
        lines.push(
            Line::from(Span::styled(
                format!("+++ {}", self.block.file_path.display()),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
        );

        for hunk in &self.block.hunks {
            let (prefix, style) = match hunk.kind {
                DiffHunkKind::Added => ("+", Style::default().fg(Color::Green)),
                DiffHunkKind::Removed => ("-", Style::default().fg(Color::Red)),
                DiffHunkKind::Modified => ("~", Style::default().fg(Color::Yellow)),
                DiffHunkKind::Context => (" ", Style::default()),
            };

            let line_num_str = if self.show_line_numbers {
                match hunk.kind {
                    DiffHunkKind::Added => format!("{:>4} ", hunk.new_line),
                    DiffHunkKind::Removed => format!("{:>4} ", hunk.old_line),
                    DiffHunkKind::Modified | DiffHunkKind::Context => {
                        format!("{:>4} ", hunk.old_line)
                    }
                }
            } else {
                String::new()
            };

            let content = match hunk.kind {
                DiffHunkKind::Added => &hunk.new_content,
                DiffHunkKind::Removed => &hunk.old_content,
                DiffHunkKind::Modified => &hunk.new_content,
                DiffHunkKind::Context => &hunk.old_content,
            };

            let line_str = format!("{}{}{}", line_num_str, prefix, content);
            lines.push(Line::from(Span::styled(line_str, style)));
        }

        lines
    }

    /// Build lines for side-by-side rendering.
    fn build_side_by_side_lines(&self) -> (Vec<Line<'a>>, Vec<Line<'a>>) {
        let mut left_lines: Vec<Line<'a>> = Vec::new();
        let mut right_lines: Vec<Line<'a>> = Vec::new();

        for hunk in &self.block.hunks {
            match hunk.kind {
                DiffHunkKind::Context => {
                    let ln = if self.show_line_numbers {
                        format!("{:>4} ", hunk.old_line)
                    } else {
                        String::new()
                    };
                    let line = format!(" {} {}", ln, hunk.old_content);
                    let style = Style::default();
                    left_lines.push(Line::from(Span::styled(line.clone(), style)));
                    right_lines.push(Line::from(Span::styled(line, style)));
                }
                DiffHunkKind::Removed => {
                    let ln = if self.show_line_numbers {
                        format!("{:>4} ", hunk.old_line)
                    } else {
                        String::new()
                    };
                    let line = format!("-{} {}", ln, hunk.old_content);
                    let style = Style::default().fg(Color::Red);
                    left_lines.push(Line::from(Span::styled(line.clone(), style)));
                    right_lines.push(Line::from(Span::styled(line, style)));
                }
                DiffHunkKind::Added => {
                    let ln = if self.show_line_numbers {
                        format!("{:>4} ", hunk.new_line)
                    } else {
                        String::new()
                    };
                    let line = format!("+{} {}", ln, hunk.new_content);
                    let style = Style::default().fg(Color::Green);
                    left_lines.push(Line::from(Span::styled(String::new(), style)));
                    right_lines.push(Line::from(Span::styled(line, style)));
                }
                DiffHunkKind::Modified => {
                    let old_ln = if self.show_line_numbers {
                        format!("{:>4} ", hunk.old_line)
                    } else {
                        String::new()
                    };
                    let new_ln = if self.show_line_numbers {
                        format!("{:>4} ", hunk.new_line)
                    } else {
                        String::new()
                    };
                    let old_line = format!("~{} {}", old_ln, hunk.old_content);
                    let new_line = format!("~{} {}", new_ln, hunk.new_content);
                    let style = Style::default().fg(Color::Yellow);
                    left_lines.push(Line::from(Span::styled(old_line, style)));
                    right_lines.push(Line::from(Span::styled(new_line, style)));
                }
            }
        }

        (left_lines, right_lines)
    }
}

impl Widget for DiffView<'_> {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let lines = self.build_inline_lines();
        let text = Text::from(lines);
        let paragraph = Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", self.block.file_path.display())),
        );
        paragraph.render(area, buf);
    }
}

// ---------------------------------------------------------------------------
// Convenience functions
// ---------------------------------------------------------------------------

/// 解析统一 diff 字符串为 DiffBlock 列表
///
/// 【领域含义】将标准 unified diff 格式的字符串解析为多个 DiffBlock。
/// 【核心职责】通过 `---` / `+++` 对分割多文件 diff，逐个解析为 DiffBlock。
pub fn parse_diff(diff_text: &str) -> Vec<DiffBlock> {
    let mut blocks = Vec::new();
    let mut current_file: Option<PathBuf> = None;
    let mut current_diff = String::new();

    for line in diff_text.lines() {
        if line.starts_with("--- ") {
            // If we were building a previous diff, finalize it
            if let Some(ref path) = current_file {
                if !current_diff.is_empty() {
                    blocks.push(DiffBlock::parse(&current_diff, path.clone()));
                }
            }
            // Extract the file path from "--- a/path" or "--- path"
            let path_str = line.trim_start_matches("--- ").trim();
            let path_str = path_str.strip_prefix("a/").unwrap_or(path_str);
            current_file = Some(PathBuf::from(path_str));
            current_diff = line.to_string() + "\n";
        } else if line.starts_with("+++ ") {
            current_diff.push_str(line);
            current_diff.push('\n');
        } else if let Some(ref _path) = current_file {
            current_diff.push_str(line);
            current_diff.push('\n');
        }
    }

    // Finalize the last block
    if let Some(ref path) = current_file {
        if !current_diff.is_empty() {
            blocks.push(DiffBlock::parse(&current_diff, path.clone()));
        }
    }

    blocks
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Sample diffs ─────────────────────────────────────────────────────

    const SIMPLE_DIFF: &str = "\
--- a/hello.txt
+++ b/hello.txt
@@ -1,3 +1,4 @@
 hello
-world
+earth
+universe
 goodbye
";

    const MULTI_HUNK_DIFF: &str = "\
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,5 +1,6 @@
 fn main() {
     println!(\"Hello\");
+    println!(\"World\");
-    println!(\"Goodbye\");
+    println!(\"Farewell\");
     println!(\"End\");
 }
";

    const MULTI_FILE_DIFF: &str = "\
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,4 @@
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
 }
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,2 +1,3 @@
 pub fn greet() {
+    println!(\"hello\");
 }
";

    const EMPTY_DIFF: &str = "";

    // ── DiffBlock tests ──────────────────────────────────────────────────

    #[test]
    fn parse_simple_diff() {
        let block = DiffBlock::parse(SIMPLE_DIFF, "hello.txt".into());
        assert_eq!(block.file_path.to_str().unwrap(), "hello.txt");
        assert!(!block.hunks.is_empty());
        assert!(block.hunks.iter().any(|h| h.kind == DiffHunkKind::Added));
        assert!(block.hunks.iter().any(|h| h.kind == DiffHunkKind::Removed));
        assert!(block.hunks.iter().any(|h| h.kind == DiffHunkKind::Context));
    }

    #[test]
    fn parse_multi_hunk_diff() {
        let block = DiffBlock::parse(MULTI_HUNK_DIFF, "src/main.rs".into());
        assert_eq!(block.file_path.to_str().unwrap(), "src/main.rs");
        assert!(!block.hunks.is_empty());

        // Count by kind
        let added = block.hunks.iter().filter(|h| h.kind == DiffHunkKind::Added).count();
        let removed = block.hunks.iter().filter(|h| h.kind == DiffHunkKind::Removed).count();
        let context = block.hunks.iter().filter(|h| h.kind == DiffHunkKind::Context).count();

        assert!(added >= 1, "expected at least 1 addition, got {added}");
        assert!(removed >= 1, "expected at least 1 deletion, got {removed}");
        assert!(context >= 1, "expected at least 1 context line, got {context}");
    }

    #[test]
    fn parse_empty_diff() {
        let block = DiffBlock::parse(EMPTY_DIFF, "empty.txt".into());
        assert!(block.hunks.is_empty());
    }

    #[test]
    fn from_contents_creates_diff() {
        let original = "line1\nline2\nline3\n";
        let new = "line1\nline2_modified\nline3\nline4\n";
        let block = DiffBlock::from_contents("test.txt".into(), original, new);

        assert_eq!(block.file_path.to_str().unwrap(), "test.txt");
        assert!(!block.hunks.is_empty());

        let added = block.hunks.iter().filter(|h| h.kind == DiffHunkKind::Added).count();
        let removed = block.hunks.iter().filter(|h| h.kind == DiffHunkKind::Removed).count();

        // "line2" → "line2_modified" is a delete+insert pair
        // "line4" is an insert
        assert!(added >= 1);
        assert!(removed >= 1);
    }

    #[test]
    fn diff_block_has_unique_ids() {
        let b1 = DiffBlock::parse(SIMPLE_DIFF, "a.txt".into());
        let b2 = DiffBlock::parse(SIMPLE_DIFF, "b.txt".into());
        assert_ne!(b1.id, b2.id);
    }

    // ── parse_diff (multi-file) tests ────────────────────────────────────

    #[test]
    fn parse_multi_file_diff() {
        let blocks = parse_diff(MULTI_FILE_DIFF);
        assert_eq!(blocks.len(), 2, "expected 2 blocks, got {}", blocks.len());

        assert!(blocks[0].file_path.to_str().unwrap().contains("main.rs"));
        assert!(blocks[1].file_path.to_str().unwrap().contains("lib.rs"));
    }

    #[test]
    fn parse_empty_diff_returns_empty() {
        let blocks = parse_diff("");
        assert!(blocks.is_empty());
    }

    // ── EditManager tests ────────────────────────────────────────────────

    #[test]
    fn edit_manager_add_pending() {
        let mut mgr = EditManager::new(EditMode::PerEdit);
        let block = DiffBlock::parse(SIMPLE_DIFF, "hello.txt".into());
        mgr.add_pending(block);

        assert_eq!(mgr.pending_count(), 1);
        assert!(mgr.has_pending());
    }

    #[test]
    fn edit_manager_accept_and_reject() {
        let mut mgr = EditManager::new(EditMode::PerEdit);
        let block = DiffBlock::parse(SIMPLE_DIFF, "hello.txt".into());
        let id = block.id;
        mgr.add_pending(block);

        // Reject
        mgr.reject(id).unwrap();
        assert_eq!(mgr.pending_count(), 0);
    }

    #[test]
    fn edit_manager_accept_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "old content\n").unwrap();

        let mut mgr = EditManager::new(EditMode::PerEdit);
        let block = DiffBlock::from_contents(file_path.clone(), "old content\n", "new content\n");
        let id = block.id;
        mgr.add_pending(block);

        mgr.accept(id).unwrap();
        assert_eq!(mgr.pending_count(), 0);
        assert_eq!(mgr.history_count(), 1);

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "new content\n");
    }

    #[test]
    fn edit_manager_accept_all() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        std::fs::write(&f1, "old a\n").unwrap();
        std::fs::write(&f2, "old b\n").unwrap();

        let mut mgr = EditManager::new(EditMode::Auto);
        mgr.add_pending(DiffBlock::from_contents(f1.clone(), "old a\n", "new a\n"));
        mgr.add_pending(DiffBlock::from_contents(f2.clone(), "old b\n", "new b\n"));

        mgr.accept_all().unwrap();
        assert_eq!(mgr.pending_count(), 0);
        assert_eq!(mgr.history_count(), 2);
        assert_eq!(std::fs::read_to_string(&f1).unwrap(), "new a\n");
        assert_eq!(std::fs::read_to_string(&f2).unwrap(), "new b\n");
    }

    #[test]
    fn edit_manager_reject_all() {
        let mut mgr = EditManager::new(EditMode::PerEdit);
        mgr.add_pending(DiffBlock::parse(SIMPLE_DIFF, "a.txt".into()));
        mgr.add_pending(DiffBlock::parse(SIMPLE_DIFF, "b.txt".into()));

        let count = mgr.reject_all().unwrap();
        assert_eq!(count, 2);
        assert_eq!(mgr.pending_count(), 0);
    }

    #[test]
    fn edit_manager_undo_restores_backup() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "original content\n").unwrap();

        let mut mgr = EditManager::new(EditMode::PerEdit);
        let block = DiffBlock::from_contents(file_path.clone(), "original content\n", "modified content\n");
        let id = block.id;
        mgr.add_pending(block);

        mgr.accept(id).unwrap();
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "modified content\n");

        mgr.undo().unwrap();
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "original content\n");
    }

    #[test]
    fn edit_manager_undo_n() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        std::fs::write(&f1, "a\n").unwrap();
        std::fs::write(&f2, "b\n").unwrap();

        let mut mgr = EditManager::new(EditMode::Auto);
        mgr.add_pending(DiffBlock::from_contents(f1.clone(), "a\n", "a2\n"));
        mgr.add_pending(DiffBlock::from_contents(f2.clone(), "b\n", "b2\n"));
        mgr.accept_all().unwrap();

        assert_eq!(mgr.history_count(), 2);
        mgr.undo_n(1).unwrap();
        assert_eq!(mgr.history_count(), 1);
        // undo_n(1) undoes the last accepted edit (b.txt → b)
        assert_eq!(std::fs::read_to_string(&f1).unwrap(), "a2\n");
        assert_eq!(std::fs::read_to_string(&f2).unwrap(), "b\n");
    }

    #[test]
    fn edit_manager_undo_empty_history() {
        let mut mgr = EditManager::new(EditMode::PerEdit);
        assert_eq!(mgr.undo().unwrap(), 0);
    }

    #[test]
    fn edit_manager_reject_not_found() {
        let mut mgr = EditManager::new(EditMode::PerEdit);
        let err = mgr.reject(99999).unwrap_err();
        assert!(matches!(err, EditError::NotFound(99999)));
    }

    #[test]
    fn edit_manager_accept_not_found() {
        let mut mgr = EditManager::new(EditMode::PerEdit);
        let err = mgr.accept(99999).unwrap_err();
        assert!(matches!(err, EditError::NotFound(99999)));
    }

    #[test]
    fn edit_manager_pending_files() {
        let mut mgr = EditManager::new(EditMode::PerEdit);
        mgr.add_pending(DiffBlock::parse(SIMPLE_DIFF, "a.txt".into()));
        mgr.add_pending(DiffBlock::parse(SIMPLE_DIFF, "b.txt".into()));

        let files = mgr.pending_files();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn edit_manager_accept_file() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        std::fs::write(&f1, "old a\n").unwrap();
        std::fs::write(&f2, "old b\n").unwrap();

        let mut mgr = EditManager::new(EditMode::PerFile);
        mgr.add_pending(DiffBlock::from_contents(f1.clone(), "old a\n", "new a\n"));
        mgr.add_pending(DiffBlock::from_contents(f2.clone(), "old b\n", "new b\n"));

        mgr.accept_file(&f1).unwrap();
        assert_eq!(mgr.pending_count(), 1);
        assert_eq!(std::fs::read_to_string(&f1).unwrap(), "new a\n");
        assert_eq!(std::fs::read_to_string(&f2).unwrap(), "old b\n");
    }

    #[test]
    fn edit_manager_reject_file() {
        let mut mgr = EditManager::new(EditMode::PerFile);
        mgr.add_pending(DiffBlock::parse(SIMPLE_DIFF, "a.txt".into()));
        mgr.add_pending(DiffBlock::parse(SIMPLE_DIFF, "b.txt".into()));

        mgr.reject_file(Path::new("a.txt")).unwrap();
        assert_eq!(mgr.pending_count(), 1);
    }

    #[test]
    fn edit_manager_get_pending() {
        let mut mgr = EditManager::new(EditMode::PerEdit);
        let block = DiffBlock::parse(SIMPLE_DIFF, "test.txt".into());
        let id = block.id;
        mgr.add_pending(block);

        assert!(mgr.get_pending(id).is_some());
        assert!(mgr.get_pending(99999).is_none());
    }

    #[test]
    fn edit_manager_mode_default() {
        let mgr = EditManager::new(EditMode::PerEdit);
        assert_eq!(mgr.mode(), EditMode::PerEdit);
    }

    #[test]
    fn edit_manager_set_mode() {
        let mut mgr = EditManager::new(EditMode::PerEdit);
        mgr.set_mode(EditMode::Auto);
        assert_eq!(mgr.mode(), EditMode::Auto);
    }

    #[test]
    fn edit_manager_backup_preserved() {
        let mut mgr = EditManager::new(EditMode::PerEdit);
        let block = DiffBlock::parse(SIMPLE_DIFF, "test.txt".into());
        mgr.add_pending(block);

        assert!(mgr.get_backup(Path::new("test.txt")).is_some());
    }

    // ── DiffView tests ───────────────────────────────────────────────────

    #[test]
    fn diff_view_creates_inline_lines() {
        let block = DiffBlock::parse(SIMPLE_DIFF, "hello.txt".into());
        let view = DiffView::new(&block);
        let lines = view.build_inline_lines();

        assert!(!lines.is_empty(), "expected at least one line");
        // First line should be the file header
        assert!(lines[0].to_string().contains("hello.txt"));
    }

    #[test]
    fn diff_view_side_by_side() {
        let block = DiffBlock::parse(SIMPLE_DIFF, "hello.txt".into());
        let view = DiffView::new(&block);
        let (left, right) = view.build_side_by_side_lines();

        assert!(!left.is_empty());
        assert!(!right.is_empty());
        assert_eq!(left.len(), right.len());
    }

    #[test]
    fn diff_view_no_line_numbers() {
        let block = DiffBlock::parse(SIMPLE_DIFF, "hello.txt".into());
        let view = DiffView::new(&block).show_line_numbers(false);
        let lines = view.build_inline_lines();

        assert!(!lines.is_empty());
    }

    #[test]
    fn diff_view_empty_block() {
        let block = DiffBlock::parse("", "empty.txt".into());
        let view = DiffView::new(&block);
        let lines = view.build_inline_lines();

        // Should still have the file header lines
        assert_eq!(lines.len(), 2);
    }

    // ── EditMode tests ───────────────────────────────────────────────────

    #[test]
    fn edit_mode_default_is_per_edit() {
        assert_eq!(EditMode::default(), EditMode::PerEdit);
    }

    #[test]
    fn edit_mode_debug_and_clone() {
        let modes = [EditMode::Auto, EditMode::PerEdit, EditMode::PerFile];
        for mode in modes {
            let cloned = mode;
            assert_eq!(format!("{:?}", mode), format!("{:?}", cloned));
        }
    }

    // ── Manual parser fallback tests ──────────────────────────────────────

    #[test]
    fn manual_parser_handles_simple_diff() {
        let hunks = parse_unified_diff_manual(SIMPLE_DIFF);
        assert!(!hunks.is_empty(), "manual parser should produce hunks");

        let added = hunks.iter().filter(|h| h.kind == DiffHunkKind::Added).count();
        let removed = hunks.iter().filter(|h| h.kind == DiffHunkKind::Removed).count();
        assert!(added >= 1);
        assert!(removed >= 1);
    }

    #[test]
    fn manual_parser_handles_empty() {
        let hunks = parse_unified_diff_manual("");
        assert!(hunks.is_empty());
    }

    #[test]
    fn parse_hunk_start_various_formats() {
        assert_eq!(parse_hunk_start("-3"), 3);
        assert_eq!(parse_hunk_start("+1"), 1);
        assert_eq!(parse_hunk_start("-3,4"), 3);
        assert_eq!(parse_hunk_start("+10,2"), 10);
        assert_eq!(parse_hunk_start("0"), 0);
        assert_eq!(parse_hunk_start("abc"), 1); // fallback
    }

    // ── Integration: EditManager + file system ───────────────────────────

    #[test]
    fn integration_accept_reject_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("cycle.txt");
        std::fs::write(&file_path, "line1\nline2\nline3\n").unwrap();

        let mut mgr = EditManager::new(EditMode::PerEdit);

        // Add a diff that changes line2
        let block = DiffBlock::from_contents(
            file_path.clone(),
            "line1\nline2\nline3\n",
            "line1\nline2_modified\nline3\n",
        );
        let id = block.id;
        mgr.add_pending(block);

        // Accept
        mgr.accept(id).unwrap();
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "line1\nline2_modified\nline3\n");

        // Undo
        mgr.undo().unwrap();
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "line1\nline2\nline3\n");
    }

    #[test]
    fn integration_multi_file_accept() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("a.rs");
        let f2 = dir.path().join("b.rs");
        std::fs::write(&f1, "fn a() {}\n").unwrap();
        std::fs::write(&f2, "fn b() {}\n").unwrap();

        let mut mgr = EditManager::new(EditMode::Auto);
        mgr.add_pending(DiffBlock::from_contents(f1.clone(), "fn a() {}\n", "fn a_updated() {}\n"));
        mgr.add_pending(DiffBlock::from_contents(f2.clone(), "fn b() {}\n", "fn b_updated() {}\n"));

        mgr.accept_all().unwrap();
        assert_eq!(std::fs::read_to_string(&f1).unwrap(), "fn a_updated() {}\n");
        assert_eq!(std::fs::read_to_string(&f2).unwrap(), "fn b_updated() {}\n");

        mgr.undo_n(2).unwrap();
        assert_eq!(std::fs::read_to_string(&f1).unwrap(), "fn a() {}\n");
        assert_eq!(std::fs::read_to_string(&f2).unwrap(), "fn b() {}\n");
    }
}
