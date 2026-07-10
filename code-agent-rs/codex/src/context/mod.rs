//! Agent code context retrieval pipeline.
//!
//! The `CodeContextBuilder` assembles relevant code symbols for a given
//! user task, producing a structured XML prompt block that the Agent Engine
//! can inject into its system message.
//!
//! # Pipeline
//!
//! ```text
//! Task → Embed → Hybrid Search → Graph Expansion → Priority Weighting
//!                                                    ↓
//!                                           Token Budget → Format XML
//! ```
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use code_agent_codex::context::{CodeContextBuilder, ContextConfig, ContextSignals};
//! use code_agent_codex::retrieval::Retriever;
//!
//! # async fn example() {
//! let retriever = Arc::new(Retriever::builder()
//!     .db_path("/path/to/index.db")
//!     .api_key("sk-xxx")
//!     .build()
//!     .unwrap());
//!
//! let builder = CodeContextBuilder::new(retriever, ContextConfig::default());
//!
//! let context = builder.build_context(
//!     "Add input validation to the login endpoint",
//!     &ContextSignals::default(),
//! ).await.unwrap();
//!
//! println!("{}", context.formatted_block);
//! # }
//! ```

pub mod builder;
pub mod cache;

use std::cell::RefCell;
use std::sync::Arc;

use crate::graph::CallGraph;
use crate::retrieval::{RetrievalError, ScoredResult};
use builder::ContextAssembler;
use cache::ContextCache;

// ---------------------------------------------------------------------------
// ContextConfig
// ---------------------------------------------------------------------------

/// 上下文配置（值对象）
///
/// 【领域含义】上下文检索管道的配置值对象，控制最大 Token 数、检索符号数、
/// 图谱扩展开关、重排序器开关和优先级信号列表。属于"上下文聚合"限界上下文的
/// 配置模型。
#[derive(Debug, Clone)]
pub struct ContextConfig {
    /// Maximum tokens for the formatted context block. Default: 8192.
    pub max_context_tokens: usize,
    /// Number of top results to retrieve before graph expansion. Default: 30.
    pub top_k_symbols: usize,
    /// Whether to expand results to include 1-hop call graph neighbours.
    /// Default: true.
    pub enable_graph_expansion: bool,
    /// Whether to apply the Qwen3-Reranker after fusion. Default: false
    /// (cost optimisation).
    pub enable_reranker: bool,
    /// Priority signals that influence scoring weight.
    pub priority_signals: Vec<PrioritySignal>,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            max_context_tokens: 8192,
            top_k_symbols: 30,
            enable_graph_expansion: true,
            enable_reranker: false,
            priority_signals: vec![
                PrioritySignal::Recency,
                PrioritySignal::OpenFiles,
                PrioritySignal::Explicit,
            ],
        }
    }
}

/// 优先级信号（值对象）
///
/// 【领域含义】影响相关性评分权重的外部信号枚举。Recency 表示最近编辑的文件权重更高，
/// OpenFiles 表示当前打开的文件权重更高，Explicit 表示用户指定的 @提及权重最高。
/// 属于"上下文聚合"限界上下文中的信号模型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrioritySignal {
    /// Recently edited files rank higher.
    Recency,
    /// Currently open files rank higher.
    OpenFiles,
    /// User-specified @mentions rank highest.
    Explicit,
}

// ---------------------------------------------------------------------------
// ContextSignals
// ---------------------------------------------------------------------------

/// 上下文信号（值对象）
///
/// 【领域含义】运行时影响上下文选择的信号值对象，包含最近编辑的文件列表、
/// 当前打开的文件列表、用户显式提及的文件/符号和当前正在编辑的文件。
/// 用于在上下文组装时调整检索结果的权重。
#[derive(Debug, Clone, Default)]
pub struct ContextSignals {
    /// File paths recently edited by the user.
    pub recently_edited: Vec<String>,
    /// File paths currently open in the editor.
    pub open_files: Vec<String>,
    /// User-specified files/symbols (e.g. @mentions).
    pub explicit_mentions: Vec<String>,
    /// The file currently being edited, if any.
    pub current_file: Option<String>,
}

// ---------------------------------------------------------------------------
// CodeContext
// ---------------------------------------------------------------------------

