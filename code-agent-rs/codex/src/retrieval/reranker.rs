//! Re-ranker client — semantic re-ranking via the configured reranker model.
//!
//! After BM25+vector fusion produces a candidate set, the reranker scores
//! each candidate against the original query to produce a final ranked list.

use serde::{Deserialize, Serialize};

use super::{RetrievalError, RetrievalResult, ScoredResult};

/// Qwen3 重排序器（领域服务）
///
/// 【领域含义】基于配置的重排序模型的语义重排序客户端，通过自定义 `/v1/rerank` 端点
/// 对候选文档进行相关性评分。在 BM25+向量融合产生候选集后，重排序器对每个候选与
/// 原始查询的相关性进行评分，生成最终的排序列表。属于"代码检索"限界上下文中的
/// 重排序组件。
pub struct Qwen3Reranker {
    client: reqwest::Client,
    api_base_url: String,
    api_key: String,
    model: String,
}

impl Qwen3Reranker {
    /// 创建重排序器客户端
    ///
    /// 【领域含义】创建 Qwen3-Reranker 客户端，配置 API 基础 URL、认证密钥和模型名称。
    pub fn new(api_base_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
            api_base_url: api_base_url.to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
        }
    }

    /// 重排序搜索结果
    ///
    /// 【领域含义】根据与查询的相关性对搜索结果进行重排序。每个结果的符号名称
    /// （附带文件路径上下文）作为文档文本发送给重排序 API。返回按相关性评分
    /// 降序排列的结果列表。
    pub async fn rerank(
        &self,
        query: &str,
        documents: &[ScoredResult],
        top_k: usize,
    ) -> RetrievalResult<Vec<ScoredResult>> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }

        // Build document texts: symbol_name (file_path) + optional context
        let doc_texts: Vec<String> = documents
            .iter()
            .map(|d| {
                format!(
                    "{} [{}:{}] in {}",
                    d.symbol_name, d.symbol_kind, d.file_path, d.source
                )
            })
            .collect();

        let url = format!("{}/rerank", self.api_base_url.trim_end_matches('/'));

        let request = RerankRequest {
            model: self.model.clone(),
            query: query.to_string(),
            documents: doc_texts,
        };

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(RetrievalError::Reranker(format!(
                "API error {}: {}",
                status, body
            )));
        }

        let body: RerankResponse = response.json().await?;

        // Build results with reranker scores
        let mut reranked: Vec<ScoredResult> = body
            .results
            .into_iter()
            .filter_map(|r| {
                documents.get(r.index).map(|doc| {
                    let mut result = doc.clone();
                    result.score = r.relevance_score as f64;
                    result.source = "reranker".to_string();
                    result
                })
            })
            .collect();

        // Sort by descending score
        reranked.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        reranked.truncate(top_k);

        Ok(reranked)
    }
}

// ---------------------------------------------------------------------------
// API types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
struct RerankRequest {
    model: String,
    query: String,
    documents: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct RerankResponse {
    results: Vec<RerankResult>,
}

#[derive(Clone, Debug, Deserialize)]
struct RerankResult {
    index: usize,
    relevance_score: f32,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;
    use std::ops::Range;

    fn make_doc(name: &str, source: &str, score: f64) -> ScoredResult {
        ScoredResult {
            symbol_name: name.to_string(),
            symbol_kind: "function".to_string(),
            file_path: "src/lib.rs".to_string(),
            line_range: Range { start: 1, end: 5 },
            score,
            source: source.to_string(),
        }
    }

    #[tokio::test]
    async fn test_rerank_returns_sorted_results() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/rerank")
                .header("Authorization", "Bearer sk-test");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(r#"{
                    "model": "test-rerank-model",
                    "results": [
                        {"index": 2, "relevance_score": 0.95},
                        {"index": 0, "relevance_score": 0.72},
                        {"index": 1, "relevance_score": 0.31}
                    ]
                }"#);
        });

        let reranker =
            Qwen3Reranker::new(&server.base_url(), "sk-test", "test-rerank-model");

        let docs = vec![
            make_doc("func_a", "bm25", 0.5),
            make_doc("func_b", "vector", 0.7),
            make_doc("func_c", "bm25", 0.3),
        ];

        let results = reranker.rerank("query", &docs, 10).await.unwrap();
        assert_eq!(results.len(), 3);
        // Sorted by relevance_score descending: func_c(0.95) > func_a(0.72) > func_b(0.31)
        assert_eq!(results[0].symbol_name, "func_c");
        assert!((results[0].score - 0.95).abs() < 0.001);
        assert_eq!(results[1].symbol_name, "func_a");
        assert!((results[1].score - 0.72).abs() < 0.001);
        assert_eq!(results[2].symbol_name, "func_b");
        assert!((results[2].score - 0.31).abs() < 0.001);

        mock.assert();
    }

    #[tokio::test]
    async fn test_rerank_empty_documents() {
        let server = MockServer::start();
        let reranker =
            Qwen3Reranker::new(&server.base_url(), "sk-test", "test-rerank-model");

        let results = reranker.rerank("query", &[], 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_rerank_truncates_to_top_k() {
        let server = MockServer::start();

        let _mock = server.mock(|when, then| {
            when.method(POST).path("/rerank");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(r#"{
                    "model": "test-rerank-model",
                    "results": [
                        {"index": 1, "relevance_score": 0.9},
                        {"index": 0, "relevance_score": 0.8},
                        {"index": 2, "relevance_score": 0.7}
                    ]
                }"#);
        });

        let reranker =
            Qwen3Reranker::new(&server.base_url(), "sk-test", "test-rerank-model");

        let docs = vec![
            make_doc("a", "bm25", 0.5),
            make_doc("b", "vector", 0.7),
            make_doc("c", "bm25", 0.3),
        ];

        let results = reranker.rerank("query", &docs, 2).await.unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].symbol_name, "b");
        assert_eq!(results[1].symbol_name, "a");
    }

    #[tokio::test]
    async fn test_rerank_handles_api_error() {
        let server = MockServer::start();

        let _mock = server.mock(|when, then| {
            when.method(POST).path("/rerank");
            then.status(500)
                .header("Content-Type", "application/json")
                .body(r#"{"error": "Internal error"}"#);
        });

        let reranker =
            Qwen3Reranker::new(&server.base_url(), "sk-test", "test-rerank-model");

        let docs = vec![make_doc("test", "bm25", 0.5)];
        let result = reranker.rerank("query", &docs, 10).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_rerank_sets_source_to_reranker() {
        let server = MockServer::start();

        let _mock = server.mock(|when, then| {
            when.method(POST).path("/rerank");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(r#"{
                    "model": "test-rerank-model",
                    "results": [
                        {"index": 0, "relevance_score": 0.88}
                    ]
                }"#);
        });

        let reranker =
            Qwen3Reranker::new(&server.base_url(), "sk-test", "test-rerank-model");

        let docs = vec![make_doc("test", "bm25", 0.5)];
        let results = reranker.rerank("query", &docs, 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source, "reranker");
    }
}
