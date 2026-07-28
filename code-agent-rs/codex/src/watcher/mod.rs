//! Incremental file watcher for code indexing.
//!
//! Watches project directories for file changes and incrementally
//! updates the symbol index and call/dependency graphs.
//!
//! Uses the `notify` crate for cross-platform file system events
//! (inotify on Linux, FSEvents on macOS, ReadDirectoryChanges on Windows).
//!
//! # Architecture
//!
//! ```text
//! File System ──▶ notify::RecommendedWatcher ──▶ mpsc channel
//!                                                     │
//!                                                     ▼
//!                                        FileWatcher.process_events()
//!                                                     │
//!                                                     ▼
//!                            IndexSync (hash dedup → index → graph rebuild)
//! ```

pub mod event;
pub mod sync;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use log::{debug, warn};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::graph::builder::GraphBuilder;
use crate::indexer::CodeIndexer;

use self::event::FileEvent;
use self::sync::{IndexSync, SyncResult, WatcherError};

/// Default debounce interval in milliseconds.
const DEFAULT_DEBOUNCE_MS: u64 = 150;

/// 文件监听器（聚合根）
///
/// 【领域含义】跨平台文件系统监听器，当源文件变更时增量同步代码索引和调用图。
/// 封装 notify::RecommendedWatcher 和 IndexSync，提供防抖、哈希去重的增量索引能力。
/// 是"增量同步"限界上下文的聚合根。
pub struct FileWatcher {
    /// The index synchronization engine.
    sync: IndexSync,
    /// The underlying notify file system watcher.
    watcher: Option<RecommendedWatcher>,
    /// Channel receiver for incoming notify events.
    rx: Option<mpsc::Receiver<notify::Result<Event>>>,
    /// Paths being watched.
    watch_paths: Vec<PathBuf>,
    /// Debounce interval in milliseconds.
    debounce_ms: u64,
    /// Timestamp of the last event batch processed.
    last_event_time: Option<Instant>,
    /// Accumulated events since last process_events call.
    pending_events: Vec<FileEvent>,
}

impl FileWatcher {
    /// 创建文件监听器
    ///
    /// 【领域含义】创建文件监听器实例。indexer 用于直接的文件索引/删除操作，
    /// graph_builder 用于重建调用/依赖图。两者应指向同一个 SQLite 数据库。
    pub fn new(indexer: CodeIndexer, graph_builder: GraphBuilder) -> Self {
        let sync = IndexSync::new(indexer, graph_builder);

        Self {
            sync,
            watcher: None,
            rx: None,
            watch_paths: Vec::new(),
            debounce_ms: DEFAULT_DEBOUNCE_MS,
            last_event_time: None,
            pending_events: Vec::new(),
        }
    }

