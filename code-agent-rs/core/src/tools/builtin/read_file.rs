//! Read file contents tool.
//!
//! Reads a file from disk with optional offset/limit controls.
//! Output is capped at 10,000 characters per read.

use async_trait::async_trait;
use code_agent_protocol::CapabilityLevel;

use crate::tools::{require_string, tool_success, optional_u64, Tool, ToolError, ToolResultMessage};

/// Reads file contents from disk.
///
/// Input schema:
/// ```json
/// {
///   "path": "string (required) – path to the file",
///   "offset": "integer (optional) – 0-based line offset",
///   "limit": "integer (optional) – max lines to read"
/// }
/// ```
pub struct ReadFileTool {
    /// Maximum number of characters to return per read.
    max_chars: usize,
}

impl Default for ReadFileTool {
    fn default() -> Self {
        Self {
            max_chars: 10_000,
        }
    }
}

impl ReadFileTool {
    /// Create a ReadFileTool with a custom character limit.
    pub fn with_max_chars(max_chars: usize) -> Self {
        Self { max_chars }
    }
}

/// P0-2: 单次读取文件最大字节数（10 MiB）。超过此大小的文件直接拒绝，
/// 不执行 `read_to_string` 以防止 OOM / DoS。
const MAX_FILE_SIZE_BYTES: u64 = 10 * 1024 * 1024;  // 10 MiB

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Supports optional offset and limit for \
         partial reads. Returns at most 10,000 characters."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "0-based line number to start reading from"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read"
                }
            },
            "required": ["path"]
        })
    }

    fn capability(&self) -> CapabilityLevel {
        CapabilityLevel::Read
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let path = require_string(&params, "path")?;
        let offset = optional_u64(&params, "offset").unwrap_or(0) as usize;
        let limit = optional_u64(&params, "limit");

        let canonical_path = crate::tools::resolve_safe_path(&path)?;

        // P0-2: 检查文件大小，超过 10 MiB 拒绝读取以防 OOM
        if let Ok(meta) = std::fs::metadata(&canonical_path) {
            if meta.is_file() && meta.len() > MAX_FILE_SIZE_BYTES {
                return Err(ToolError::execution_error(format!(
                    "file too large: {} bytes exceeds {MAX_FILE_SIZE_BYTES} bytes limit",
                    meta.len()
                )));
            }
        }

        let content = std::fs::read_to_string(&canonical_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ToolError::execution_error(format!("file not found: {path}"))
            } else {
                ToolError::Io(e)
            }
        })?;

        let lines: Vec<&str> = content.lines().collect();

        // Apply offset
        let start = offset.min(lines.len());
        // Apply limit
        let end = match limit {
            Some(lim) => (start + lim as usize).min(lines.len()),
            None => lines.len(),
        };

        let selected: String = lines[start..end].join("\n");

        // Truncate to max_chars
        let (display, truncated) = if selected.chars().count() > self.max_chars {
            let truncated: String = selected.chars().take(self.max_chars).collect();
            (truncated, true)
        } else {
            (selected, false)
        };

        let mut output = if truncated {
            format!(
                "{display}\n\n[Truncated: showing first {} of {} characters]",
                self.max_chars,
                content.chars().count()
            )
        } else {
            display
        };

        // If the file was empty, make that explicit
        if output.is_empty() && lines.is_empty() {
            output = "(file is empty)".to_string();
        } else if output.is_empty() {
            output = "(no content in selected range)".to_string();
        }

        Ok(tool_success("", output))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_temp_file(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        let mut f = std::fs::File::create(&file_path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (dir, file_path)
    }

    #[tokio::test]
    async fn read_entire_file() {
        let (_dir, path) = make_temp_file("hello\nworld\n");
        let tool = ReadFileTool::default();
        let result = tool
            .execute(serde_json::json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap();
        assert!(result.is_success());
        assert_eq!(result.output.unwrap(), "hello\nworld");
    }

    #[tokio::test]
    async fn read_with_offset_and_limit() {
        let (_dir, path) = make_temp_file("line0\nline1\nline2\nline3\nline4\n");
        let tool = ReadFileTool::default();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "offset": 1,
                "limit": 2
            }))
            .await
            .unwrap();
        assert_eq!(result.output.unwrap(), "line1\nline2");
    }

    #[tokio::test]
    async fn file_not_found_error() {
        let tool = ReadFileTool::default();
        let err = tool
            .execute(serde_json::json!({"path": "/nonexistent/path/file.txt"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn missing_path_param() {
        let tool = ReadFileTool::default();
        let err = tool
            .execute(serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing required field"));
    }

    #[tokio::test]
    async fn empty_file() {
        let (_dir, path) = make_temp_file("");
        let tool = ReadFileTool::default();
        let result = tool
            .execute(serde_json::json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap();
        assert!(result.output.unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn offset_past_end() {
        let (_dir, path) = make_temp_file("one\ntwo\n");
        let tool = ReadFileTool::default();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "offset": 100
            }))
            .await
            .unwrap();
        assert!(result.output.unwrap().contains("no content"));
    }

    #[tokio::test]
    async fn truncation_at_max_chars() {
        let content = "x".repeat(15_000);
        let (_dir, path) = make_temp_file(&content);
        let tool = ReadFileTool::with_max_chars(100);
        let result = tool
            .execute(serde_json::json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap();
        let out = result.output.unwrap();
        assert!(out.contains("Truncated"));
        assert!(out.len() < 500); // should be small after truncation
    }
}
