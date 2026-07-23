//! Index synchronization that handles file changes incrementally.
//!
//! [`IndexSync`] coordinates between the [`CodeIndexer`] and [`GraphBuilder`]
//! to efficiently update the symbol index and call/dependency graphs when
//! source files change.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use blake3::Hasher;
use log::{debug, warn};

use crate::graph::builder::GraphBuilder;
use crate::graph::GraphError;
use crate::indexer::{CodeIndexer, IndexerError};

use super::event::FileEvent;

/// 监听器同步错误
///
/// 【领域含义】文件监听器和索引同步操作中可能出现的错误类型，涵盖 I/O 错误、
/// 索引器错误、图谱错误、notify 错误和路径非文件错误。属于"增量同步"限界上下文的
/// 异常模型。
#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Indexer error: {0}")]
    Indexer(#[from] IndexerError),

    #[error("Graph error: {0}")]
    Graph(#[from] GraphError),

    #[error("Notify error: {0}")]
    Notify(#[from] notify::Error),

    #[error("Path is not a file: {0}")]
    NotAFile(String),
}

/// 同步动作（值对象）
///
/// 【领域含义】同步过程中对单个文件执行的动作枚举。Indexed 表示首次索引或创建后索引，
/// Patched 表示内容变更后增量更新，Removed 表示从索引中删除，Skipped 表示内容未变跳过。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncAction {
    /// File was fully indexed (first time or after creation).
    Indexed,
    /// File was incrementally patched (content changed).
    Patched,
    /// File was removed from the index.
    Removed,
    /// No change — identical content hash.
    Skipped,
}

/// 同步结果（值对象）
///
/// 【领域含义】处理单个文件变更事件的结果值对象，包含处理的文件路径、执行的动作、
/// 新增/删除的符号数、更新的图谱边数和操作耗时。用于监控和调试增量同步过程。
#[derive(Debug, Clone)]
pub struct SyncResult {
    /// The file that was processed.
    pub file: PathBuf,
    /// The action taken.
    pub action: SyncAction,
    /// Number of symbols added during this operation.
    pub symbols_added: usize,
    /// Number of symbols removed during this operation.
    pub symbols_removed: usize,
    /// Number of graph edges updated.
    pub edges_updated: usize,
    /// Wall-clock duration of the operation in milliseconds.
    pub duration_ms: u64,
}

/// 索引同步器（领域服务）
///
/// 【领域含义】协调索引器和图谱构建器处理增量文件变更的领域服务。维护内容哈希映射
/// 以在文件内容未变化时跳过冗余重新索引（如 touch 或仅元数据保存）。
/// 是"增量同步"限界上下文的核心领域服务。
pub struct IndexSync {
    /// The code indexer for parsing files and storing symbols.
    indexer: CodeIndexer,
    /// The graph builder for constructing call/dependency graphs.
    /// Uses its own internal indexer for reading from the same DB.
    graph_builder: GraphBuilder,
    /// BLAKE3 content hashes for deduplication. Key is canonical path.
    content_hashes: HashMap<PathBuf, String>,
}

impl IndexSync {
    /// 创建索引同步器
    ///
    /// 【领域含义】创建 IndexSync 实例。indexer 和 graph_builder 应指向同一个
    /// SQLite 数据库，以确保图谱查询能看到最新的索引状态。
    pub fn new(indexer: CodeIndexer, graph_builder: GraphBuilder) -> Self {
        Self {
            indexer,
            graph_builder,
            content_hashes: HashMap::new(),
        }
    }

    /// 处理单个文件变更事件
    ///
    /// 【领域含义】处理单个文件变更事件。基于内容哈希比较判断是否需要重新索引。
    /// 如果内容发生变化，调用索引器和图谱构建器进行增量更新。
    pub fn handle_event(&mut self, event: FileEvent) -> Result<SyncResult, WatcherError> {
        let start = Instant::now();

        match event {
            FileEvent::Created(path) => self.handle_created(&path, start),
            FileEvent::Modified(path) => self.handle_modified(&path, start),
            FileEvent::Deleted(path) => self.handle_deleted(&path, start),
            FileEvent::Renamed(from, to) => self.handle_renamed(&from, &to, start),
        }
    }

    /// 计算文件 BLAKE3 哈希
    ///
    /// 【领域含义】计算文件内容的 BLAKE3 哈希值。将文件读入内存后使用 BLAKE3 哈希，
    /// 用于内容去重判断。
    pub fn file_hash(&self, path: &Path) -> Result<String, WatcherError> {
        let content = std::fs::read(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                WatcherError::NotAFile(path.display().to_string())
            } else {
                WatcherError::Io(e)
            }
        })?;

        let mut hasher = Hasher::new();
        hasher.update(&content);
        let hash = hasher.finalize();
        Ok(hash.to_hex().to_string())
    }

    /// 批量处理多个事件
    ///
    /// 【领域含义】批量处理多个文件变更事件，返回每个事件的处理结果。
    /// 单个事件处理失败不会中断批次处理。
    pub fn handle_batch(&mut self, events: Vec<FileEvent>) -> Result<Vec<SyncResult>, WatcherError> {
        let mut results = Vec::with_capacity(events.len());
        for event in events {
            match self.handle_event(event) {
                Ok(result) => results.push(result),
                Err(e) => {
                    warn!("Error handling batch event: {}", e);
                    // Continue processing remaining events
                }
            }
        }
        Ok(results)
    }

    /// Get a reference to the stored content hashes (for testing).
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn content_hashes(&self) -> &HashMap<PathBuf, String> {
        &self.content_hashes
    }

    // ── private handlers ────────────────────────────────────────────────

    fn handle_created(&mut self, path: &Path, start: Instant) -> Result<SyncResult, WatcherError> {
        let canonical = canonicalize_path(path);

        // Compute hash and store it
        let hash = match self.file_hash(path) {
            Ok(h) => h,
            Err(WatcherError::NotAFile(_)) => {
                return Ok(SyncResult {
                    file: canonical,
                    action: SyncAction::Skipped,
                    symbols_added: 0,
                    symbols_removed: 0,
                    edges_updated: 0,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
            Err(e) => return Err(e),
        };

        // Index the new file
        let _prev_count = self.indexer.symbol_count().unwrap_or(0);
        let symbols = self.indexer.index_file(path)?;

        let symbols_added = symbols.len();

        // Rebuild graphs to include new file's edges
        let edges_updated = self.rebuild_graphs()?;

        self.content_hashes.insert(canonical.clone(), hash);

        let action = if symbols_added > 0 {
            SyncAction::Indexed
        } else {
            SyncAction::Skipped
        };

        debug!("Created: {} ({} symbols)", canonical.display(), symbols_added);

        Ok(SyncResult {
            file: canonical,
            action,
            symbols_added,
            symbols_removed: 0,
            edges_updated,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }

    fn handle_modified(&mut self, path: &Path, start: Instant) -> Result<SyncResult, WatcherError> {
        let canonical = canonicalize_path(path);

        // Compute new hash
        let new_hash = match self.file_hash(path) {
            Ok(h) => h,
            Err(WatcherError::NotAFile(_)) => {
                return Ok(SyncResult {
                    file: canonical,
                    action: SyncAction::Skipped,
                    symbols_added: 0,
                    symbols_removed: 0,
                    edges_updated: 0,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
            Err(e) => return Err(e),
        };

        // Check if content actually changed
        if let Some(old_hash) = self.content_hashes.get(&canonical) {
            if *old_hash == new_hash {
                debug!(
                    "Skipped (no content change): {}",
                    canonical.display()
                );
                return Ok(SyncResult {
                    file: canonical,
                    action: SyncAction::Skipped,
                    symbols_added: 0,
                    symbols_removed: 0,
                    edges_updated: 0,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
        }

        // Count previous symbols for this file
        let file_path_str = canonical.display().to_string();
        let prev_symbols = self
            .indexer
            .get_file_symbols(&file_path_str)
            .unwrap_or_default();
        let prev_count = prev_symbols.len();

        // Re-index the file (this replaces symbols atomically)
        let new_symbols = self.indexer.index_file(path)?;
        let new_count = new_symbols.len();

        // Determine action based on whether symbols were actually extracted
        let action = if new_count == 0 && prev_count == 0 {
            SyncAction::Skipped
        } else if new_count > 0 || prev_count > 0 {
            SyncAction::Patched
        } else {
            SyncAction::Skipped
        };

        // Rebuild graphs
        let edges_updated = self.rebuild_graphs()?;

        self.content_hashes.insert(canonical.clone(), new_hash);

        debug!(
            "Patched: {} ({}→{} symbols)",
            canonical.display(),
            prev_count,
            new_count
        );

        Ok(SyncResult {
            file: canonical,
            action,
            symbols_added: new_count.saturating_sub(prev_count),
            symbols_removed: prev_count.saturating_sub(new_count),
            edges_updated,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }

    fn handle_deleted(&mut self, path: &Path, start: Instant) -> Result<SyncResult, WatcherError> {
        let canonical = canonicalize_path(path);
        let file_path_str = canonical.display().to_string();

        // Count symbols before removal
        let prev_symbols = self
            .indexer
            .get_file_symbols(&file_path_str)
            .unwrap_or_default();
        let removed_count = prev_symbols.len();

        // Remove from index
        if removed_count > 0 {
            self.indexer.remove_file(path)?;
        }

        self.content_hashes.remove(&canonical);

        // Rebuild graphs to remove stale edges
        let edges_updated = self.rebuild_graphs()?;

        debug!("Removed: {} ({} symbols)", canonical.display(), removed_count);

        Ok(SyncResult {
            file: canonical,
            action: SyncAction::Removed,
            symbols_added: 0,
            symbols_removed: removed_count,
            edges_updated,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }

    fn handle_renamed(
        &mut self,
        from: &Path,
        to: &Path,
        start: Instant,
    ) -> Result<SyncResult, WatcherError> {
        let canonical_to = canonicalize_path(to);
        let canonical_from = canonicalize_path(from);

        // Remove old path
        let from_str = canonical_from.display().to_string();
        let prev_symbols = self
            .indexer
            .get_file_symbols(&from_str)
            .unwrap_or_default();
        let removed_count = prev_symbols.len();
        if removed_count > 0 {
            self.indexer.remove_file(from)?;
        }
        self.content_hashes.remove(&canonical_from);

        // Compute hash for new path
        let hash = match self.file_hash(to) {
            Ok(h) => h,
            Err(WatcherError::NotAFile(_)) => {
                return Ok(SyncResult {
                    file: canonical_to,
                    action: SyncAction::Removed,
                    symbols_added: 0,
                    symbols_removed: removed_count,
                    edges_updated: 0,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
            Err(e) => return Err(e),
        };

        // Index at new path
        let new_symbols = self.indexer.index_file(to)?;
        let added_count = new_symbols.len();

        // Rebuild graphs
        let edges_updated = self.rebuild_graphs()?;

        self.content_hashes.insert(canonical_to.clone(), hash);

        Ok(SyncResult {
            file: canonical_to,
            action: if added_count > 0 {
                SyncAction::Indexed
            } else {
                SyncAction::Skipped
            },
            symbols_added: added_count,
            symbols_removed: removed_count,
            edges_updated,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }

    /// Rebuild both call and dependency graphs.
    /// Returns the total number of edges across both graphs.
    fn rebuild_graphs(&mut self) -> Result<usize, WatcherError> {
        let call_graph = self.graph_builder.build_call_graph()?;
        let dep_graph = self.graph_builder.build_dependency_graph()?;
        Ok(call_graph.edge_count() + dep_graph.import_count())
    }
}

/// Canonicalize a path, falling back to the original on error.
fn canonicalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_index_sync(dir: &TempDir, db_name: &str) -> (IndexSync, PathBuf) {
        let db_path = dir.path().join(db_name);
        let db_str = db_path.display().to_string();

        // IndexSync's own indexer (for direct index/remove operations)
        let indexer = CodeIndexer::new(&db_str).unwrap();
        // GraphBuilder's indexer (for reading from DB during graph construction)
        let graph_indexer = CodeIndexer::new(&db_str).unwrap();
        let graph_builder = GraphBuilder::new(graph_indexer);

        let sync = IndexSync::new(indexer, graph_builder);
        (sync, db_path)
    }

    fn write_file(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    // ── Content hash dedup ─────────────────────────────────────────────

    #[test]
    fn test_content_hash_dedup_same_content_skipped() {
        let dir = TempDir::new().unwrap();
        let py_path = write_file(&dir, "lib.py", "def foo():\n    pass\n");
        let (mut sync, _db) = setup_index_sync(&dir, "dedup.db");

        // First event: Created — should index
        let result = sync
            .handle_event(FileEvent::Created(py_path.clone()))
            .unwrap();
        assert_eq!(result.action, SyncAction::Indexed);
        assert!(result.symbols_added > 0);

        // Second event: Modified same content — should skip (hash match)
        let result = sync
            .handle_event(FileEvent::Modified(py_path.clone()))
            .unwrap();
        assert_eq!(
            result.action,
            SyncAction::Skipped,
            "Same content should be skipped"
        );
        assert_eq!(result.symbols_added, 0);
        assert_eq!(result.symbols_removed, 0);
    }

    #[test]
    fn test_content_change_patched() {
        let dir = TempDir::new().unwrap();
        let py_path = write_file(&dir, "lib.py", "def old_func():\n    pass\n");
        let (mut sync, _db) = setup_index_sync(&dir, "patch.db");

        // Index initial content
        let result = sync
            .handle_event(FileEvent::Created(py_path.clone()))
            .unwrap();
        assert!(result.symbols_added > 0);

        // Modify the file with different content
        std::fs::write(&py_path, "def new_func():\n    pass\n\ndef other():\n    pass\n").unwrap();

        let result = sync
            .handle_event(FileEvent::Modified(py_path.clone()))
            .unwrap();
        assert_eq!(
            result.action,
            SyncAction::Patched,
            "Different content should be patched"
        );
        // We went from 1 → 2 symbols
        assert!(result.symbols_added > 0 || result.symbols_removed > 0);
    }

    #[test]
    fn test_file_creation_indexed() {
        let dir = TempDir::new().unwrap();
        let py_path = write_file(
            &dir,
            "module.py",
            "def hello():\n    pass\n\nclass Greeter:\n    pass\n",
        );
        let (mut sync, _db) = setup_index_sync(&dir, "create.db");

        let result = sync
            .handle_event(FileEvent::Created(py_path.clone()))
            .unwrap();

        assert_eq!(result.action, SyncAction::Indexed);
        assert_eq!(result.symbols_added, 2); // hello + Greeter
        assert_eq!(result.symbols_removed, 0);

        // Verify symbols are in the indexer
        let file_str = py_path.canonicalize().unwrap();
        let symbols = sync
            .indexer
            .get_file_symbols(&file_str.display().to_string())
            .unwrap();
        let names: Vec<String> = symbols.iter().map(|s| s.symbol_name.clone()).collect();
        assert!(names.contains(&"hello".to_string()));
        assert!(names.contains(&"Greeter".to_string()));
    }

    #[test]
    fn test_file_deletion_removed() {
        let dir = TempDir::new().unwrap();
        let py_path = write_file(&dir, "temp.py", "def temp_func():\n    pass\n");
        let (mut sync, _db) = setup_index_sync(&dir, "delete.db");

        // Index first
        sync.handle_event(FileEvent::Created(py_path.clone()))
            .unwrap();

        // Delete
        let result = sync
            .handle_event(FileEvent::Deleted(py_path.clone()))
            .unwrap();

        assert_eq!(result.action, SyncAction::Removed);
        assert_eq!(result.symbols_removed, 1);

        // Verify removed from index
        let file_str = py_path.canonicalize().unwrap();
        let symbols = sync
            .indexer
            .get_file_symbols(&file_str.display().to_string())
            .unwrap();
        assert!(symbols.is_empty());
    }

    #[test]
    fn test_file_rename_handled() {
        let dir = TempDir::new().unwrap();
        let old_path = write_file(&dir, "old.py", "def old_func():\n    pass\n");
        let new_path = dir.path().join("new.py");

        // Copy to simulate rename (watcher gives both paths)
        std::fs::copy(&old_path, &new_path).unwrap();

        let (mut sync, _db) = setup_index_sync(&dir, "rename.db");

        // Index old path first
        sync.handle_event(FileEvent::Created(old_path.clone()))
            .unwrap();

        // Rename: from old to new
        let result = sync
            .handle_event(FileEvent::Renamed(old_path.clone(), new_path.clone()))
            .unwrap();

        assert_eq!(result.action, SyncAction::Indexed);
        assert_eq!(result.symbols_removed, 1); // old removed
        assert!(result.symbols_added > 0); // new indexed

        // Verify old is gone
        let old_str = old_path.canonicalize().unwrap();
        let old_symbols = sync
            .indexer
            .get_file_symbols(&old_str.display().to_string())
            .unwrap();
        assert!(old_symbols.is_empty());

        // Verify new is indexed
        let new_str = new_path.canonicalize().unwrap();
        let new_symbols = sync
            .indexer
            .get_file_symbols(&new_str.display().to_string())
            .unwrap();
        assert!(!new_symbols.is_empty());
    }

    #[test]
    fn test_batch_handling() {
        let dir = TempDir::new().unwrap();
        let (mut sync, _db) = setup_index_sync(&dir, "batch.db");

        let mut events = Vec::new();
        for i in 0..10 {
            let path = write_file(&dir, &format!("file_{}.py", i), "def func():\n    pass\n");
            events.push(FileEvent::Created(path));
        }

        let results = sync.handle_batch(events).unwrap();
        assert_eq!(results.len(), 10);

        // All should be indexed
        for r in &results {
            assert_eq!(r.action, SyncAction::Indexed);
            assert_eq!(r.symbols_added, 1);
        }

        // Total symbol count should be 10
        let count = sync.indexer.symbol_count().unwrap();
        assert_eq!(count, 10);
    }

    #[test]
    fn test_unsupported_file_skipped() {
        let dir = TempDir::new().unwrap();
        let path = write_file(&dir, "README.md", "# Hello\n");
        let (mut sync, _db) = setup_index_sync(&dir, "unsup.db");

        let result = sync
            .handle_event(FileEvent::Created(path))
            .unwrap();

        assert_eq!(
            result.action,
            SyncAction::Skipped,
            "Unsupported file type should be skipped"
        );
        assert_eq!(result.symbols_added, 0);
    }

    #[test]
    fn test_blake3_file_hash() {
        let dir = TempDir::new().unwrap();
        let path = write_file(&dir, "test.py", "def foo():\n    pass\n");
        let (sync, _db) = setup_index_sync(&dir, "hash.db");

        let hash1 = sync.file_hash(&path).unwrap();
        let hash2 = sync.file_hash(&path).unwrap();

        assert_eq!(hash1, hash2, "Same content should produce same hash");
        assert_eq!(hash1.len(), 64, "BLAKE3 hex hash should be 64 chars");

        // Different content produces different hash
        std::fs::write(&path, "def bar():\n    pass\n").unwrap();
        let hash3 = sync.file_hash(&path).unwrap();
        assert_ne!(hash1, hash3, "Different content should produce different hash");
    }

    #[test]
    fn test_file_hash_not_found() {
        let dir = TempDir::new().unwrap();
        let (sync, _db) = setup_index_sync(&dir, "nf.db");
        let nonexistent = dir.path().join("nope.py");

        let result = sync.file_hash(&nonexistent);
        assert!(result.is_err());
    }
}