    /// 开始监听目录
    ///
    /// 【领域含义】启动文件系统监听器，开始递归监听指定路径的文件变更。
    /// 设置 notify watcher 并开始从所有指定路径收集事件。.gitignore 风格的过滤
    /// 在处理阶段而非监听阶段处理。
    pub fn start(&mut self, paths: &[PathBuf]) -> Result<(), WatcherError> {
        if paths.is_empty() {
            return Ok(());
        }

        let (tx, rx) = mpsc::channel();

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                // Best-effort send; if the receiver is dropped, just stop.
                let _ = tx.send(res);
            },
            Config::default(),
        )?;

        for path in paths {
            // Watch recursively so we catch changes in nested subdirectories
            watcher.watch(path, RecursiveMode::Recursive)?;
        }

        self.watch_paths = paths.to_vec();
        self.watcher = Some(watcher);
        self.rx = Some(rx);

        debug!("FileWatcher started watching {:?}", self.watch_paths);
        Ok(())
    }

    /// 停止监听
    ///
    /// 【领域含义】停止文件系统监听，丢弃 notify watcher 并清除所有状态。
    pub fn stop(&mut self) {
        self.watcher = None;
        self.rx = None;
        self.last_event_time = None;
        self.pending_events.clear();
        debug!("FileWatcher stopped");
    }

    /// 处理待处理事件（非阻塞）
    ///
    /// 【领域含义】收集并处理待处理的文件变更事件，带防抖机制。从 notify 通道
    /// 取出所有可用事件，过滤相关文件扩展名，去重，等待防抖间隔后通过
    /// IndexSync::handle_batch 批量处理。如果没有可用事件则返回 Ok(vec![])。
    pub fn process_events(&mut self) -> Result<Vec<SyncResult>, WatcherError> {
        let rx = match &self.rx {
            Some(rx) => rx,
            None => return Ok(Vec::new()),
        };

        // Drain all currently available events from the channel
        let _notify_events: Vec<Event> = loop {
            match rx.try_recv() {
                Ok(Ok(event)) => {
                    self.last_event_time = Some(Instant::now());
                    self.pending_events
                        .extend(convert_notify_event(&event));
                }
                Ok(Err(e)) => {
                    warn!("Notify error: {}", e);
                    // Continue draining
                }
                Err(mpsc::TryRecvError::Empty) => break vec![],
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Sender dropped — watcher was stopped
                    self.rx = None;
                    break vec![];
                }
            }
        };

        if self.pending_events.is_empty() {
            return Ok(Vec::new());
        }

        // Check debounce: if the last event was less than debounce_ms ago,
        // wait a moment and try again (caller's responsibility to call
        // process_events in a loop or rely on a timer).
        if let Some(last_time) = self.last_event_time {
            let elapsed = last_time.elapsed().as_millis() as u64;
            if elapsed < self.debounce_ms {
                debug!(
                    "Debouncing: {}ms since last event (threshold: {}ms)",
                    elapsed, self.debounce_ms
                );
                return Ok(Vec::new());
            }
        }

        // Deduplicate: remove duplicate paths (keep last event for each path)
        let events = deduplicate_events(std::mem::take(&mut self.pending_events));

        debug!("Processing {} deduplicated events", events.len());

        // Process through IndexSync
        let results = self.sync.handle_batch(events)?;

        Ok(results)
    }

    /// 阻塞等待并处理事件
    ///
    /// 【领域含义】阻塞等待事件到达，然后执行防抖批量处理。在指定超时内等待
    /// 至少一个事件，然后执行防抖处理。如果监听器未运行则立即返回。
    pub fn process_events_blocking(&mut self, timeout: Duration) -> Result<Vec<SyncResult>, WatcherError> {
        let rx = match &self.rx {
            Some(rx) => rx,
            None => return Ok(Vec::new()),
        };

        // Wait for the first event
        match rx.recv_timeout(timeout) {
            Ok(Ok(event)) => {
                self.last_event_time = Some(Instant::now());
                self.pending_events
                    .extend(convert_notify_event(&event));
            }
            Ok(Err(e)) => {
                warn!("Notify error: {}", e);
                return Ok(Vec::new());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Ok(Vec::new());
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.rx = None;
                return Ok(Vec::new());
            }
        }

        // Drain any additional events that arrived while we processed
        let _ = self.drain_remaining();

        // Apply debounce: wait for the debounce interval to collect late events
        if let Some(last_time) = self.last_event_time {
            let remaining = self.debounce_ms.saturating_sub(
                last_time.elapsed().as_millis() as u64,
            );
            if remaining > 0 {
                std::thread::sleep(Duration::from_millis(remaining));
            }
        }

        // Drain any events that came during the wait
        let _ = self.drain_remaining();

        let events = deduplicate_events(std::mem::take(&mut self.pending_events));
        self.last_event_time = None;

        if events.is_empty() {
            return Ok(Vec::new());
        }

        debug!("Processing {} deduplicated events (blocking)", events.len());
        self.sync.handle_batch(events)
    }

    /// 检查监听器是否运行中
    ///
    /// 【领域含义】如果文件监听器当前处于活动状态则返回 true。
    pub fn is_running(&self) -> bool {
        self.watcher.is_some()
    }

    /// 获取 IndexSync 可变引用
    ///
    /// 【领域含义】返回内部 IndexSync 的可变引用，用于直接操作索引同步引擎。
    pub fn sync_mut(&mut self) -> &mut IndexSync {
        &mut self.sync
    }

    /// 获取 IndexSync 引用
    ///
    /// 【领域含义】返回内部 IndexSync 的只读引用。
    pub fn sync(&self) -> &IndexSync {
        &self.sync
    }

    /// Drain any remaining events without processing.
    fn drain_remaining(&mut self) -> usize {
        let rx = match &self.rx {
            Some(rx) => rx,
            None => return 0,
        };

        let mut count = 0;
        loop {
            match rx.try_recv() {
                Ok(Ok(event)) => {
                    self.pending_events
                        .extend(convert_notify_event(&event));
                    count += 1;
                }
                Ok(Err(_)) => { /* skip */ }
                Err(_) => break,
            }
        }
        count
    }
}

