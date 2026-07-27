//! Write file tool.
//!
//! Creates or overwrites a file with the given content. Parent directories
//! are created automatically if they don't exist.

use async_trait::async_trait;
use code_agent_protocol::CapabilityLevel;

use crate::tools::{require_string, tool_success, Tool, ToolError, ToolResultMessage};

/// Writes content to a file, creating parent directories as needed.
///
/// Input schema:
/// ```json
/// {
///   "path": "string (required) – path to the file to write",
///   "content": "string (required) – the content to write"
/// }
/// ```
pub struct WriteFileTool;

impl Default for WriteFileTool {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file. Creates parent directories if they don't exist. \
         Overwrites the file if it already exists."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn capability(&self) -> CapabilityLevel {
        CapabilityLevel::Edit
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let path_str = require_string(&params, "path")?;
        let content = require_string(&params, "content")?;
        let canonical_path = crate::tools::resolve_safe_path_create(&path_str)?;

        // Create parent directories if they don't exist
        if let Some(parent) = canonical_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        std::fs::write(&canonical_path, &content)?;

        let byte_count = content.len();
        let line_count = content.lines().count();
        Ok(tool_success(
            "",
            format!(
                "Wrote {byte_count} bytes ({line_count} lines) to {path_str}"
            ),
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_file_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("output.txt");
        let tool = WriteFileTool::default();

        let result = tool
            .execute(serde_json::json!({
                "path": file_path.to_str().unwrap(),
                "content": "hello world"
            }))
            .await
            .unwrap();

        assert!(result.is_success());
        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "hello world");
    }

    #[tokio::test]
    async fn write_file_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("sub").join("deep").join("nested.txt");
        let tool = WriteFileTool::default();

        let result = tool
            .execute(serde_json::json!({
                "path": file_path.to_str().unwrap(),
                "content": "nested content"
            }))
            .await
            .unwrap();

        assert!(result.is_success());
        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "nested content");
    }

    #[tokio::test]
    async fn write_file_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("overwrite.txt");
        std::fs::write(&file_path, "original").unwrap();

        let tool = WriteFileTool::default();
        tool.execute(serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "content": "replaced"
        }))
        .await
        .unwrap();

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "replaced");
    }

    #[tokio::test]
    async fn write_file_missing_content_param() {
        let tool = WriteFileTool::default();
        let err = tool
            .execute(serde_json::json!({"path": "/tmp/test.txt"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing required field"));
    }

    #[tokio::test]
    async fn capability_is_edit() {
        assert_eq!(WriteFileTool::default().capability(), CapabilityLevel::Edit);
    }
}
