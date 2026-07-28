//! Vector semantic search via the configured embedding model.
//!
//! Indexes code symbols as dense vectors and retrieves the most similar ones
//! using cosine similarity against query embeddings.

use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use super::{RetrievalError, RetrievalResult, ScoredResult};
use crate::indexer::symbol::SymbolEntry;

// ---------------------------------------------------------------------------
// VectorStorage trait
// ---------------------------------------------------------------------------

/// 向量存储接口（仓储接口）
///
/// 【领域含义】向量嵌入的存储和查询接口，定义仓储契约。方法为同步的，
/// 调用方应在需要时通过 tokio::task::spawn_blocking 包装。
/// 属于"代码检索"限界上下文中的基础设施接口。
pub trait VectorStorage: Send + Sync {
    /// 存储符号的嵌入向量
    ///
    /// 【领域含义】将符号的嵌入向量持久化到存储中。
    fn store_embedding(&self, symbol_id: i64, embedding: &[f32]) -> RetrievalResult<()>;

    /// 获取所有嵌入向量
    ///
    /// 【领域含义】检索所有已存储的 (symbol_id, embedding) 对，用于全量相似度计算。
    fn get_all_embeddings(&self) -> RetrievalResult<Vec<(i64, Vec<f32>)>>;

    /// 删除符号的嵌入向量
    ///
    /// 【领域含义】从存储中删除指定符号的嵌入向量。
    fn delete_embedding(&self, symbol_id: i64) -> RetrievalResult<()>;

    /// 获取嵌入向量总数
    ///
    /// 【领域含义】返回存储中嵌入向量的总数量。
    fn embedding_count(&self) -> RetrievalResult<usize>;
}

// ---------------------------------------------------------------------------
// SqliteVectorStorage
// ---------------------------------------------------------------------------

/// SQLite 向量存储（仓储实现）
///
/// 【领域含义】基于 SQLite 的向量嵌入存储实现。嵌入向量序列化为小端序 f32 字节的 BLOB。
/// 内部 Connection 通过 Mutex 包装以保证线程安全。
pub struct SqliteVectorStorage {
    conn: Mutex<Connection>,
}

impl SqliteVectorStorage {
    /// 创建内存向量存储
    ///
    /// 【领域含义】创建基于内存 SQLite 的向量存储，主要用于测试场景。
    pub fn new_in_memory() -> RetrievalResult<Self> {
        let conn = Connection::open_in_memory()?;
        let storage = Self {
            conn: Mutex::new(conn),
        };
        storage.init_schema()?;
        Ok(storage)
    }

    /// 创建文件型向量存储
    ///
    /// 【领域含义】创建基于文件 SQLite 的向量存储，数据持久化到磁盘。
    pub fn new(db_path: &str) -> RetrievalResult<Self> {
        let conn = Connection::open(db_path)?;
        let storage = Self {
            conn: Mutex::new(conn),
        };
        storage.init_schema()?;
        Ok(storage)
    }

    fn init_schema(&self) -> RetrievalResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vector_embeddings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                symbol_id INTEGER NOT NULL,
                embedding BLOB NOT NULL,
                dimensionality INTEGER NOT NULL,
                created_at TEXT DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_vector_symbol ON vector_embeddings(symbol_id);",
        )?;
        Ok(())
    }

    /// 编码嵌入向量为字节
    ///
    /// 【领域含义】将 Vec<f32> 序列化为小端序字节数组，用于 SQLite BLOB 存储。
    pub fn encode_embedding(vec: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(vec.len() * 4);
        for &f in vec {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        bytes
    }

    /// 解码字节为嵌入向量
    ///
    /// 【领域含义】将小端序字节数组反序列化为 Vec<f32>，用于从 SQLite BLOB 读取嵌入向量。
    pub fn decode_embedding(bytes: &[u8]) -> Vec<f32> {
        bytes
            .chunks_exact(4)
            .map(|chunk| {
                let arr: [u8; 4] = chunk.try_into().unwrap();
                f32::from_le_bytes(arr)
            })
            .collect()
    }
}

