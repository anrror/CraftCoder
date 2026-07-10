//! LRU context cache with TTL-based expiry and per-file invalidation.
//!
//! The cache stores [`CodeContext`] results keyed by a hash of the task
//! description and signal state. Entries expire after a configurable TTL.
//! Individual files can be invalidated when they are modified on disk.

use std::collections::hash_map::RandomState;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use lru::LruCache;

use super::CodeContext;

/// 上下文缓存（基础设施）
///
/// 【领域含义】基于 LRU 的 CodeContext 结果缓存，支持 TTL 过期和按文件失效。
/// 缓存键为任务描述和信号状态的哈希组合，条目在可配置的 TTL 后过期。
/// 当磁盘文件变更时可按文件路径失效相关缓存条目。属于"上下文聚合"限界上下文的
/// 基础设施层组件。
pub struct ContextCache {
    cache: LruCache<String, CachedEntry, RandomState>,
    ttl: Duration,
}

/// 缓存条目（值对象）
///
/// 【领域含义】缓存中的上下文条目值对象，包含缓存的 CodeContext 和插入时间戳，
/// 用于 TTL 过期判断。
#[derive(Debug, Clone)]
pub struct CachedEntry {
    pub context: CodeContext,
    pub cached_at: Instant,
}

impl ContextCache {
    /// 创建上下文缓存
    ///
    /// 【领域含义】创建指定容量和 TTL（秒）的上下文缓存。容量必须大于 0，否则 panic。
    pub fn new(capacity: usize, ttl_secs: u64) -> Self {
        let cap = NonZeroUsize::new(capacity).expect("ContextCache capacity must be > 0");
        Self {
            cache: LruCache::with_hasher(cap, RandomState::default()),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    /// 按键查找缓存
    ///
    /// 【领域含义】根据键查找缓存的上下文。如果未找到或条目已过期则返回 None。
    /// 过期的条目会自动从缓存中移除。
    pub fn get(&mut self, key: &str) -> Option<CodeContext> {
        if let Some(entry) = self.cache.get(key) {
            if entry.cached_at.elapsed() < self.ttl {
                return Some(entry.context.clone());
            }
            // Expired — remove it
            self.cache.pop(key);
        }
        None
    }

    /// 存储上下文到缓存
    ///
    /// 【领域含义】将上下文结果存入缓存，附带当前时间戳用于 TTL 过期判断。
    pub fn put(&mut self, key: String, context: CodeContext) {
        self.cache.put(
            key,
            CachedEntry {
                context,
                cached_at: Instant::now(),
            },
        );
    }

    /// 按文件路径失效缓存
    ///
    /// 【领域含义】失效所有引用了指定文件路径的缓存条目。当磁盘文件变更时调用，
    /// 确保下次请求使用最新数据重新检索。
    pub fn invalidate(&mut self, file: &str) {
        // Collect keys to remove (we can't iterate and mutate simultaneously
        // with LruCache's API, so we collect first).
        let keys_to_remove: Vec<String> = self
            .cache
            .iter()
            .filter(|(_, entry)| {
                entry
                    .context
                    .symbols
                    .iter()
                    .any(|s| s.file_path == file)
            })
            .map(|(key, _)| key.clone())
            .collect();

        for key in keys_to_remove {
            self.cache.pop(&key);
        }
    }

    /// 清空缓存
    ///
    /// 【领域含义】移除缓存中的所有条目。
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// 获取缓存条目数
    ///
    /// 【领域含义】返回当前缓存中的条目数量。
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// 判断缓存是否为空
    ///
    /// 【领域含义】如果缓存中没有条目则返回 true。
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// 驱逐过期条目
    ///
    /// 【领域含义】移除所有已过期的缓存条目。可定期调用或在容量检查前调用。
    /// 返回被驱逐的条目数。
    pub fn evict_expired(&mut self) -> usize {
        let now = Instant::now();
        let keys_to_remove: Vec<String> = self
            .cache
            .iter()
            .filter(|(_, entry)| now.duration_since(entry.cached_at) >= self.ttl)
            .map(|(key, _)| key.clone())
            .collect();

        let count = keys_to_remove.len();
        for key in keys_to_remove {
            self.cache.pop(&key);
        }
        count
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::RetrievalStats;
    use crate::retrieval::ScoredResult;

    fn make_context(symbols: Vec<ScoredResult>) -> CodeContext {
        CodeContext {
            symbols,
            formatted_block: "<context></context>".into(),
            token_count: 10,
            retrieval_stats: RetrievalStats::default(),
        }
    }

    fn make_result(file: &str, name: &str) -> ScoredResult {
        ScoredResult {
            symbol_name: name.to_string(),
            symbol_kind: "function".to_string(),
            file_path: file.to_string(),
            line_range: 1..5,
            score: 0.9,
            source: "test".to_string(),
        }
    }

    // ── Basic cache operations ──────────────────────────────────────────

    #[test]
    fn test_cache_put_and_get() {
        let mut cache = ContextCache::new(8, 60);
        let ctx = make_context(vec![make_result("src/main.rs", "main")]);

        cache.put("key1".into(), ctx);
        assert_eq!(cache.len(), 1);

        let retrieved = cache.get("key1");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().token_count, 10);
    }

    #[test]
    fn test_cache_miss() {
        let mut cache = ContextCache::new(8, 60);
        let result = cache.get("nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn test_cache_clear() {
        let mut cache = ContextCache::new(8, 60);
        cache.put("k1".into(), make_context(vec![]));
        cache.put("k2".into(), make_context(vec![]));
        assert_eq!(cache.len(), 2);

        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn test_cache_overwrite() {
        let mut cache = ContextCache::new(8, 60);
        cache.put("key".into(), make_context(vec![make_result("a.rs", "a")]));
        cache.put("key".into(), make_context(vec![make_result("b.rs", "b")]));

        let result = cache.get("key").unwrap();
        assert_eq!(result.symbols[0].symbol_name, "b");
    }

    // ── TTL expiry ─────────────────────────────────────────────────────

    #[test]
    fn test_cache_ttl_expiry() {
        // Use a short TTL for testing
        let mut cache = ContextCache::new(8, 0); // 0 seconds TTL means instant expiry

        cache.put("key".into(), make_context(vec![]));
        assert_eq!(cache.len(), 1);

        // Entry should be considered expired immediately
        let result = cache.get("key");
        assert!(result.is_none());
        assert_eq!(cache.len(), 0, "Expired entry should be removed");
    }

    #[test]
    fn test_evict_expired() {
        let mut cache = ContextCache::new(8, 0); // 0-second TTL
        cache.put("k1".into(), make_context(vec![]));
        cache.put("k2".into(), make_context(vec![]));
        assert_eq!(cache.len(), 2);

        let evicted = cache.evict_expired();
        assert_eq!(evicted, 2);
        assert_eq!(cache.len(), 0);
    }

    // ── Invalidation by file ───────────────────────────────────────────

    #[test]
    fn test_cache_invalidate_by_file() {
        let mut cache = ContextCache::new(8, 600);

        cache.put(
            "k1".into(),
            make_context(vec![
                make_result("src/auth.rs", "login"),
                make_result("src/db.rs", "query"),
            ]),
        );
        cache.put(
            "k2".into(),
            make_context(vec![make_result("src/api.rs", "handler")]),
        );

        assert_eq!(cache.len(), 2);

        // Invalidate src/auth.rs — should remove k1 only
        cache.invalidate("src/auth.rs");

        assert_eq!(cache.len(), 1, "Only k1 should be invalidated");
        assert!(cache.get("k1").is_none());
        assert!(cache.get("k2").is_some());
    }

    #[test]
    fn test_cache_invalidate_nonexistent_file() {
        let mut cache = ContextCache::new(8, 600);
        cache.put("k1".into(), make_context(vec![make_result("src/a.rs", "f")]));
        assert_eq!(cache.len(), 1);

        cache.invalidate("nonexistent.rs");
        assert_eq!(cache.len(), 1); // No change
    }

    // ── LRU eviction ───────────────────────────────────────────────────

    #[test]
    fn test_cache_lru_eviction() {
        let mut cache = ContextCache::new(2, 600);

        cache.put("a".into(), make_context(vec![]));
        cache.put("b".into(), make_context(vec![]));
        assert_eq!(cache.len(), 2);

        // Access "a" to make it most recently used
        let _ = cache.get("a");

        // Insert "c" — should evict "b" (least recently used)
        cache.put("c".into(), make_context(vec![]));
        assert_eq!(cache.len(), 2);

        assert!(cache.get("a").is_some(), "a should still be cached (was accessed)");
        assert!(cache.get("b").is_none(), "b should be evicted (LRU)");
        assert!(cache.get("c").is_some(), "c should be cached");
    }

    #[test]
    fn test_cache_len_and_is_empty() {
        let mut cache = ContextCache::new(4, 600);
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);

        cache.put("k".into(), make_context(vec![]));
        assert!(!cache.is_empty());
        assert_eq!(cache.len(), 1);
    }
}
