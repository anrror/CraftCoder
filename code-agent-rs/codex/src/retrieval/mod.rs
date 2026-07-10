//! Hybrid retrieval pipeline — BM25 + Vector + Graph reranking.
//!
//! Combines lexical search (SQLite FTS5), semantic search (vector embeddings),
//! and graph-based reranking (call-graph centrality) into a unified code
//! retrieval interface.
//!
//! # Pipeline
//!
//! ```text
//! Query
//!   ├─ BM25 search (lexical)        ─┐
//!   └─ Vector search (semantic)     ─┤
//!                                     ├─ RRF fusion ──→ Reranker ──→ Graph boost ──→ Results
//! ```

pub mod bm25;
pub mod fusion;
pub mod reranker;
pub mod vector;

use std::ops::Range;
use std::sync::Arc;

use crate::graph::CallGraph;
use bm25::Bm25Searcher;
use fusion::RrfFusion;
use reranker::Qwen3Reranker;
use vector::VectorSearcher;

// ---------------------------------------------------------------------------
// ScoredResult
// ---------------------------------------------------------------------------

/// 评分搜索结果（值对象）
///
/// 【领域含义】带有相关性评分的搜索结果值对象，记录匹配的符号信息、文件位置、
/// 评分和来源后端（bm25/vector/hybrid）。是"代码检索"限界上下文的核心值对象。
#[derive(Debug, Clone)]
pub struct ScoredResult {
    /// Name of the matched symbol.
    pub symbol_name: String,
    /// Kind of the symbol (function, class, struct, etc.).
    pub symbol_kind: String,
    /// Path to the file containing the symbol.
    pub file_path: String,
    /// 1-based line range within the file.
    pub line_range: Range<usize>,
    /// Relevance score. Higher is better. Interpretations vary by source.
    pub score: f64,
    /// Which search backend produced this result: `"bm25"`, `"vector"`, or `"hybrid"`.
    pub source: String,
}

impl ScoredResult {
    /// 从符号条目创建评分结果
    ///
    /// 【领域含义】将索引器中的 SymbolEntry 转换为检索评分结果，保留符号名称、
    /// 类型、文件路径和行范围信息。
    pub fn from_symbol(
        sym: &crate::indexer::symbol::SymbolEntry,
        score: f64,
        source: &str,
    ) -> Self {
        Self {
            symbol_name: sym.symbol_name.clone(),
            symbol_kind: sym.symbol_kind.as_str().to_string(),
            file_path: sym.file_path.clone(),
            line_range: sym.line_range.clone(),
            score,
            source: source.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// SearchOptions
// ---------------------------------------------------------------------------

/// 搜索选项（值对象）
///
/// 【领域含义】控制混合检索管道行为的配置值对象，包括返回数量上限、语言过滤、
/// 是否启用重排序器和图谱中心性提升。属于"代码检索"限界上下文的查询参数模型。
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Maximum number of results to return.
    pub top_k: usize,
    /// Optional language filter (e.g. `["python", "rust"]`).
    pub languages: Option<Vec<String>>,
    /// Whether to apply the reranker after fusion.
    pub use_reranker: bool,
    /// Whether to boost results using call-graph centrality.
    pub use_graph: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            top_k: 10,
            languages: None,
            use_reranker: true,
            use_graph: true,
        }
    }
}

// ---------------------------------------------------------------------------
// RetrievalError
// ---------------------------------------------------------------------------

/// 检索错误
///
/// 【领域含义】检索操作过程中可能出现的错误类型，涵盖 BM25 搜索、向量搜索、
/// 重排序器、融合、SQLite、索引器、图谱、HTTP 和超时等异常。属于"代码检索"
/// 限界上下文的异常模型。
#[derive(Debug, thiserror::Error)]
pub enum RetrievalError {
    #[error("BM25 search error: {0}")]
    Bm25(String),

    #[error("Vector search error: {0}")]
    Vector(String),

    #[error("Reranker error: {0}")]
    Reranker(String),

