//! CraftCoder example: web search via DuckDuckGo Instant Answer API (no API key).
//! Register: registry.register(Arc::new(WebSearchTool::new()))?;

use async_trait::async_trait;
use code_agent_core::tools::{optional_u64, require_string, tool_success, Tool, ToolError};
use code_agent_protocol::{CapabilityLevel, ToolResultMessage};
use reqwest::Client;

pub struct WebSearchTool { client: Client }

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("CraftCoder-Example/0.1")
                .build()
                .expect("failed to create HTTP client"),
        }
    }
}

impl Default for WebSearchTool {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str { "web_search" }

    fn description(&self) -> &str {
        "Search the web using DuckDuckGo Instant Answer API. \
         Returns abstract and related topics. No API key needed."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "The search query" },
                "max_results": { "type": "integer", "description": "Max results (default: 5, max: 10)" }
            },
            "required": ["query"]
        })
    }

    fn capability(&self) -> CapabilityLevel { CapabilityLevel::Read }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let query = require_string(&params, "query")?;
        let max_results = optional_u64(&params, "max_results").unwrap_or(5).min(10);

        let url = format!(
            "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
            urlencoding::encode(&query)
        );

        let response = self.client.get(&url).send().await.map_err(|e| {
            ToolError::execution_error(format!("HTTP request failed: {e}"))
        })?;

        if !response.status().is_success() {
            return Err(ToolError::execution_error(format!(
                "HTTP error: {}", response.status()
            )));
        }

        let body: serde_json::Value = response.json().await.map_err(|e| {
            ToolError::execution_error(format!("Failed to parse JSON: {e}"))
        })?;

        let mut output = format!("Search results for: {query}\n\n");
        if let Some(text) = body.get("AbstractText").and_then(|v| v.as_str()) {
            if !text.is_empty() { output.push_str(&format!("{text}\n")); }
        }
        if let Some(src) = body.get("AbstractURL").and_then(|v| v.as_str()) {
            if !src.is_empty() { output.push_str(&format!("Source: {src}\n\n")); }
        }
        if let Some(topics) = body.get("RelatedTopics").and_then(|v| v.as_array()) {
            let mut count: u64 = 0;
            for topic in topics {
                if count >= max_results { break; }
                if let Some(text) = topic.get("Text").and_then(|v| v.as_str()) {
                    count += 1;
                    output.push_str(&format!("{count}. {text}\n"));
                    if let Some(link) = topic.get("FirstURL").and_then(|v| v.as_str()) {
                        output.push_str(&format!("   {link}\n"));
                    }
                }
            }
        }
        Ok(tool_success("", output.trim_end().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_query_returns_error() {
        let err = WebSearchTool::new().execute(serde_json::json!({})).await.unwrap_err();
        assert!(err.to_string().contains("missing required field"));
    }

    #[tokio::test]
    async fn live_search_returns_result() {
        let r = WebSearchTool::new()
            .execute(serde_json::json!({"query": "Rust programming language"}))
            .await.unwrap();
        assert!(r.is_success() && r.output.unwrap().contains("Rust"));
    }

    #[tokio::test]
    async fn max_results_capped() {
        assert!(WebSearchTool::new()
            .execute(serde_json::json!({"query": "test", "max_results": 999}))
            .await.is_ok());
    }

    #[test]
    fn metadata() {
        let t = WebSearchTool::new();
        assert_eq!(t.name(), "web_search");
        assert!(!t.description().is_empty());
        assert_eq!(t.capability(), CapabilityLevel::Read);
    }
}
