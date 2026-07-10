//! 重排序客户端 —— 按查询相关性重排搜索结果
//!
//! 【领域含义】使用配置的重排序模型通过自定义 `/v1/rerank`
//! 端点对候选文档按查询相关性重新排序。
//!
//! Uses the configured reranker model via the custom `/v1/rerank` endpoint.

use async_trait::async_trait;
use tracing::debug;

use crate::model::config::ModelConfig;
use crate::model::types::RerankRequest;
use crate::model::{build_http_client, ModelError, ModelResult};

// ---------------------------------------------------------------------------
// RerankerClient trait
// ---------------------------------------------------------------------------

/// 重排序模型客户端 —— 文档相关性排序的领域接口
///
/// 【领域含义】给定一个查询和一组候选文档，返回按相关性排序的文档列表
/// （最相关在前），附带相关性分数。
#[async_trait]
pub trait RerankerClient: Send + Sync {
    /// 按与给定查询的相关性重排文档
    ///
    /// 返回 `(document_index, relevance_score)` 对，按相关性降序排列。
    /// 分数通常在 [0.0, 1.0] 范围内，但取决于模型。
    async fn rerank(&self, query: &str, documents: &[&str]) -> ModelResult<Vec<(usize, f32)>>;
}

// ---------------------------------------------------------------------------
// Qwen3RerankerClient
// ---------------------------------------------------------------------------

/// 重排序客户端
///
/// 【领域含义】发送 `POST {base_url}/rerank`，
/// 携带 config 中配置的重排序模型名称。
pub struct Qwen3RerankerClient {
    config: ModelConfig,
    http: reqwest::Client,
}

impl Qwen3RerankerClient {
    /// 从 `ModelConfig` 创建新的重排序客户端
    pub fn new(config: ModelConfig) -> Self {
        Self {
            config,
            http: build_http_client(),
        }
    }
}

#[async_trait]
impl RerankerClient for Qwen3RerankerClient {
    async fn rerank(&self, query: &str, documents: &[&str]) -> ModelResult<Vec<(usize, f32)>> {
        if documents.is_empty() {
            return Ok(vec![]);
        }

        let url = self.config.rerank_url();
        let request = RerankRequest {
            model: self.config.reranker_model.clone(),
            query: query.to_string(),
            documents: documents.iter().map(|s| s.to_string()).collect(),
        };

        debug!(
            url = %url,
            model = %request.model,
            num_docs = documents.len(),
            "Sending rerank request"
        );

        let response = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(ModelError::Api {
                status,
                message: body,
            });
        }

        let body: crate::model::types::RerankResponse = response.json().await?;
        let results: Vec<(usize, f32)> = body
            .results
            .into_iter()
            .map(|r| (r.index, r.relevance_score))
            .collect();

        Ok(results)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn test_config(server: &MockServer) -> ModelConfig {
        ModelConfig::builder()
            .api_key("sk-test".into())
            .api_base_url(server.base_url())
            .model("test-chat-model".into())
            .reranker_model("test-rerank-model".into())
            .build()
            .expect("test config")
    }

    #[tokio::test]
    async fn rerank_returns_sorted_results() {
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

        let config = test_config(&server);
        let client = Qwen3RerankerClient::new(config);

        let docs = &["doc A", "doc B", "doc C"];
        let result = client.rerank("query", docs).await.expect("rerank");

        assert_eq!(result.len(), 3);
        assert_eq!(result[0], (2, 0.95));
        assert_eq!(result[1], (0, 0.72));
        assert_eq!(result[2], (1, 0.31));

        mock.assert();
    }

    #[tokio::test]
    async fn rerank_empty_documents() {
        let server = MockServer::start();
        let config = test_config(&server);
        let client = Qwen3RerankerClient::new(config);

        let result = client.rerank("query", &[]).await.expect("rerank");
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn rerank_single_document() {
        let server = MockServer::start();

        let _mock = server.mock(|when, then| {
            when.method(POST).path("/rerank");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(r#"{
                    "model": "test-rerank-model",
                    "results": [{"index": 0, "relevance_score": 0.88}]
                }"#);
        });

        let config = test_config(&server);
        let client = Qwen3RerankerClient::new(config);

        let result = client.rerank("query", &["doc"]).await.expect("rerank");
        assert_eq!(result, vec![(0, 0.88)]);
    }

    #[tokio::test]
    async fn rerank_handles_api_error() {
        let server = MockServer::start();

        let _mock = server.mock(|when, then| {
            when.method(POST).path("/rerank");
            then.status(400)
                .header("Content-Type", "application/json")
                .body(r#"{"error": "Invalid request"}"#);
        });

        let config = test_config(&server);
        let client = Qwen3RerankerClient::new(config);

        let result = client.rerank("query", &["doc"]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rerank_request_includes_model() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/rerank")
                .json_body_partial(
                    r#"{"model": "test-rerank-model"}"#,
                );
            then.status(200)
                .header("Content-Type", "application/json")
                .body(r#"{"model": "test-rerank-model", "results": []}"#);
        });

        let config = test_config(&server);
        let client = Qwen3RerankerClient::new(config);

        let _ = client.rerank("query", &["doc"]).await.expect("rerank");
        mock.assert();
    }
}
