//! Build failure clustering and data flywheel.
//!
//! This module collects error traces from agent execution, clusters them by
//! common dimensions (error type, tool, language, file pattern, turn count),
//! generates improvement suggestions, and feeds them back into tool descriptions
//! -- creating a continuous improvement loop.
//!
//! # Architecture
//!
//! ```text
//! ErrorTrace         ?FailureAnalyzer         ?Vec<FailureCluster>
//!                                           ?
//!                                           ?
//!                                  ImprovementSuggester
//!                                           ?
//!                                           ?
//!                                   Vec<Suggestion>
//!                                           ?
//!                                           ?
//!                                  NightlyPipeline
//!                                           ?
//!                                           ?
//!                              auto-update tool descriptions
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use code_agent_core::flywheel::{FailureAnalyzer, ImprovementSuggester, ErrorTrace};
//!
//! let traces = vec![ErrorTrace { error_type: "timeout".into(), tool_name: "bash".into(), .. }];
//! let analyzer = FailureAnalyzer::new();
//! let clusters = analyzer.cluster_by(&traces, &["error_type", "tool_name"]);
//!
//! let suggester = ImprovementSuggester::new();
//! let suggestions = suggester.suggest(&clusters);
//! ```


//! Core data types for the data flywheel: ErrorTrace, ClusterKey, FailureCluster, Suggestion.
pub mod analyzer;
pub mod pipeline;
pub mod suggester;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

// ---------------------------------------------------------------------------
// Core data types
// ---------------------------------------------------------------------------

/// A single error trace captured during agent execution.
///
/// Each trace records what went wrong, which tool was involved, what language
/// the agent was working with, and the file pattern being operated on.
/// A single error trace captured during agent execution for the data flywheel.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorTrace {
    /// The error category (e.g., `"timeout"`, `"permission_denied"`,
    /// `"syntax_error"`, `"build_failure"`, `"tool_not_found"`).
    pub error_type: String,

    /// The tool that was being used when the error occurred
    /// (e.g., `"bash"`, `"read_file"`, `"write_file"`, `"grep"`).
    pub tool_name: String,

    /// The programming language context, if detectable
    /// (e.g., `"rust"`, `"python"`, `"typescript"`, `"unknown"`).
    pub language: String,

    /// A glob-like pattern for the file being operated on
    /// (e.g., `"src/**/*.rs"`, `"*.py"`, `"Cargo.toml"`).
    pub file_pattern: String,

    /// The turn number within the session when the error occurred (1-based).
    pub turn_count: usize,

    /// The raw error message text.
    pub message: String,

    /// Timestamp when the error was recorded.
    pub timestamp: DateTime<Utc>,
}

/// The key used to group error traces into a cluster.
///
/// A cluster key is a set of dimension values that define a unique bucket.
/// The key used to group error traces into a cluster (e.g. {"error_type": "timeout", "tool_name": "bash"}).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClusterKey {
    /// Dimension name -> value pairs (e.g., `{"error_type": "timeout",
    /// "tool_name": "bash"}`).
    /// Uses BTreeMap for deterministic ordering and Hash support.
    pub dimensions: BTreeMap<String, String>,
}

impl ClusterKey {
    /// Create a new cluster key from dimension pairs.

    pub fn new(dimensions: BTreeMap<String, String>) -> Self {
        Self { dimensions }
    }

    /// Create a cluster key from a single dimension.

    pub fn single(dim: &str, value: &str) -> Self {
        let mut dimensions = BTreeMap::new();
        dimensions.insert(dim.to_string(), value.to_string());
        Self { dimensions }
    }
}

impl std::fmt::Display for ClusterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts: Vec<String> = self
            .dimensions
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        parts.sort();
        write!(f, "{}", parts.join(","))
    }
}

/// A cluster of related error traces.
/// A cluster of related error traces with a representative message.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailureCluster {
    /// The key that defines this cluster's dimensions.
    pub key: ClusterKey,

    /// The error traces grouped into this cluster.
    pub traces: Vec<ErrorTrace>,

    /// The number of traces in this cluster (cached for convenience).
    pub count: usize,

    /// The most common error message in this cluster (heuristic).
    pub representative_message: String,
}