    #[error("Fusion error: {0}")]
    Fusion(String),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Indexer error: {0}")]
    Indexer(#[from] crate::indexer::IndexerError),

    #[error("Graph error: {0}")]
    Graph(#[from] crate::graph::GraphError),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("{0}")]
    Other(String),
}

/// 检索结果类型别名
///
/// 【领域含义】以 RetrievalError 为错误类型的便捷 Result 别名，统一检索操作的返回类型。
pub type RetrievalResult<T> = Result<T, RetrievalError>;

// ---------------------------------------------------------------------------
// Retriever
// ---------------------------------------------------------------------------

/// 混合检索器（聚合根）
///
/// 【领域含义】融合 BM25 词法搜索、向量语义搜索和图谱重排序的混合代码检索器，
/// 是"代码检索"限界上下文的聚合根。内部协调 Bm25Searcher、VectorSearcher、
/// Qwen3Reranker、RrfFusion 和 CallGraph 五个领域对象，提供统一的检索接口。
///
/// # 示例
///
/// ```no_run
/// use code_agent_codex::retrieval::{Retriever, SearchOptions};
///
/// # async fn example() {
/// let retriever = Retriever::builder()
///     .db_path("/path/to/index.db")
///     .api_base_url("http://localhost:8080/v1")
///     .api_key("sk-xxx")
///     .build()
///     .unwrap();
///
/// let results = retriever.search(
///     "find_user_by_email",
///     SearchOptions::default(),
/// ).await.unwrap();
/// # }
/// ```
pub struct Retriever {
    bm25: Bm25Searcher,
    vector: VectorSearcher,
    reranker: Qwen3Reranker,
    fusion: RrfFusion,
    graph: Option<Arc<CallGraph>>,
}

impl Retriever {
    /// 创建检索器构建器
    ///
    /// 【领域含义】返回 RetrieverBuilder 实例，用于通过构建器模式配置和创建 Retriever。
    pub fn builder() -> RetrieverBuilder {
        RetrieverBuilder::default()
    }

    /// 执行混合检索管道
    ///
    /// 【领域含义】执行完整的五步混合检索管道：
    /// 1. BM25 词法搜索
    /// 2. 向量语义搜索
    /// 3. 倒数排序融合（RRF）合并
    /// Step 4: Reranker 重排序（可选）
    /// 5. 图谱中心性提升（可选）
    /// 返回按相关性评分降序排列的搜索结果列表。
    pub async fn search(
        &self,
        query: &str,
        opts: SearchOptions,
    ) -> RetrievalResult<Vec<ScoredResult>> {
        // Step 1: BM25 search
        let bm25_results = if let Some(ref langs) = opts.languages {
            let mut all = Vec::new();
            for lang in langs {
                let mut partial = self.bm25.search_filtered(query, lang, opts.top_k * 2)?;
                all.append(&mut partial);
            }
            // Deduplicate and re-score
            all.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
            all.dedup_by(|a, b| a.symbol_name == b.symbol_name && a.file_path == b.file_path);
            all.truncate(opts.top_k * 2);
            all
        } else {
            self.bm25.search(query, opts.top_k * 2)?
        };

        // Step 2: Vector search
        let vector_results = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.vector.search(query, opts.top_k * 2),
        )
        .await
        .map_err(|_| RetrievalError::Timeout("Vector search timed out after 30s".into()))??;

        // Step 3: RRF fusion
        let fused = self.fusion.fuse(vec![bm25_results, vector_results]);

        let mut results = fused;
        results.truncate(opts.top_k * 2);

        // Step 4: Reranker (optional)
        if opts.use_reranker && !results.is_empty() {
            results = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                self.reranker.rerank(query, &results, opts.top_k),
            )
            .await
            .map_err(|_| RetrievalError::Timeout("Reranker timed out after 30s".into()))??;
        }

        // Step 5: Graph centrality boost (optional)
        if opts.use_graph {
            if let Some(ref graph) = self.graph {
                self.fusion.rerank_with_graph(&mut results, graph);
            }
        }

        results.truncate(opts.top_k);

        // Mark all as hybrid source
        for r in &mut results {
            r.source = "hybrid".to_string();
        }

        Ok(results)
    }
}

// ---------------------------------------------------------------------------
// RetrieverBuilder
// ---------------------------------------------------------------------------

/// 检索器构建器
///
/// 【领域含义】Retriever 的构建器，采用 Builder 模式组装混合检索器的各个组件。
/// 必需字段：db_path（SQLite 索引数据库路径）和 api_key（Qwen3 API 密钥）。
#[derive(Default)]
pub struct RetrieverBuilder {
    db_path: Option<String>,
    api_base_url: Option<String>,
    api_key: Option<String>,
    embedding_model: Option<String>,
    reranker_model: Option<String>,
    graph: Option<Arc<CallGraph>>,
}

