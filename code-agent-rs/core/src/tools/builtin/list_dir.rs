//! List directory tool.
//!
//! Lists the contents of a directory, returning file/directory names with
//! metadata (type, size for files).

use async_trait::async_trait;
use code_agent_protocol::CapabilityLevel;

use crate::tools::{require_string, tool_success, Tool, ToolError, ToolResultMessage};

/// Lists directory contents with metadata.
///
/// Input schema:
/// ```json
/// {
///   "path": "string (required) – path to the directory to list"
/// }
/// ```
pub struct ListDirTool;

impl Default for ListDirTool {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn description(&self) -> &str {
        "List the contents of a directory. Returns entries with type (file/dir) \
         and file sizes."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the directory to list"
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
        let canonical_path = crate::tools::resolve_safe_path(&path)?;

        let entries = std::fs::read_dir(&canonical_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ToolError::execution_error(format!("directory not found: {path}"))
            } else {
                ToolError::Io(e)
            }
        })?;

        let mut lines: Vec<String> = Vec::new();
        let mut file_count = 0usize;
        let mut dir_count = 0usize;

        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            let file_type = entry.file_type()?;
            let metadata = entry.metadata()?;

            if file_type.is_dir() {
                lines.push(format!("[DIR]  {name}/"));
                dir_count += 1;
            } else if file_type.is_symlink() {
                let target = std::fs::read_link(entry.path())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| "???".to_string());
                lines.push(format!("[LINK] {name} -> {target}"));
            } else {
                let size = metadata.len();
                lines.push(format!("[FILE] {name} ({size} bytes)"));
                file_count += 1;
            }
        }

        if lines.is_empty() {
            lines.push("(directory is empty)".to_string());
        }

        let summary = format!(
            "Contents of {path}:\n{entries}\n\n{file_count} files, {dir_count} directories",
            entries = lines.join("\n"),
        );

        Ok(tool_success("", summary))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn list_directory_with_files_and_dirs() {
        let dir = tempfile::tempdir().unwrap();
        // Create a subdirectory and a file
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        let mut f = std::fs::File::create(dir.path().join("file1.txt")).unwrap();
        f.write_all(b"hello").unwrap();

        let tool = ListDirTool::default();
        let result = tool
            .execute(serde_json::json!({"path": dir.path().to_str().unwrap()}))
            .await
            .unwrap();

        assert!(result.is_success());
        let output = result.output.unwrap();
        assert!(output.contains("subdir"));
        assert!(output.contains("file1.txt"));
        assert!(output.contains("[DIR]"));
        assert!(output.contains("[FILE]"));
        assert!(output.contains("1 files, 1 directories"));
    }

    #[tokio::test]
    async fn list_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let tool = ListDirTool::default();
        let result = tool
            .execute(serde_json::json!({"path": dir.path().to_str().unwrap()}))
            .await
            .unwrap();

        assert!(result.is_success());
        assert!(result.output.unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn list_nonexistent_directory() {
        let tool = ListDirTool::default();
        let err = tool
            .execute(serde_json::json!({"path": "/nonexistent/dir"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn list_path_to_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("not_a_dir.txt");
        std::fs::write(&file_path, "content").unwrap();

        let tool = ListDirTool::default();
        let result = tool
            .execute(serde_json::json!({"path": file_path.to_str().unwrap()}))
            .await;
        // Should be an error (read_dir on a file)
        assert!(result.is_err() || result.unwrap().is_error());
    }
}
