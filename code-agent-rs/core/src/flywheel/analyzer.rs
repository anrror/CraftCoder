//! FailureAnalyzer -- clusters error traces by configurable dimensions.
//!
//! Uses simple pattern matching and grouping (no ML). Supports clustering by
//! any combination of: `error_type`, `tool_name`, `language`, `file_pattern`,
//! and `turn_count` (bucketed into early/mid/late).

//! Clusters error traces by error_type, tool_name, language, file_pattern, and turn_count.
use crate::flywheel::{ClusterKey, ErrorTrace, FailureCluster};
use std::collections::BTreeMap;

/// Clusters error traces by specified dimensions.
///
/// # Dimension reference
///
/// | Dimension       | Source field       | Notes                              |
/// |-----------------|--------------------|------------------------------------|
/// | `error_type`    | `ErrorTrace`       | Direct match                       |
/// | `tool_name`     | `ErrorTrace`       | Direct match                       |
/// | `language`      | `ErrorTrace`       | Direct match                       |
/// | `file_pattern`  | `ErrorTrace`       | Direct match                       |
/// | `turn_count`    | `ErrorTrace`       | Bucketed: early(1-3), mid(4-10), late(11+) |
///
/// Unknown dimensions are silently ignored.
/// FailureAnalyzer clusters error traces by configurable dimensions (no ML).
#[derive(Clone, Debug, Default)]
pub struct FailureAnalyzer {
    /// The dimensions to cluster by (e.g., `["error_type", "tool_name"]`).
    dimensions: Vec<String>,
}

impl FailureAnalyzer {
    /// Create a new analyzer with default dimensions.
    ///
    /// Default dimensions are `["error_type", "tool_name"]`.
    pub fn new() -> Self {
        Self {
            dimensions: vec!["error_type".into(), "tool_name".into()],
        }
    }

    /// Create an analyzer with a custom set of dimensions.
    pub fn with_dimensions(dimensions: Vec<String>) -> Self {
        Self { dimensions }
    }

    /// Set the dimensions to cluster by.
    pub fn set_dimensions(&mut self, dimensions: Vec<String>) {
        self.dimensions = dimensions;
    }

    /// Cluster the given traces by the configured dimensions.
    ///
    /// Returns a list of clusters sorted by count (largest first).
    pub fn cluster_by(&self, traces: &[ErrorTrace]) -> Vec<FailureCluster> {
        let mut buckets: BTreeMap<ClusterKey, Vec<ErrorTrace>> = BTreeMap::new();

        for trace in traces {
            let key = self.build_key(trace);
            buckets.entry(key).or_default().push(trace.clone());
        }

        let mut clusters: Vec<FailureCluster> = buckets
            .into_iter()
            .map(|(key, traces)| FailureCluster::new(key, traces))
            .collect();

        // Sort by count descending
        clusters.sort_by_key(|b| std::cmp::Reverse(b.count));

        clusters
    }

    /// Build a cluster key from a single trace using the configured dimensions.
    fn build_key(&self, trace: &ErrorTrace) -> ClusterKey {
        let mut dimensions = BTreeMap::new();
        for dim in &self.dimensions {
            let value = self.extract_dimension(dim, trace);
            dimensions.insert(dim.clone(), value);
        }
        ClusterKey::new(dimensions)
    }

    /// Extract a single dimension value from a trace.
    fn extract_dimension(&self, dim: &str, trace: &ErrorTrace) -> String {
        match dim {
            "error_type" => trace.error_type.clone(),
            "tool_name" => trace.tool_name.clone(),
            "language" => trace.language.clone(),
            "file_pattern" => trace.file_pattern.clone(),
            "turn_count" => bucket_turn_count(trace.turn_count),
            _ => "unknown".into(),
        }
    }
}

