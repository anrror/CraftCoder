//! File system event types for the incremental file watcher.
//!
//! Provides [`FileEvent`] for representing file system changes and
//! [`FileChangeKind`] for classifying the type of change detected.

use std::path::PathBuf;

/// 文件事件（值对象）
///
/// 【领域含义】文件系统变更事件的值对象枚举，表示监听器检测到的文件创建、修改、
/// 删除或重命名操作。是"增量同步"限界上下文中的事件模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileEvent {
    /// A new file was created.
    Created(PathBuf),
    /// An existing file was modified.
    Modified(PathBuf),
    /// A file was deleted.
    Deleted(PathBuf),
    /// A file was renamed or moved. `(from, to)` paths.
    Renamed(PathBuf, PathBuf),
}

/// 文件变更类型（值对象）
///
/// 【领域含义】文件变更的分类枚举，用于决定是否需要重新索引。ContentChange 表示
/// 内容变更需要重新索引，MetadataChange 表示仅元数据变更可跳过，NoChange 表示
/// 内容哈希与上次索引一致无需操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeKind {
    /// File content changed — requires re-indexing.
    ContentChange,
    /// Only metadata changed (permissions, timestamps) — skip re-index.
    MetadataChange,
    /// Content hash is identical to the last indexed version — skip.
    NoChange,
}

impl FileEvent {
    /// 获取事件主路径
    ///
    /// 【领域含义】返回受此事件影响的主要文件路径。对于重命名事件返回目标路径。
    pub fn primary_path(&self) -> &PathBuf {
        match self {
            FileEvent::Created(path)
            | FileEvent::Modified(path)
            | FileEvent::Deleted(path) => path,
            FileEvent::Renamed(_, to) => to,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_event_created() {
        let event = FileEvent::Created(PathBuf::from("src/main.rs"));
        assert_eq!(event.primary_path(), &PathBuf::from("src/main.rs"));
        assert!(matches!(event, FileEvent::Created(_)));
    }

    #[test]
    fn test_file_event_modified() {
        let event = FileEvent::Modified(PathBuf::from("lib.py"));
        assert_eq!(event.primary_path(), &PathBuf::from("lib.py"));
        assert!(matches!(event, FileEvent::Modified(_)));
    }

    #[test]
    fn test_file_event_deleted() {
        let event = FileEvent::Deleted(PathBuf::from("old.rs"));
        assert_eq!(event.primary_path(), &PathBuf::from("old.rs"));
        assert!(matches!(event, FileEvent::Deleted(_)));
    }

    #[test]
    fn test_file_event_renamed() {
        let event = FileEvent::Renamed(
            PathBuf::from("old.rs"),
            PathBuf::from("new.rs"),
        );
        // primary_path returns the "to" path
        assert_eq!(event.primary_path(), &PathBuf::from("new.rs"));
        assert!(matches!(event, FileEvent::Renamed(_, _)));
    }

    #[test]
    fn test_file_change_kind_values() {
        assert_ne!(FileChangeKind::ContentChange, FileChangeKind::MetadataChange);
        assert_ne!(FileChangeKind::ContentChange, FileChangeKind::NoChange);
        assert_ne!(FileChangeKind::MetadataChange, FileChangeKind::NoChange);
    }
}