impl FailureCluster {
    /// Create a new cluster from a key and a list of traces.

    pub fn new(key: ClusterKey, traces: Vec<ErrorTrace>) -> Self {
        let count = traces.len();
        let representative_message = Self::find_representative(&traces);
        Self {
            key,
            traces,
            count,
            representative_message,
        }
    }

    /// Find the most common error message in the traces.
    fn find_representative(traces: &[ErrorTrace]) -> String {
        if traces.is_empty() {
            return String::new();
        }
        let mut freq: HashMap<&str, usize> = HashMap::new();
        for t in traces {
            *freq.entry(&t.message).or_insert(0) += 1;
        }
        freq.into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(msg, _)| msg.to_string())
            .unwrap_or_default()
    }
}

/// A suggestion for improving tool descriptions or agent behavior.
/// A suggestion for improving tool descriptions or agent behavior.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Suggestion {
    /// A short title for the suggestion.
    pub title: String,

    /// Detailed description of what to change.
    pub description: String,

    /// The tool this suggestion targets (if any).
    pub target_tool: Option<String>,

    /// The priority of this suggestion (higher = more important).
    pub priority: usize,

    /// The cluster key that triggered this suggestion.
    pub source_cluster: ClusterKey,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_trace(error_type: &str, tool_name: &str, message: &str) -> ErrorTrace {
        ErrorTrace {
            error_type: error_type.into(),
            tool_name: tool_name.into(),
            language: "rust".into(),
            file_pattern: "src/**/*.rs".into(),
            turn_count: 3,
            message: message.into(),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn cluster_key_single() {
        let key = ClusterKey::single("error_type", "timeout");
        assert_eq!(key.dimensions.len(), 1);
        assert_eq!(key.dimensions.get("error_type").unwrap(), "timeout");
    }

    #[test]
    fn cluster_key_display() {
        let key = ClusterKey::single("error_type", "timeout");
        let display = key.to_string();
        assert_eq!(display, "error_type=timeout");
    }

    #[test]
    fn cluster_key_display_sorted() {
        let mut dims = BTreeMap::new();
        dims.insert("b".into(), "2".into());
        dims.insert("a".into(), "1".into());
        let key = ClusterKey::new(dims);
        let display = key.to_string();
        assert_eq!(display, "a=1,b=2");
    }

    #[test]
    fn failure_cluster_representative_message() {
        let traces = vec![
            sample_trace("timeout", "bash", "command timed out after 30s"),
            sample_trace("timeout", "bash", "command timed out after 30s"),
            sample_trace("timeout", "bash", "connection reset"),
        ];
        let cluster = FailureCluster::new(ClusterKey::single("error_type", "timeout"), traces);
        assert_eq!(cluster.count, 3);
        assert_eq!(cluster.representative_message, "command timed out after 30s");
    }

    #[test]
    fn failure_cluster_empty_traces() {
        let cluster = FailureCluster::new(ClusterKey::single("error_type", "timeout"), vec![]);
        assert_eq!(cluster.count, 0);
        assert!(cluster.representative_message.is_empty());
    }

    #[test]
    fn error_trace_serde_roundtrip() {
        let trace = sample_trace("build_failure", "bash", "cargo build failed");
        let json = serde_json::to_string(&trace).unwrap();
        let parsed: ErrorTrace = serde_json::from_str(&json).unwrap();
        assert_eq!(trace, parsed);
    }

    #[test]
    fn suggestion_creation() {
        let key = ClusterKey::single("tool_name", "bash");
        let suggestion = Suggestion {
            title: "Add timeout hint".into(),
            description: "Mention --timeout flag in bash tool description".into(),
            target_tool: Some("bash".into()),
            priority: 5,
            source_cluster: key.clone(),
        };
        assert_eq!(suggestion.target_tool.as_deref(), Some("bash"));
        assert_eq!(suggestion.source_cluster, key);
    }
}