/// Bucket a turn count into a category string.
///
/// - 1-3   => "early"
/// - 4-10  => "mid"
/// - 11+   => "late"
fn bucket_turn_count(turn_count: usize) -> String {
    match turn_count {
        0..=3 => "early".into(),
        4..=10 => "mid".into(),
        _ => "late".into(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn trace(
        error_type: &str,
        tool_name: &str,
        language: &str,
        file_pattern: &str,
        turn_count: usize,
        message: &str,
    ) -> ErrorTrace {
        ErrorTrace {
            error_type: error_type.into(),
            tool_name: tool_name.into(),
            language: language.into(),
            file_pattern: file_pattern.into(),
            turn_count,
            message: message.into(),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn cluster_by_default_dimensions() {
        let traces = vec![
            trace("timeout", "bash", "rust", "src/**/*.rs", 2, "timeout"),
            trace("timeout", "bash", "rust", "src/**/*.rs", 5, "timeout"),
            trace("timeout", "bash", "python", "*.py", 1, "timeout"),
            trace("permission_denied", "read_file", "rust", "src/**/*.rs", 3, "denied"),
        ];

        let analyzer = FailureAnalyzer::new();
        let clusters = analyzer.cluster_by(&traces);

        // Default dims: error_type + tool_name   ?2 unique combos
        // (timeout+bash appears twice, permission_denied+read_file once)
        assert_eq!(clusters.len(), 2);

        // Largest cluster first (timeout+bash has 3 traces)
        assert_eq!(clusters[0].count, 3);
        assert_eq!(
            clusters[0].key.dimensions.get("error_type").unwrap(),
            "timeout"
        );
        assert_eq!(
            clusters[0].key.dimensions.get("tool_name").unwrap(),
            "bash"
        );
    }

    #[test]
    fn cluster_by_single_dimension() {
        let traces = vec![
            trace("timeout", "bash", "rust", "*.rs", 1, "msg1"),
            trace("timeout", "read_file", "rust", "*.rs", 2, "msg2"),
            trace("permission_denied", "bash", "rust", "*.rs", 3, "msg3"),
        ];

        let analyzer = FailureAnalyzer::with_dimensions(vec!["error_type".into()]);
        let clusters = analyzer.cluster_by(&traces);

        assert_eq!(clusters.len(), 2); // timeout    2, permission_denied    1
        assert_eq!(clusters[0].count, 2);
        assert_eq!(clusters[1].count, 1);
    }

    #[test]
    fn cluster_by_turn_count_buckets() {
        let traces = vec![
            trace("timeout", "bash", "rust", "*.rs", 1, "early"),
            trace("timeout", "bash", "rust", "*.rs", 2, "early"),
            trace("timeout", "bash", "rust", "*.rs", 7, "mid"),
            trace("timeout", "bash", "rust", "*.rs", 15, "late"),
        ];

        let analyzer = FailureAnalyzer::with_dimensions(vec!["turn_count".into()]);
        let clusters = analyzer.cluster_by(&traces);

        assert_eq!(clusters.len(), 3); // early    2, mid    1, late    1
        assert_eq!(clusters[0].count, 2);
    }

    #[test]
    fn cluster_by_all_dimensions() {
        let traces = vec![
            trace("timeout", "bash", "rust", "src/**/*.rs", 2, "msg"),
            trace("timeout", "bash", "rust", "src/**/*.rs", 3, "msg"),
        ];

        let analyzer = FailureAnalyzer::with_dimensions(vec![
            "error_type".into(),
            "tool_name".into(),
            "language".into(),
            "file_pattern".into(),
            "turn_count".into(),
        ]);
        let clusters = analyzer.cluster_by(&traces);

        // Same error_type+tool_name+language+file_pattern but different turn_count buckets
        //   ?2 clusters (early vs early -- same bucket, so 1 cluster)
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].count, 2);
    }

    #[test]
    fn cluster_by_unknown_dimension() {
        let traces = vec![trace("timeout", "bash", "rust", "*.rs", 1, "msg")];

        let analyzer = FailureAnalyzer::with_dimensions(vec!["nonexistent".into()]);
        let clusters = analyzer.cluster_by(&traces);

        assert_eq!(clusters.len(), 1);
        assert_eq!(
            clusters[0]
                .key
                .dimensions
                .get("nonexistent")
                .unwrap(),
            "unknown"
        );
    }

    #[test]
    fn cluster_empty_traces() {
        let analyzer = FailureAnalyzer::new();
        let clusters = analyzer.cluster_by(&[]);
        assert!(clusters.is_empty());
    }

    #[test]
    fn set_dimensions_after_creation() {
        let mut analyzer = FailureAnalyzer::new();
        analyzer.set_dimensions(vec!["language".into()]);

        let traces = vec![
            trace("timeout", "bash", "rust", "*.rs", 1, "msg"),
            trace("timeout", "bash", "python", "*.py", 1, "msg"),
        ];

        let clusters = analyzer.cluster_by(&traces);
        assert_eq!(clusters.len(), 2);
    }

    #[test]
    fn bucket_turn_count_values() {
        assert_eq!(bucket_turn_count(0), "early");
        assert_eq!(bucket_turn_count(1), "early");
        assert_eq!(bucket_turn_count(3), "early");
        assert_eq!(bucket_turn_count(4), "mid");
        assert_eq!(bucket_turn_count(10), "mid");
        assert_eq!(bucket_turn_count(11), "late");
        assert_eq!(bucket_turn_count(100), "late");
    }
}
