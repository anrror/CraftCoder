//! Glob (find files by pattern) tool.
//!
//! Finds files matching a glob pattern. Returns matching file paths sorted
//! alphabetically.

use async_trait::async_trait;
use code_agent_protocol::CapabilityLevel;

use crate::tools::{require_string, tool_success, Tool, ToolError, ToolResultMessage};

/// Finds files matching a glob pattern.
///
/// Input schema:
/// ```json
/// {
///   "pattern": "string (required) – glob pattern (e.g. \"**/*.rs\")"
/// }
/// ```
pub struct GlobTool;

impl Default for GlobTool {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files matching a glob pattern. Returns sorted list of matching \
         file paths. Supports ** for recursive matching."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match files (e.g. \"**/*.rs\", \"src/**/*.ts\")"
                }
            },
            "required": ["pattern"]
        })
    }

    fn capability(&self) -> CapabilityLevel {
        CapabilityLevel::Read
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let pattern = require_string(&params, "pattern")?;

        let paths = glob::glob(&pattern).map_err(|e| {
            ToolError::invalid_input(format!("invalid glob pattern: {e}"))
        })?;

        let mut matches: Vec<String> = Vec::new();
        for entry in paths {
            match entry {
                Ok(path) => {
                    matches.push(path.to_string_lossy().to_string());
                }
                Err(e) => {
                    // Skip permission errors and other access issues
                    if !matches.is_empty() {
                        eprintln!("glob warning (skipping): {e}");
                    }
                }
            }
        }

        if matches.is_empty() {
            return Ok(tool_success(
                "",
                format!("No files matched pattern '{pattern}'"),
            ));
        }

        matches.sort();

        let count = matches.len();
        let output = format!(
            "Found {count} file(s) matching '{pattern}':\n{files}",
            files = matches.join("\n"),
        );

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

    fn setup_files() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let mut f1 = std::fs::File::create(dir.path().join("src").join("main.rs")).unwrap();
        f1.write_all(b"// main").unwrap();
        let mut f2 = std::fs::File::create(dir.path().join("src").join("lib.rs")).unwrap();
        f2.write_all(b"// lib").unwrap();
        let mut f3 = std::fs::File::create(dir.path().join("README.md")).unwrap();
        f3.write_all(b"# readme").unwrap();
        dir
    }

    #[tokio::test]
    async fn glob_finds_rs_files() {
        let dir = setup_files();
        let pattern = format!("{}\\**\\*.rs", dir.path().to_string_lossy());

        let tool = GlobTool::default();
        let result = tool
            .execute(serde_json::json!({"pattern": pattern}))
            .await
            .unwrap();

        assert!(result.is_success());
        let output = result.output.unwrap();
        assert!(output.contains("main.rs"));
        assert!(output.contains("lib.rs"));
        assert!(!output.contains("README.md"));
        assert!(output.contains("Found 2 file"));
    }

    #[tokio::test]
    async fn glob_no_matches() {
        let dir = setup_files();
        let pattern = format!("{}\\**\\*.py", dir.path().to_string_lossy());

        let tool = GlobTool::default();
        let result = tool
            .execute(serde_json::json!({"pattern": pattern}))
            .await
            .unwrap();

        assert!(result.is_success());
        assert!(result.output.unwrap().contains("No files matched"));
    }

    #[tokio::test]
    async fn glob_invalid_pattern() {
        let tool = GlobTool::default();
        let result = tool.execute(serde_json::json!({"pattern": "***"})).await;
        // This may fail or produce empty results; either is acceptable
        if let Err(e) = result {
            assert!(e.to_string().contains("invalid") || e.to_string().contains("glob"));
        }
    }

    #[tokio::test]
    async fn glob_missing_pattern_param() {
        let tool = GlobTool::default();
        let err = tool.execute(serde_json::json!({})).await.unwrap_err();
        assert!(err.to_string().contains("missing required field"));
    }

    #[tokio::test]
    async fn glob_output_is_sorted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("c.txt"), "c").unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b").unwrap();

        let pattern = format!("{}\\*.txt", dir.path().to_string_lossy());
        let tool = GlobTool::default();
        let result = tool
            .execute(serde_json::json!({"pattern": pattern}))
            .await
            .unwrap();

        let output = result.output.unwrap();
        // Find positions of a.txt, b.txt, c.txt in output
        let a_pos = output.find("a.txt").unwrap();
        let b_pos = output.find("b.txt").unwrap();
        let c_pos = output.find("c.txt").unwrap();
        assert!(a_pos < b_pos);
        assert!(b_pos < c_pos);
    }
}
