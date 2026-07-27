//! Edit file tool.
//!
//! Performs a search-and-replace operation on a file. The search string must
//! appear exactly once in the file; otherwise, an error is returned.

use async_trait::async_trait;
use code_agent_protocol::CapabilityLevel;

use crate::tools::{require_string, tool_success, Tool, ToolError, ToolResultMessage};

/// Performs a surgical search-and-replace edit on a file.
///
/// The `old_string` must appear exactly once in the file content. Zero or
/// multiple matches will produce an error.
///
/// Input schema:
/// ```json
/// {
///   "path": "string (required) – path to the file to edit",
///   "old_string": "string (required) – exact text to replace",
///   "new_string": "string (required) – replacement text"
/// }
/// ```
pub struct EditFileTool;

impl Default for EditFileTool {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Perform a search-and-replace edit on a file. The old_string must \
         appear exactly once in the file. Use this for precise, surgical edits."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to find and replace"
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text"
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    fn capability(&self) -> CapabilityLevel {
        CapabilityLevel::Edit
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let path = require_string(&params, "path")?;
        let old_string = require_string(&params, "old_string")?;
        let new_string = require_string(&params, "new_string")?;

        let canonical_path = crate::tools::resolve_safe_path(&path)?;

        // M4: TOCTOU guard — snapshot file metadata before reading
        let file_len_before = std::fs::metadata(&canonical_path)
            .map_err(ToolError::Io)?
            .len();

        let content = std::fs::read_to_string(&canonical_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ToolError::execution_error(format!("file not found: {path}"))
            } else {
                ToolError::Io(e)
            }
        })?;

        // Count occurrences
        let matches: Vec<_> = content.match_indices(&old_string).collect();

        match matches.len() {
            0 => {
                return Err(ToolError::execution_error(format!(
                    "old_string not found in file: {path}"
                )));
            }
            1 => {
                let (pos, _) = matches[0];
                let new_content = format!(
                    "{}{new_string}{}",
                    &content[..pos],
                    &content[pos + old_string.len()..]
                );
                // M4: TOCTOU guard — verify file unchanged between read and write
                let file_len_now = std::fs::metadata(&canonical_path)
                    .map_err(ToolError::Io)?
                    .len();
                if file_len_before != file_len_now {
                    return Err(ToolError::execution_error(format!(
                        "File {path} was modified by another process; please retry"
                    )));
                }
                std::fs::write(&canonical_path, &new_content)?;
                Ok(tool_success(
                    "",
                    format!(
                        "Successfully edited {path}: replaced 1 occurrence of \
                         old_string ({old_len} chars) with new_string ({new_len} chars)",
                        old_len = old_string.len(),
                        new_len = new_string.len(),
                    ),
                ))
            }
            n => {
                Err(ToolError::execution_error(format!(
                    "old_string found {n} times in {path}, expected exactly 1. \
                     Add more surrounding context to make the match unique."
                )))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn setup_file(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("edit_test.txt");
        let mut f = std::fs::File::create(&file_path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (dir, file_path)
    }

    #[tokio::test]
    async fn edit_single_occurrence() {
        let (_dir, path) = setup_file("fn main() {\n    println!(\"old\");\n}\n");
        let tool = EditFileTool::default();

        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "\"old\"",
                "new_string": "\"new\""
            }))
            .await
            .unwrap();

        assert!(result.is_success());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"new\""));
        assert!(!content.contains("\"old\""));
    }

    #[tokio::test]
    async fn edit_zero_occurrences_error() {
        let (_dir, path) = setup_file("hello world");
        let tool = EditFileTool::default();

        let err = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "nonexistent",
                "new_string": "replacement"
            }))
            .await
            .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("not found"));
    }

    #[tokio::test]
    async fn edit_multiple_occurrences_error() {
        let (_dir, path) = setup_file("dup dup dup");
        let tool = EditFileTool::default();

        let err = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "dup",
                "new_string": "replaced"
            }))
            .await
            .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("3 times"));
    }

    #[tokio::test]
    async fn edit_file_not_found() {
        let tool = EditFileTool::default();
        let err = tool
            .execute(serde_json::json!({
                "path": "/nonexistent/edit.txt",
                "old_string": "x",
                "new_string": "y"
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn edit_preserves_rest_of_file() {
        let original = "line1\nline2 TARGET line3\nline4\n";
        let (_dir, path) = setup_file(original);
        let tool = EditFileTool::default();

        tool.execute(serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_string": "TARGET",
            "new_string": "REPLACED"
        }))
        .await
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("line1\n"));
        assert!(content.contains("line4\n"));
        assert!(content.contains("REPLACED"));
        assert!(!content.contains("TARGET"));
    }
}