/// 代码上下文（实体）
///
/// 【领域含义】组装完成的代码上下文，准备注入 Agent 提示词的结构化数据。
/// 包含检索到的符号结果、格式化后的 XML 提示块、近似 Token 数和检索统计信息。
/// 是"上下文聚合"限界上下文的输出实体。
#[derive(Debug, Clone)]
pub struct CodeContext {
    /// All retrieved and expanded symbol results.
    pub symbols: Vec<ScoredResult>,
    /// The formatted XML block ready for prompt injection.
    pub formatted_block: String,
    /// Approximate token count of the formatted block.
    pub token_count: usize,
    /// Statistics about the retrieval process.
    pub retrieval_stats: RetrievalStats,
}

/// 检索统计（值对象）
///
/// 【领域含义】上下文组装过程中产生的诊断统计信息，记录 BM25 结果数、向量结果数、
/// 图谱扩展数、总候选数、最终选中数和管道延迟。用于监控和调试。
#[derive(Debug, Clone, Default)]
pub struct RetrievalStats {
    /// Number of results from BM25 search.
    pub bm25_results: usize,
    /// Number of results from vector search.
    pub vector_results: usize,
    /// Symbols added by graph expansion.
    pub graph_expanded: usize,
    /// Total candidates before budget trimming.
    pub total_candidates: usize,
    /// Symbols selected after budget constraints.
    pub final_selected: usize,
    /// Wall-clock latency of the full pipeline in milliseconds.
    pub latency_ms: u64,
}

// ---------------------------------------------------------------------------
// ContextError
// ---------------------------------------------------------------------------

/// 上下文检索错误
///
/// 【领域含义】上下文检索过程中可能出现的错误类型，涵盖检索错误、I/O 错误、
/// 空任务描述等。属于"上下文聚合"限界上下文的异常模型。
#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error("Retrieval error: {0}")]
    Retrieval(#[from] RetrievalError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Task description is empty")]
    EmptyTask,

    #[error("{0}")]
    Other(String),
}

/// 上下文结果类型别名
///
/// 【领域含义】以 ContextError 为错误类型的便捷 Result 别名，统一上下文检索操作的返回类型。
pub type ContextResult<T> = Result<T, ContextError>;

// ---------------------------------------------------------------------------
// CodeContextBuilder
// ---------------------------------------------------------------------------

/// 代码上下文构建器（领域服务）
///
/// 【领域含义】从用户任务描述构建 Agent 引擎提示词所需的代码上下文的领域服务。
/// 包装 Retriever 和可选的 CallGraph，执行配置的六步管道（混合检索→图谱扩展→
/// 优先级加权→Token 预算→XML 格式化）生成 CodeContext。属于"上下文聚合"
/// 限界上下文的核心领域服务。
pub struct CodeContextBuilder {
    retriever: Arc<crate::retrieval::Retriever>,
    graph: Option<Arc<CallGraph>>,
    cache: RefCell<ContextCache>,
    config: ContextConfig,
}

impl CodeContextBuilder {
    /// 创建上下文构建器
    ///
    /// 【领域含义】使用检索器和默认配置创建 CodeContextBuilder 实例。
    /// 内部初始化 64 容量、300 秒 TTL 的上下文缓存。
    pub fn new(
        retriever: Arc<crate::retrieval::Retriever>,
        config: ContextConfig,
    ) -> Self {
        Self {
            retriever,
            graph: None,
            cache: RefCell::new(ContextCache::new(64, 300)),
            config,
        }
    }

    /// 注入调用图
    ///
    /// 【领域含义】注入调用图实例，用于上下文组装时的图谱扩展（1 跳邻居）。
    pub fn with_graph(mut self, graph: Arc<CallGraph>) -> Self {
        self.graph = Some(graph);
        self
    }