impl VectorStorage for SqliteVectorStorage {
    fn store_embedding(&self, symbol_id: i64, embedding: &[f32]) -> RetrievalResult<()> {
        let blob = Self::encode_embedding(embedding);
        let dim = embedding.len() as i64;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO vector_embeddings (symbol_id, embedding, dimensionality)
             VALUES (?1, ?2, ?3)",
            params![symbol_id, blob, dim],
        )?;
        Ok(())
    }

    fn get_all_embeddings(&self) -> RetrievalResult<Vec<(i64, Vec<f32>)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT symbol_id, embedding FROM vector_embeddings",
        )?;
        let rows = stmt.query_map([], |row| {
            let symbol_id: i64 = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            Ok((symbol_id, Self::decode_embedding(&blob)))
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    fn delete_embedding(&self, symbol_id: i64) -> RetrievalResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM vector_embeddings WHERE symbol_id = ?1",
            params![symbol_id],
        )?;
        Ok(())
    }

    fn embedding_count(&self) -> RetrievalResult<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM vector_embeddings",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }
}

// ---------------------------------------------------------------------------
// EmbeddingClient trait (local, decoupled from core)
// ---------------------------------------------------------------------------

/// 嵌入模型客户端接口
///
/// 【领域含义】嵌入模型客户端的抽象接口，定义生成文本嵌入向量的契约。
/// 支持批量嵌入和单查询嵌入两种模式。
#[async_trait::async_trait]
pub trait EmbeddingClient: Send + Sync {
    /// 批量生成文本嵌入
    ///
    /// 【领域含义】为多个文本生成嵌入向量，返回对应的向量列表。
    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError>;

    /// 生成单个查询嵌入
    ///
    /// 【领域含义】为单个查询文本生成嵌入向量，默认实现委托给 embed 方法。
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let mut results = self.embed(&[text]).await?;
        results
            .pop()
            .ok_or_else(|| EmbedError::EmptyResult)
    }
}

/// 嵌入操作错误
///
/// 【领域含义】嵌入向量生成过程中可能出现的错误类型，涵盖 HTTP 错误、序列化错误、
/// API 错误、空结果等。
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("API error {status}: {message}")]
    Api { status: u16, message: String },
    #[error("Empty embedding result")]
    EmptyResult,
    #[error("{0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// DirectEmbeddingClient
// ---------------------------------------------------------------------------

/// Direct HTTP client for the Qwen3 embedding API.
struct DirectEmbeddingClient {
    http: reqwest::Client,
    api_base_url: String,
    api_key: String,
    model: String,
}

impl DirectEmbeddingClient {
    fn new(api_base_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
            api_base_url: api_base_url.to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
        }
    }
}

#[derive(Serialize)]
struct EmbeddingRequest {
    model: String,
    input: serde_json::Value,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

#[async_trait::async_trait]
impl EmbeddingClient for DirectEmbeddingClient {
    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        let url = format!(
            "{}/embeddings",
            self.api_base_url.trim_end_matches('/')
        );

        let input = if texts.len() == 1 {
            serde_json::json!(texts[0])
        } else {
            serde_json::json!(texts)
        };

        let request = EmbeddingRequest {
            model: self.model.clone(),
            input,
        };

        let response = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(EmbedError::Api {
                status,
                message: body,
            });
        }

        let body: EmbeddingResponse = response.json().await?;
        let embeddings: Vec<Vec<f32>> = body.data.into_iter().map(|d| d.embedding).collect();

        Ok(embeddings)
    }
}

// ---------------------------------------------------------------------------
// VectorSearcher
// ---------------------------------------------------------------------------

/// 向量搜索器（领域服务）
///
/// 【领域含义】基于配置的嵌入模型的语义代码搜索领域服务。将代码符号索引为
/// 稠密向量，通过余弦相似度检索与查询语义最匹配的符号。属于"代码检索"限界上下文
/// 中的语义检索组件。
pub struct VectorSearcher {
    embedding_client: Arc<dyn EmbeddingClient>,
    storage: Arc<dyn VectorStorage>,
}

