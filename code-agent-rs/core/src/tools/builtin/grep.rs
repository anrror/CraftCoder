//! Grep (search file contents) tool.
//!
//! Searches file contents in a directory using regex patterns. Returns
//! matching lines with file paths and line numbers.

use async_trait::async_trait;
use code_agent_protocol::CapabilityLevel;

use crate::tools::{
    optional_string, require_string, tool_success, Tool, ToolError, ToolResultMessage,
};

const MAX_RESULTS: usize = 500;
const MAX_FILE_SIZE: u64 = 1_000_000; // 1 MB skip threshold

/// Searches file contents for a regex pattern.
///
/// Input schema:
/// ```json
/// {
///   "pattern": "string (required) – regex pattern to search for",
///   "path": "string (optional) – directory to search (default: current dir)",
///   "include": "string (optional) – glob pattern to filter files"
/// }
/// ```
pub struct GrepTool;

impl Default for GrepTool {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents for a regex pattern. Returns matching lines with \
         file paths and line numbers. Use 'include' to filter by file pattern."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (default: current directory)"
                },
                "include": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g. \"*.rs\")"
                }
            },
            "required": ["pattern"]
        })
    }

    fn capability(&self) -> CapabilityLevel {
        CapabilityLevel::Read
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let pattern_str = require_string(&params, "pattern")?;
        let search_path = require_string(&params, "path").unwrap_or_else(|_| ".".to_string());
        let include_filter = optional_string(&params, "include");

        let regex = regex::Regex::new(&pattern_str).map_err(|e| {
            ToolError::invalid_input(format!("invalid regex pattern: {e}"))
        })?;

        let mut results: Vec<String> = Vec::new();
        let mut files_searched = 0usize;
        let mut total_matches = 0usize;

        Self::walk_dir(
            &search_path,
            &include_filter,
            &regex,
            &mut results,
            &mut files_searched,
            &mut total_matches,
        )?;

        if results.is_empty() {
            return Ok(tool_success(
                "",
                format!("No matches found for pattern '{pattern_str}'"),
            ));
        }

        let summary = format!(
            "Found {total_matches} matches in {files_searched} files:\n\n{results}",
            results = results.join("\n"),
        );

        Ok(tool_success("", summary))
    }
}

impl GrepTool {
    #[allow(clippy::too_many_arguments)]
    fn walk_dir(
        dir: &str,
        include_filter: &Option<String>,
        regex: &regex::Regex,
        results: &mut Vec<String>,
        files_searched: &mut usize,
        total_matches: &mut usize,
    ) -> Result<(), ToolError> {
        let entries = std::fs::read_dir(dir).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ToolError::execution_error(format!("directory not found: {dir}"))
            } else {
                ToolError::Io(e)
            }
        })?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                // Skip hidden directories and common non-source dirs
                let dir_name = entry.file_name().to_string_lossy().to_string();
                if dir_name.starts_with('.')
                    || dir_name == "target"
                    || dir_name == "node_modules"
                {
                    continue;
                }
                Self::walk_dir(
                    &path.to_string_lossy(),
                    include_filter,
                    regex,
                    results,
                    files_searched,
                    total_matches,
                )?;
                continue;
            }

            if !path.is_file() {
                continue;
            }

            // Apply include filter
            if let Some(ref filter) = include_filter {
                let file_name = entry.file_name().to_string_lossy().to_string();
                if !simple_glob_match(filter, &file_name) {
                    continue;
                }
            }

            // Skip large files
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if metadata.len() > MAX_FILE_SIZE {
                continue;
            }

            // Skip binary files (check first 8KB for null bytes)
            let content = match std::fs::read(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if content.iter().take(8192).any(|&b| b == 0) {
                continue;
            }

            let text = match String::from_utf8(content) {
                Ok(t) => t,
                Err(_) => continue,
            };

            *files_searched += 1;
            let file_display = path.to_string_lossy().to_string();

            for (line_num, line) in text.lines().enumerate() {
                if *total_matches >= MAX_RESULTS {
                    break;
                }
                if regex.is_match(line) {
                    results.push(format!("{file_display}:{}: {line}", line_num + 1));
                    *total_matches += 1;
                }
            }

            if *total_matches >= MAX_RESULTS {
                results.push(format!("... (truncated at {MAX_RESULTS} results)"));
                break;
            }
        }

        Ok(())
    }
}

/// Simple glob matching: supports `*` (any chars), `**` (matches across path separators,
/// treated as prefix-suffix match), and `?` (single char).
fn simple_glob_match(pattern: &str, name: &str) -> bool {
    // Handle `**` — matches across directory separators
    if let Some(pos) = pattern.find("**") {
        let prefix = &pattern[..pos];
        let suffix = &pattern[pos + 2..];

        // Check that name starts with the prefix
        if !name.starts_with(prefix) {
            return false;
        }

        // The suffix may start with `/` which is the path separator
        let filename_pattern = suffix.trim_start_matches('/');

        // `**` matches everything between prefix and the filename
        // Match the filename pattern against the filename (after last `/`)
        let remaining = &name[prefix.len()..];
        if let Some(last_slash) = remaining.rfind('/') {
            let filename = &remaining[last_slash + 1..];
            simple_glob_match_single(filename_pattern, filename)
        } else {
            // No directory separator in remaining — match directly
            simple_glob_match_single(filename_pattern, remaining)
        }
    } else {
        simple_glob_match_single(pattern, name)
    }
}

