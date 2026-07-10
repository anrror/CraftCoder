//! Embedding client — text-to-vector conversion for code search and retrieval.
//!
//! Uses the configured embedding model via the OpenAI-compatible `/v1/embeddings` endpoint.
//!
//! # 领域描述
//!
//! 嵌入客户端是语义搜索与 RAG（检索增强生成）管道的向量化层。
//! 将代码文本、注释或查询转换为稠密向量，供向量数据库进行相似度检索。
//! 基于配置的嵌入模型，通过 OpenAI 兼容的 /v1/embeddings 端点调用。

use async_trait::async_trait;
use serde_json::json;
use tracing::debug;

use crate::model::config::ModelConfig;
use crate::model::types::EmbeddingRequest;
use crate::model::{build_http_client, ModelError, ModelResult};

// ---------------------------------------------------------------------------
// EmbeddingClient trait
// ---------------------------------------------------------------------------

/// 嵌入模型客户端 trait —— 将文本转换为稠密向量
///
/// 【领域含义】EmbeddingClient 定义了 AI Agent 进行语义向量化的标准接口。
/// 将输入文本（代码、注释、查询语句）转换为定长稠密浮点向量，
/// 用于语义搜索、聚类与 RAG 管道的向量检索阶段。
///
/// Trait for embedding model clients.
///
/// Converts text inputs into dense vector representations for use in
/// semantic search, clustering, and retrieval-augmented generation (RAG).
#[async_trait]
pub trait EmbeddingClient: Send + Sync {
    /// 批量生成文本嵌入向量
    ///
    /// 【领域含义】核心嵌入方法。接收多个文本输入，一次性发送至
    /// 使用配置的嵌入模型，返回与输入顺序一致的向量列表。
    /// 批处理可减少 API 调用次数，提升吞吐量。
    ///
    /// Generate embeddings for multiple texts in a single batch.
    ///
    /// Returns one vector per input text. The vectors preserve input order.
    async fn embed(&self, texts: &[&str]) -> ModelResult<Vec<Vec<f32>>>;

    /// 生成单个查询文本的嵌入向量（便捷方法）
    ///
    /// 【领域含义】封装 embed 方法，为单文本查询提供便捷入口。
    /// 常用于实时搜索场景：将用户查询编码后与向量数据库中的文档向量进行相似度匹配。
    ///
    /// Generate a single embedding for a query string.
    ///
    /// Convenience method that wraps `embed` with a single-element input.
    async fn embed_query(&self, text: &str) -> ModelResult<Vec<f32>> {
        let results = self.embed(&[text]).await?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| ModelError::Other("Empty embedding result".into()))
    }
}

// ---------------------------------------------------------------------------
// Qwen3EmbeddingClient
// ---------------------------------------------------------------------------

/// 嵌入客户端 —— 通过 OpenAI 兼容 API 调用嵌入模型
///
/// 【领域含义】Qwen3EmbeddingClient 是 EmbeddingClient trait 的 Qwen3 实现。
/// 发送 POST {base_url}/embeddings 请求，使用 config 中配置的模型名称，
/// 将返回的 JSON 响应解析为浮点向量数组。
///
/// Embedding client via an OpenAI-compatible API gateway.
///
/// Sends `POST {base_url}/embeddings` with the configured embedding model.
pub struct Qwen3EmbeddingClient {
    config: ModelConfig,
    http: reqwest::Client,
}

impl Qwen3EmbeddingClient {
    /// 创建 Qwen3 嵌入客户端实例
    ///
    /// 【领域含义】从 ModelConfig 中提取嵌入模型名称与 API 端点，
    /// 初始化 HTTP 连接池。
    ///
    /// Create a new embedding client from a [`ModelConfig`].
    pub fn new(config: ModelConfig) -> Self {
        Self {
            config,
            http: build_http_client(),
        }
    }
}

#[async_trait]
impl EmbeddingClient for Qwen3EmbeddingClient {
    async fn embed(&self, texts: &[&str]) -> ModelResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        let url = self.config.embeddings_url();
        let input = if texts.len() == 1 {
            json!(texts[0])
        } else {
            json!(texts)
        };

        let request = EmbeddingRequest {
            model: self.config.embedding_model.clone(),
            input,
        };

        debug!(
            url = %url,
            model = %request.model,
            num_texts = texts.len(),
            "Sending embedding request"
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

        let body: crate::model::types::EmbeddingResponse = response.json().await?;
        let embeddings: Vec<Vec<f32>> = body.data.into_iter().map(|d| d.embedding).collect();

        Ok(embeddings)
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
            .embedding_model("test-embed-model".into())
            .build()
            .expect("test config")
    }

    #[tokio::test]
    async fn embed_single_text() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/embeddings")
                .header("Authorization", "Bearer sk-test");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(r#"{
                    "object": "list",
                    "data": [{
                        "object": "embedding",
                        "index": 0,
                        "embedding": [0.1, 0.2, 0.3]
                    }],
                    "model": "test-embed-model",
                    "usage": {"prompt_tokens": 3, "total_tokens": 3}
                }"#);
        });

        let config = test_config(&server);
        let client = Qwen3EmbeddingClient::new(config);

        let result = client.embed(&["hello world"]).await.expect("embed");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 3);
        assert!((result[0][0] - 0.1).abs() < 0.001);
        assert!((result[0][1] - 0.2).abs() < 0.001);
        assert!((result[0][2] - 0.3).abs() < 0.001);

        mock.assert();
    }

    #[tokio::test]
    async fn embed_multiple_texts() {
        let server = MockServer::start();

        let _mock = server.mock(|when, then| {
            when.method(POST).path("/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(r#"{
                    "object": "list",
                    "data": [
                        {"object": "embedding", "index": 0, "embedding": [0.1, 0.2]},
                        {"object": "embedding", "index": 1, "embedding": [0.3, 0.4]}
                    ],
                    "model": "test-embed-model",
                    "usage": {"prompt_tokens": 6, "total_tokens": 6}
                }"#);
        });

        let config = test_config(&server);
        let client = Qwen3EmbeddingClient::new(config);

        let result = client.embed(&["hello", "world"]).await.expect("embed");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], vec![0.1, 0.2]);
        assert_eq!(result[1], vec![0.3, 0.4]);
    }

    #[tokio::test]
    async fn embed_query_wraps_single() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST).path("/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(r#"{
                    "object": "list",
                    "data": [{"object": "embedding", "index": 0, "embedding": [0.5, 0.6]}],
                    "model": "test-embed-model",
                    "usage": {"prompt_tokens": 2, "total_tokens": 2}
                }"#);
        });

        let config = test_config(&server);
        let client = Qwen3EmbeddingClient::new(config);

        let vec = client.embed_query("search query").await.expect("embed");
        assert_eq!(vec, vec![0.5, 0.6]);
        mock.assert();
    }

    #[tokio::test]
    async fn embed_empty_inputs() {
        let server = MockServer::start();
        let config = test_config(&server);
        let client = Qwen3EmbeddingClient::new(config);

        let result = client.embed(&[]).await.expect("embed");
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn embed_handles_api_error() {
        let server = MockServer::start();

        let _mock = server.mock(|when, then| {
            when.method(POST).path("/embeddings");
            then.status(500)
                .header("Content-Type", "application/json")
                .body(r#"{"error": "Internal server error"}"#);
        });

        let config = test_config(&server);
        let client = Qwen3EmbeddingClient::new(config);

        let result = client.embed(&["test"]).await;
        assert!(result.is_err());
    }
}