impl VectorSearcher {
    /// 创建向量搜索器
    ///
    /// 【领域含义】使用配置的嵌入模型 API 直接创建向量搜索器，附带内存向量存储。
    pub fn new(api_base_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            embedding_client: Arc::new(DirectEmbeddingClient::new(api_base_url, api_key, model)),
            storage: Arc::new(
                SqliteVectorStorage::new_in_memory().expect("in-memory SQLite"),
            ),
        }
    }

    /// 使用自定义存储创建向量搜索器
    ///
    /// 【领域含义】使用自定义 VectorStorage 实现（如文件型 SQLite）创建向量搜索器。
    #[allow(dead_code)]
    pub fn with_storage(
        api_base_url: &str,
        api_key: &str,
        model: &str,
        storage: Arc<dyn VectorStorage>,
    ) -> Self {
        Self {
            embedding_client: Arc::new(DirectEmbeddingClient::new(api_base_url, api_key, model)),
            storage,
        }
    }

    /// 使用自定义嵌入客户端创建向量搜索器
    ///
    /// 【领域含义】使用自定义 EmbeddingClient 实现创建向量搜索器，主要用于测试。
    #[allow(dead_code)]
    pub fn with_client(
        embedding_client: Arc<dyn EmbeddingClient>,
        storage: Arc<dyn VectorStorage>,
    ) -> Self {
        Self {
            embedding_client,
            storage,
        }
    }

    /// 语义搜索
    ///
    /// 【领域含义】搜索与查询语义相似的符号。先生成查询嵌入向量，然后与所有已索引的
    /// 符号嵌入计算余弦相似度，返回 top_k 个最相似的结果。
    pub async fn search(&self, query: &str, top_k: usize) -> RetrievalResult<Vec<ScoredResult>> {
        let query_embedding = self
            .embedding_client
            .embed_query(query)
            .await
            .map_err(|e| RetrievalError::Vector(e.to_string()))?;

        // get_all_embeddings is sync; run in spawn_blocking to avoid blocking
        let storage = Arc::clone(&self.storage);
        let all_embeddings = tokio::task::spawn_blocking(move || storage.get_all_embeddings())
            .await
            .map_err(|e| RetrievalError::Vector(format!("Spawn error: {}", e)))?
            .map_err(|e| RetrievalError::Vector(format!("Storage error: {}", e)))?;

        if all_embeddings.is_empty() {
            return Ok(Vec::new());
        }

        // Cosine similarity ranking
        let mut scored: Vec<(i64, f64)> = all_embeddings
            .iter()
            .map(|(symbol_id, emb)| {
                let sim = cosine_similarity(&query_embedding, emb);
                (*symbol_id, sim)
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);

        let results: Vec<ScoredResult> = scored
            .into_iter()
            .map(|(symbol_id, score)| ScoredResult {
                symbol_name: format!("symbol_{}", symbol_id),
                symbol_kind: "unknown".to_string(),
                file_path: String::new(),
                line_range: 0..0,
                score,
                source: "vector".to_string(),
            })
            .collect();

        Ok(results)
    }

    /// 索引符号的嵌入向量
    ///
    /// 【领域含义】为符号生成嵌入向量并存储到向量存储中。使用符号名称和文档注释
    /// 作为嵌入文本。
    pub async fn index_symbol(&self, symbol: &SymbolEntry) -> RetrievalResult<()> {
        let text = if let Some(ref doc) = symbol.doc_comment {
            format!("{}: {}", symbol.symbol_name, doc)
        } else {
            symbol.symbol_name.clone()
        };

        let embeddings = self
            .embedding_client
            .embed(&[&text])
            .await
            .map_err(|e| RetrievalError::Vector(e.to_string()))?;

        if let Some(embedding) = embeddings.into_iter().next() {
            let symbol_id = fxhash(&symbol.symbol_name, &symbol.file_path);
            let storage = Arc::clone(&self.storage);
            tokio::task::spawn_blocking(move || storage.store_embedding(symbol_id, &embedding))
                .await
                .map_err(|e| RetrievalError::Vector(format!("Spawn error: {}", e)))?
                .map_err(|e| RetrievalError::Vector(format!("Storage error: {}", e)))?;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute the cosine similarity between two f32 vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }
    if a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;

    for i in 0..a.len() {
        let va = a[i] as f64;
        let vb = b[i] as f64;
        dot += va * vb;
        norm_a += va * va;
        norm_b += vb * vb;
    }

    let denom = (norm_a * norm_b).sqrt();
    if denom < 1e-12 {
        return 0.0;
    }

    dot / denom
}

/// Simple non-cryptographic hash for generating synthetic symbol IDs.
fn fxhash(name: &str, file_path: &str) -> i64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    file_path.hash(&mut hasher);
    (hasher.finish() % (i64::MAX as u64)) as i64
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A mock embedding client that returns fixed vectors.
    struct MockEmbeddingClient;

    #[async_trait::async_trait]
    impl EmbeddingClient for MockEmbeddingClient {
        async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
            Ok(texts
                .iter()
                .map(|t| {
                    let mut v = vec![0.0f32; 4];
                    for (i, b) in t.bytes().enumerate().take(4) {
                        v[i] = (b as f32) / 255.0;
                    }
                    v
                })
                .collect())
        }
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_empty() {
        let sim = cosine_similarity(&[], &[]);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn test_cosine_similarity_different_lengths() {
        let sim = cosine_similarity(&[1.0], &[1.0, 2.0]);
        assert_eq!(sim, 0.0);
    }

    #[tokio::test]
    async fn test_vector_search_with_mock() {
        let storage = Arc::new(SqliteVectorStorage::new_in_memory().unwrap());

        storage.store_embedding(1, &[0.9, 0.1, 0.0, 0.0]).unwrap();
        storage.store_embedding(2, &[0.1, 0.9, 0.0, 0.0]).unwrap();
        storage.store_embedding(3, &[0.0, 0.0, 0.9, 0.1]).unwrap();

        let client = Arc::new(MockEmbeddingClient);
        let searcher = VectorSearcher::with_client(client, storage);

        let results = searcher.search("test", 3).await.unwrap();
        assert_eq!(results.len(), 3);
        assert!(results[0].score >= results[1].score);
        assert_eq!(results[0].source, "vector");
    }

    #[tokio::test]
    async fn test_vector_search_empty_storage() {
        let storage = Arc::new(SqliteVectorStorage::new_in_memory().unwrap());
        let client = Arc::new(MockEmbeddingClient);
        let searcher = VectorSearcher::with_client(client, storage);

        let results = searcher.search("test", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_encode_decode_embedding() {
        let original = vec![0.1f32, 0.5, -0.3, 1.0, 0.0];
        let encoded = SqliteVectorStorage::encode_embedding(&original);
        let decoded = SqliteVectorStorage::decode_embedding(&encoded);
        assert_eq!(original.len(), decoded.len());
        for (a, b) in original.iter().zip(decoded.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn test_sqlite_vector_storage_crud() {
        let storage = SqliteVectorStorage::new_in_memory().unwrap();

        assert_eq!(storage.embedding_count().unwrap(), 0);

        storage.store_embedding(42, &[0.1, 0.2, 0.3]).unwrap();
        assert_eq!(storage.embedding_count().unwrap(), 1);

        let all = storage.get_all_embeddings().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, 42);
        assert_eq!(all[0].1.len(), 3);

        storage.delete_embedding(42).unwrap();
        assert_eq!(storage.embedding_count().unwrap(), 0);
    }

    #[test]
    fn test_fxhash_deterministic() {
        let h1 = fxhash("hello", "world.rs");
        let h2 = fxhash("hello", "world.rs");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_fxhash_different_inputs() {
        let h1 = fxhash("hello", "a.rs");
        let h2 = fxhash("world", "b.rs");
        assert_ne!(h1, h2);
    }
}