fn simple_glob_match_single(pattern: &str, name: &str) -> bool {
    let mut pi = 0usize;
    let mut ni = 0usize;
    let pchars: Vec<char> = pattern.chars().collect();
    let nchars: Vec<char> = name.chars().collect();
    let mut star_idx: Option<(usize, usize)> = None;

    while ni < nchars.len() {
        if pi < pchars.len() && (pchars[pi] == '?' || pchars[pi] == nchars[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < pchars.len() && pchars[pi] == '*' {
            star_idx = Some((pi, ni));
            pi += 1;
        } else if let Some((s_pi, s_ni)) = star_idx {
            pi = s_pi + 1;
            ni = s_ni + 1;
            star_idx = Some((s_pi, s_ni + 1));
        } else {
            return false;
        }
    }

    while pi < pchars.len() && pchars[pi] == '*' {
        pi += 1;
    }

    pi == pchars.len()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn setup_search_dir() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();
        let mut f1 = std::fs::File::create(dir.path().join("main.rs")).unwrap();
        f1.write_all(b"fn main() {\n    println!(\"TODO: implement\");\n}\n")
            .unwrap();
        let mut f2 = std::fs::File::create(dir.path().join("lib.rs")).unwrap();
        f2.write_all(b"pub fn helper() -> &'static str { \"ok\" }\n")
            .unwrap();
        let mut f3 = std::fs::File::create(dir.path().join("README.md")).unwrap();
        f3.write_all(b"# Project\n\nTODO: write docs\n").unwrap();
        (dir, dir_path)
    }

    #[tokio::test]
    async fn grep_finds_matches() {
        let (_dir, path) = setup_search_dir();
        let tool = GrepTool::default();
        let result = tool
            .execute(serde_json::json!({
                "pattern": "TODO",
                "path": path.to_str().unwrap()
            }))
            .await
            .unwrap();

        assert!(result.is_success());
        let output = result.output.unwrap();
        assert!(output.contains("TODO"));
        assert!(output.contains("main.rs"));
        assert!(output.contains("README.md"));
    }

    #[tokio::test]
    async fn grep_with_include_filter() {
        let (_dir, path) = setup_search_dir();
        let tool = GrepTool::default();
        let result = tool
            .execute(serde_json::json!({
                "pattern": "TODO",
                "path": path.to_str().unwrap(),
                "include": "*.rs"
            }))
            .await
            .unwrap();

        let output = result.output.unwrap();
        assert!(output.contains("main.rs"));
        // Should NOT contain README.md since we filtered to *.rs
        assert!(!output.contains("README.md"));
    }

    #[tokio::test]
    async fn grep_no_matches() {
        let (_dir, path) = setup_search_dir();
        let tool = GrepTool::default();
        let result = tool
            .execute(serde_json::json!({
                "pattern": "NONEXISTENT_PATTERN_XYZ",
                "path": path.to_str().unwrap()
            }))
            .await
            .unwrap();

        assert!(result.output.unwrap().contains("No matches"));
    }

    #[tokio::test]
    async fn grep_invalid_regex() {
        let tool = GrepTool::default();
        let err = tool
            .execute(serde_json::json!({
                "pattern": "[invalid",
                "path": "."
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid regex"));
    }

    #[tokio::test]
    async fn grep_nonexistent_directory() {
        let tool = GrepTool::default();
        let err = tool
            .execute(serde_json::json!({
                "pattern": "test",
                "path": "/nonexistent/dir"
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn glob_match_exact() {
        assert!(simple_glob_match("main.rs", "main.rs"));
    }

    #[test]
    fn glob_match_star() {
        assert!(simple_glob_match("*.rs", "main.rs"));
        assert!(simple_glob_match("*.rs", "lib.rs"));
        assert!(!simple_glob_match("*.rs", "README.md"));
    }

    #[test]
    fn glob_match_question() {
        assert!(simple_glob_match("file?.txt", "file1.txt"));
        assert!(simple_glob_match("file?.txt", "fileA.txt"));
        assert!(!simple_glob_match("file?.txt", "file10.txt"));
    }

    #[test]
    fn glob_match_complex() {
        assert!(simple_glob_match("src/*.rs", "src/main.rs"));
        assert!(simple_glob_match("src/*.rs", "src/lib.rs"));
        assert!(!simple_glob_match("src/*.rs", "tests/test.rs"));
    }
}