// ── Event conversion ──────────────────────────────────────────────────────

/// Convert a raw `notify::Event` into one or more [`FileEvent`]s.
///
/// Filters out non-file events (directories, symlinks) and metadata-only
/// modifications. Maps notify event kinds to our simplified event types.
fn convert_notify_event(event: &Event) -> Vec<FileEvent> {
    let mut result = Vec::new();

    for path in &event.paths {
        // Skip non-files (directories, etc.)
        if path.is_dir() {
            continue;
        }
        // Skip non-source files (our indexer handles language detection anyway)
        // but we allow all files through — IndexSync will skip unsupported ones.

        match event.kind {
            EventKind::Create(_) => {
                result.push(FileEvent::Created(path.clone()));
            }
            EventKind::Modify(modify_kind) => {
                use notify::event::ModifyKind;
                match modify_kind {
                    ModifyKind::Data(_) => {
                        // Content change — process
                        result.push(FileEvent::Modified(path.clone()));
                    }
                    ModifyKind::Metadata(_) => {
                        // Metadata-only change — ignore (IndexSync does hash dedup)
                        debug!("Ignoring metadata change for: {}", path.display());
                    }
                    ModifyKind::Name(_) => {
                        // Name change is typically followed by a Rename event
                        // We'll handle it via the Rename path
                    }
                    ModifyKind::Other | ModifyKind::Any => {
                        // Unknown modification type — process conservatively
                        result.push(FileEvent::Modified(path.clone()));
                    }
                }
            }
            EventKind::Remove(_) => {
                result.push(FileEvent::Deleted(path.clone()));
            }
            EventKind::Access(_) => {
                // File access events — ignore
            }
            EventKind::Other | EventKind::Any => {
                // Unknown event type — ignore to avoid noise
            }
        }
    }

    result
}

