//! CraftCoder example: fetch web page content via HTTP GET.
//! Register: registry.register(Arc::new(WebFetchTool::new()))?;

use async_trait::async_trait;
use code_agent_core::tools::{require_string, tool_error, tool_success, Tool, ToolError};
use code_agent_protocol::{CapabilityLevel, ToolResultMessage};
use reqwest::Client;

const MAX_CONTENT_CHARS: usize = 20_000;

pub struct WebFetchTool { client: Client }

impl WebFetchTool {
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

impl Default for WebFetchTool {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str { "web_fetch" }

    fn description(&self) -> &str {
        "Fetch web page content via HTTP GET. \
         Returns up to 20,000 characters of raw content."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "URL to fetch (http:// or https://)" }
            },
            "required": ["url"]
        })
    }

    fn capability(&self) -> CapabilityLevel { CapabilityLevel::Read }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let url = require_string(&params, "url")?;

        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(ToolError::invalid_input("url must start with http:// or https://"));
        }

        let response = self.client.get(&url)
            .header("Accept", "text/html, text/plain, */*")
            .send().await.map_err(|e| {
                let msg = if e.is_timeout() {
                    format!("Request timed out: {url}")
                } else if e.is_connect() {
                    format!("Failed to connect: {url}")
                } else {
                    format!("HTTP request failed: {e}")
                };
                ToolError::execution_error(msg)
            })?;

        let status = response.status();
        if !status.is_success() {
            return Ok(tool_error("", format!("HTTP {status}: failed to fetch {url}")));
        }

        let full = response.text().await.map_err(|e| {
            ToolError::execution_error(format!("Failed to read body: {e}"))
        })?;

        let content = if full.chars().count() > MAX_CONTENT_CHARS {
            let truncated: String = full.chars().take(MAX_CONTENT_CHARS).collect();
            format!("{truncated}\n\n[... truncated after {MAX_CONTENT_CHARS} chars ...]")
        } else { full };

        Ok(tool_success("", format!("URL: {url}\nStatus: {status}\n\n{content}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_url_returns_error() {
        let err = WebFetchTool::new().execute(serde_json::json!({})).await.unwrap_err();
        assert!(err.to_string().contains("missing required field"));
    }

    #[tokio::test]
    async fn invalid_scheme_rejected() {
        let err = WebFetchTool::new()
            .execute(serde_json::json!({"url": "ftp://example.com"}))
            .await.unwrap_err();
        assert!(err.to_string().contains("http://"));
    }

    #[tokio::test]
    async fn fetch_valid_url_succeeds() {
        let r = WebFetchTool::new()
            .execute(serde_json::json!({"url": "https://httpbin.org/ip"}))
            .await.unwrap();
        assert!(r.is_success() && r.output.unwrap().contains("Status: 200"));
    }

    #[tokio::test]
    async fn http_404_returns_error_result() {
        let r = WebFetchTool::new()
            .execute(serde_json::json!({"url": "https://httpbin.org/status/404"}))
            .await.unwrap();
        assert!(r.is_error() && r.error.unwrap().contains("404"));
    }

    #[test]
    fn metadata() {
        let t = WebFetchTool::new();
        assert_eq!(t.name(), "web_fetch");
        assert!(!t.description().is_empty());
        assert_eq!(t.capability(), CapabilityLevel::Read);
    }
}