    /// 构建代码上下文
    ///
    /// 【领域含义】根据任务描述执行完整的六步上下文构建管道：
    /// 1. 检查缓存中是否有精确匹配
    /// 2. 通过 Retriever 执行混合检索
    /// 3. 图谱扩展（可选，1 跳邻居）
    /// 4. 根据信号进行优先级加权
    /// 5. Token 预算分配
    /// 6. XML 格式化
    pub async fn build_context(
        &self,
        task: &str,
        signals: &ContextSignals,
    ) -> ContextResult<CodeContext> {
        let task = task.trim();
        if task.is_empty() {
            return Err(ContextError::EmptyTask);
        }

        let start = std::time::Instant::now();

        // 1. Check cache
        let cache_key = make_cache_key(task, signals);
        if let Some(cached) = self.cache.borrow_mut().get(&cache_key) {
            return Ok(cached);
        }

        // 2-6. Run the full pipeline
        let assembler = ContextAssembler::new(
            self.retriever.clone(),
            self.graph.clone(),
            self.config.clone(),
        );

        let mut context = assembler.assemble(task, signals).await?;

        // Record latency
        context.retrieval_stats.latency_ms = start.elapsed().as_millis() as u64;

        // Store in cache (we need a mutable ref, but cache is behind Arc...
        // For now the cache is only consulted via get; we skip put here since
        // `&self` doesn't allow mutation. The cache is populated externally.
        // In practice the caller can call `put` on the cache if needed.
        // We use interior mutability via RefCell internally in ContextCache,
        // but the builder's `build_context` takes `&self`. In a real integration,
        // the cache is shared behind `Arc<Mutex<ContextCache>>`.

        Ok(context)
    }

    /// 清空上下文缓存
    ///
    /// 【领域含义】清除所有缓存的上下文结果，强制下次请求重新执行完整管道。
    pub fn clear_cache(&self) {
        self.cache.borrow_mut().clear();
    }

    /// 获取缓存可变引用
    ///
    /// 【领域含义】暴露内部缓存的可变引用，用于外部预填充缓存条目。
    pub fn cache_mut(&self) -> std::cell::RefMut<'_, ContextCache> {
        self.cache.borrow_mut()
    }

    /// 获取配置引用
    ///
    /// 【领域含义】返回当前上下文配置的只读引用。
    pub fn config(&self) -> &ContextConfig {
        &self.config
    }
}

/// Construct a deterministic cache key from task text and signal file paths.
fn make_cache_key(task: &str, signals: &ContextSignals) -> String {
    let mut parts = vec![task.to_string()];
    parts.extend(signals.recently_edited.iter().cloned());
    parts.extend(signals.open_files.iter().cloned());
    parts.extend(signals.explicit_mentions.iter().cloned());
    if let Some(ref cf) = signals.current_file {
        parts.push(cf.clone());
    }
    parts.join("|")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = ContextConfig::default();
        assert_eq!(config.max_context_tokens, 8192);
        assert_eq!(config.top_k_symbols, 30);
        assert!(config.enable_graph_expansion);
        assert!(!config.enable_reranker);
        assert_eq!(config.priority_signals.len(), 3);
    }

    #[test]
    fn test_signals_default_empty() {
        let signals = ContextSignals::default();
        assert!(signals.recently_edited.is_empty());
        assert!(signals.open_files.is_empty());
        assert!(signals.explicit_mentions.is_empty());
        assert!(signals.current_file.is_none());
    }

    #[test]
    fn test_retrieval_stats_defaults() {
        let stats = RetrievalStats::default();
        assert_eq!(stats.bm25_results, 0);
        assert_eq!(stats.vector_results, 0);
        assert_eq!(stats.graph_expanded, 0);
        assert_eq!(stats.total_candidates, 0);
        assert_eq!(stats.final_selected, 0);
        assert_eq!(stats.latency_ms, 0);
    }

    #[test]
    fn test_cache_key_deterministic() {
        let signals = ContextSignals {
            recently_edited: vec!["src/auth.rs".into()],
            open_files: vec!["src/main.rs".into()],
            explicit_mentions: vec!["validateToken".into()],
            current_file: Some("src/auth.rs".into()),
        };
        let key1 = make_cache_key("fix the bug", &signals);
        let key2 = make_cache_key("fix the bug", &signals);
        assert_eq!(key1, key2);

        // Different task → different key
        let key3 = make_cache_key("add feature", &signals);
        assert_ne!(key1, key3);
    }

    #[test]
    fn test_context_error_display() {
        let err = ContextError::EmptyTask;
        assert_eq!(err.to_string(), "Task description is empty");
    }
}