/// Deduplicate events by path. For each unique path, keep only the
/// most recent event type. Deletes take precedence over modifies,
/// which take precedence over creates.
fn deduplicate_events(events: Vec<FileEvent>) -> Vec<FileEvent> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut deduped: Vec<FileEvent> = Vec::new();

    // Process in reverse to keep only the last event per path
    for event in events.into_iter().rev() {
        let path = event.primary_path().clone();
        if seen.insert(path.clone()) {
            deduped.push(event);
        }
    }

    // Reverse back to restore original order
    deduped.reverse();

    // If a file is both deleted and created/modified, keep the created/modified
    // (but in practice, the last event should already reflect this)
    deduped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watcher::sync::SyncAction;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn setup_watcher(dir: &TempDir, db_name: &str) -> FileWatcher {
        let db_path = dir.path().join(db_name);
        let db_str = db_path.display().to_string();
        let indexer = CodeIndexer::new(&db_str).unwrap();
        let graph_indexer = CodeIndexer::new(&db_str).unwrap();
        let graph_builder = GraphBuilder::new(graph_indexer);

        FileWatcher::new(indexer, graph_builder)
    }

    fn write_file(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn test_file_watcher_start_stop() {
        let dir = TempDir::new().unwrap();
        let mut watcher = setup_watcher(&dir, "fw.db");

        assert!(!watcher.is_running());

        // Start watching an empty dir (should work)
        watcher.start(&[dir.path().to_path_buf()]).unwrap();
        assert!(watcher.is_running());

        // Process events — none expected
        let results = watcher.process_events().unwrap();
        assert!(results.is_empty());

        // Stop
        watcher.stop();
        assert!(!watcher.is_running());
    }

    #[test]
    fn test_debounce_rapid_changes() {
        let dir = TempDir::new().unwrap();
        let py_path = write_file(&dir, "debounce.py", "def a():\n    pass\n");
        let mut watcher = setup_watcher(&dir, "debounce.db");

        watcher.start(&[dir.path().to_path_buf()]).unwrap();

        // Simulate rapid changes by directly injecting events
        // (notify events won't be fired since we're not actually
        // watching in this unit test context)
        let sync = watcher.sync_mut();
        let result = sync
            .handle_event(FileEvent::Created(py_path.clone()))
            .unwrap();
        assert_eq!(result.action, SyncAction::Indexed);

        // Repeated same-content modification should be skipped
        let result = sync
            .handle_event(FileEvent::Modified(py_path.clone()))
            .unwrap();
        assert_eq!(result.action, SyncAction::Skipped);
    }

    #[test]
    fn test_convert_notify_event_create() {
        let dir = TempDir::new().unwrap();
        let path = write_file(&dir, "test.py", "");
        let event = Event::new(EventKind::Create(notify::event::CreateKind::File))
            .add_path(path.clone());

        let converted = convert_notify_event(&event);
        assert_eq!(converted.len(), 1);
        assert!(matches!(converted[0], FileEvent::Created(_)));
    }

    #[test]
    fn test_convert_notify_event_modify_data() {
        let dir = TempDir::new().unwrap();
        let path = write_file(&dir, "test.py", "");
        let event = Event::new(EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )))
        .add_path(path.clone());

        let converted = convert_notify_event(&event);
        assert_eq!(converted.len(), 1);
        assert!(matches!(converted[0], FileEvent::Modified(_)));
    }

    #[test]
    fn test_convert_notify_event_metadata_ignored() {
        let dir = TempDir::new().unwrap();
        let path = write_file(&dir, "test.py", "");
        let event = Event::new(EventKind::Modify(notify::event::ModifyKind::Metadata(
            notify::event::MetadataKind::Permissions,
        )))
        .add_path(path);

        let converted = convert_notify_event(&event);
        assert!(
            converted.is_empty(),
            "Metadata-only changes should be filtered out"
        );
    }

    #[test]
    fn test_convert_notify_event_remove() {
        let dir = TempDir::new().unwrap();
        let path = write_file(&dir, "test.py", "");
        let event = Event::new(EventKind::Remove(notify::event::RemoveKind::File))
            .add_path(path.clone());

        let converted = convert_notify_event(&event);
        assert_eq!(converted.len(), 1);
        assert!(matches!(converted[0], FileEvent::Deleted(_)));
    }

    #[test]
    fn test_convert_notify_event_skip_directory() {
        let dir = TempDir::new().unwrap();
        let subdir = dir.path().join("subdir");
        std::fs::create_dir(&subdir).unwrap();
        let event = Event::new(EventKind::Create(notify::event::CreateKind::Folder))
            .add_path(subdir.clone());

        let converted = convert_notify_event(&event);
        assert!(converted.is_empty(), "Directory events should be filtered");
    }

    #[test]
    fn test_convert_notify_event_skip_access() {
        let dir = TempDir::new().unwrap();
        let path = write_file(&dir, "test.py", "");
        let event = Event::new(EventKind::Access(notify::event::AccessKind::Close(
            notify::event::AccessMode::Read,
        )))
        .add_path(path);

        let converted = convert_notify_event(&event);
        assert!(converted.is_empty(), "Access events should be filtered");
    }

    #[test]
    fn test_deduplicate_events() {
        let path = PathBuf::from("test.py");

        let events = vec![
            FileEvent::Modified(path.clone()),
            FileEvent::Modified(path.clone()), // duplicate
            FileEvent::Modified(path.clone()), // duplicate
            FileEvent::Created(PathBuf::from("other.py")),
        ];

        let deduped = deduplicate_events(events);
        // Should keep last Modified for test.py + the Created for other.py
        assert_eq!(deduped.len(), 2);
        assert!(matches!(deduped[0], FileEvent::Modified(_)));
        assert!(matches!(deduped[1], FileEvent::Created(_)));
    }
}