impl RetrieverBuilder {
    /// 设置数据库路径
    ///
    /// 【领域含义】设置 SQLite 索引数据库的路径（必需字段）。
    pub fn db_path(mut self, path: &str) -> Self {
        self.db_path = Some(path.to_string());
        self
    }

    /// API 基础 URL。
    ///
    /// 设置 API 端点的基础 URL。默认值为本地开发端点。
    pub fn api_base_url(mut self, url: &str) -> Self {
        self.api_base_url = Some(url.to_string());
        self
    }

    /// 设置 API 密钥
    ///
    /// 【领域含义】设置 Qwen3 API 认证密钥（必需字段）。
    pub fn api_key(mut self, key: &str) -> Self {
        self.api_key = Some(key.to_string());
        self
    }

    /// 设置嵌入模型名称
    ///
    /// 【领域含义】设置向量嵌入模型名称。
    pub fn embedding_model(mut self, model: &str) -> Self {
        self.embedding_model = Some(model.to_string());
        self
    }

    /// 设置重排序模型名称
    ///
    /// 【领域含义】设置重排序模型名称。
    pub fn reranker_model(mut self, model: &str) -> Self {
        self.reranker_model = Some(model.to_string());
        self
    }

    /// 注入调用图
    ///
    /// 【领域含义】注入预构建的调用图，用于基于中心性的检索结果重排序。
    pub fn graph(mut self, graph: Arc<CallGraph>) -> Self {
        self.graph = Some(graph);
        self
    }

    /// 构建检索器
    ///
    /// 【领域含义】根据构建器配置创建 Retriever 实例。如果缺少必需字段（db_path、api_key）
    /// 则返回 RetrievalError::Other。
    pub fn build(self) -> RetrievalResult<Retriever> {
        let db_path = self
            .db_path
            .ok_or_else(|| RetrievalError::Other("db_path is required".into()))?;
        let api_key = self
            .api_key
            .ok_or_else(|| RetrievalError::Other("api_key is required".into()))?;
        let api_base_url = self
            .api_base_url
            .unwrap_or_default();
        let embedding_model = self
            .embedding_model
            .unwrap_or_default();
        let reranker_model = self
            .reranker_model
            .unwrap_or_default();

        let conn = rusqlite::Connection::open(&db_path)?;
        let bm25 = Bm25Searcher::new(conn)?;
        bm25.create_fts_index()?;

        let vector = VectorSearcher::new(&api_base_url, &api_key, &embedding_model);
        let reranker = Qwen3Reranker::new(&api_base_url, &api_key, &reranker_model);
        let fusion = RrfFusion::default();

        Ok(Retriever {
            bm25,
            vector,
            reranker,
            fusion,
            graph: self.graph,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_options_defaults() {
        let opts = SearchOptions::default();
        assert_eq!(opts.top_k, 10);
        assert!(opts.languages.is_none());
        assert!(opts.use_reranker);
        assert!(opts.use_graph);
    }

    #[test]
    fn test_scored_result_from_symbol() {
        let sym = crate::indexer::symbol::SymbolEntry {
            symbol_name: "test_func".to_string(),
            symbol_kind: crate::indexer::symbol::SymbolKind::Function,
            file_path: "src/test.rs".to_string(),
            line_range: 10..20,
            column_range: 0..4,
            doc_comment: Some("Test function".to_string()),
            language: "rust".to_string(),
            signature: Some("fn test_func()".to_string()),
        };
        let result = ScoredResult::from_symbol(&sym, 0.85, "bm25");
        assert_eq!(result.symbol_name, "test_func");
        assert_eq!(result.symbol_kind, "function");
        assert_eq!(result.file_path, "src/test.rs");
        assert_eq!(result.score, 0.85);
        assert_eq!(result.source, "bm25");
        assert_eq!(result.line_range, 10..20);
    }

    #[test]
    fn test_retriever_builder_missing_db_path() {
        let result = Retriever::builder()
            .api_key("sk-test")
            .build();
        assert!(result.is_err());
    }

    #[test]
    fn test_retriever_builder_missing_api_key() {
        let result = Retriever::builder()
            .db_path("/tmp/test.db")
            .build();
        assert!(result.is_err());
    }
}
