//! NightlyPipeline -- orchestrates the full data flywheel.
//!
//! The nightly pipeline runs on a schedule (typically once per day) and:
//!
//! 1. **Collect** -- loads error traces from a JSON source
//! 2. **Cluster** -- groups traces by configurable dimensions
//! 3. **Suggest** -- generates improvement suggestions from clusters
//! 4. **Auto-update** -- applies suggestions to tool descriptions (simulated)
//!
//! # Usage
//!
//! ```rust,ignore
//! use code_agent_core::flywheel::pipeline::NightlyPipeline;
//!
//! let pipeline = NightlyPipeline::new("traces.json");
//! let report = pipeline.run().await?;
//! println!("{} suggestions generated", report.suggestions_count);
//! ```

//! NightlyPipeline orchestrates the full data flywheel: collect, cluster, suggest, auto-update.
use std::sync::Arc;

use crate::flywheel::analyzer::FailureAnalyzer;
use crate::flywheel::suggester::ImprovementSuggester;
use crate::flywheel::{ErrorTrace, Suggestion};
use serde::{Deserialize, Serialize};

/// Result of a single nightly pipeline run.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]

/// Result of a single nightly pipeline run.
pub struct PipelineReport {
    /// Number of traces processed.
    pub traces_processed: usize,

    /// Number of clusters found.
    pub clusters_found: usize,

    /// Number of suggestions generated.
    pub suggestions_count: usize,

    /// The generated suggestions.
    pub suggestions: Vec<Suggestion>,

    /// Whether tool descriptions were updated.
    pub tool_descriptions_updated: bool,

    /// Human-readable summary.
    pub summary: String,
}

/// The nightly pipeline configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]

/// The nightly pipeline configuration.
pub struct PipelineConfig {
    /// Dimensions to cluster by.
    pub cluster_dimensions: Vec<String>,

    /// Path to the traces JSON file.
    pub traces_path: String,

    /// Whether to auto-update tool descriptions.
    pub auto_update: bool,

    /// Minimum cluster size to generate suggestions for.
    pub min_cluster_size: usize,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            cluster_dimensions: vec!["error_type".into(), "tool_name".into()],
            traces_path: "traces.json".into(),
            auto_update: true,
            min_cluster_size: 1,
        }
    }
}

/// Orchestrates the nightly data flywheel pipeline.
///
/// Collects error traces, clusters them, generates suggestions, and optionally auto-updates tool descriptions.
///
/// # Phase G: Data Flywheel
///
/// NightlyPipeline 支持可选的建议应用回调（`on_suggestion`）。当启用了
/// `auto_update` 时，每个建议生成后都会调用此回调，允许外部系统
/// （如 KnowledgeStore、ToolRegistry）动态应用改进。
///
/// ```rust,ignore
/// use std::sync::Arc;
/// let pipeline = NightlyPipeline::new("traces.json")
///     .with_on_suggestion(Arc::new(|s| {
///         println!("Apply: {} - {}", s.title, s.description);
///     }));
/// ```
/// Orchestrates the nightly data flywheel pipeline.
pub struct NightlyPipeline {
    /// Pipeline configuration.
    config: PipelineConfig,
    /// 可选的建议应用回调（Phase G：数据飞轮闭环）。
    /// 每次生成建议后调用，用于将建议应用到 Knowledge/Tool 系统。
    #[allow(clippy::type_complexity)]
    on_suggestion: Option<Arc<dyn Fn(&Suggestion) + Send + Sync>>,
}

impl NightlyPipeline {
    /// Create a new nightly pipeline with default configuration.
    pub fn new(traces_path: &str) -> Self {
        Self {
            config: PipelineConfig {
                traces_path: traces_path.into(),
                ..Default::default()
            },
            on_suggestion: None,
        }
    }

    /// Create a pipeline with a custom configuration.
    pub fn with_config(config: PipelineConfig) -> Self {
        Self {
            config,
            on_suggestion: None,
        }
    }

    /// 设置建议应用回调（Phase G）。
    ///
    /// 当 `auto_update` 启用时，每个建议生成后都会调用此回调。
    /// 回调接收 `&Suggestion`，可用于更新 KnowledgeStore、ToolRegistry 等。
    pub fn with_on_suggestion(mut self, callback: Arc<dyn Fn(&Suggestion) + Send + Sync>) -> Self {
        self.on_suggestion = Some(callback);
        self
    }

    /// Run the full pipeline: collect   ?cluster   ?suggest   ?auto-update.
    ///
    /// Returns a report summarizing what happened.
    pub fn run(&self, traces: Vec<ErrorTrace>) -> PipelineReport {
        let traces_processed = traces.len();

        // Step 1: Cluster
        let analyzer =
            FailureAnalyzer::with_dimensions(self.config.cluster_dimensions.clone());
        let mut clusters = analyzer.cluster_by(&traces);

        // Filter by minimum cluster size
        clusters.retain(|c| c.count >= self.config.min_cluster_size);

        let clusters_found = clusters.len();

        // Step 2: Suggest
        let suggester = ImprovementSuggester::new();
        let suggestions = suggester.suggest(&clusters);
        let suggestions_count = suggestions.len();

        // Step 3: Auto-update (simulated -- in production this would modify
        // tool registry descriptions)
        let tool_descriptions_updated = if self.config.auto_update && !suggestions.is_empty()
        {
            self.apply_suggestions(&suggestions)
        } else {
            false
        };

        let summary = format!(
            "Processed {} traces -> {} clusters -> {} suggestions (auto-update: {})",
            traces_processed,
            clusters_found,
            suggestions_count,
            if tool_descriptions_updated { "yes" } else { "no" }
        );

        PipelineReport {
            traces_processed,
            clusters_found,
            suggestions_count,
            suggestions,
            tool_descriptions_updated,
            summary,
        }
    }

    /// Apply suggestions to tool descriptions or knowledge system.
    ///
    /// Phase G: 如果设置了 `on_suggestion` 回调，对每个建议调用回调。
    /// 否则仅记录日志（模拟更新）。
    fn apply_suggestions(&self, suggestions: &[Suggestion]) -> bool {
        for s in suggestions {
            // 如果设置了回调，委托给外部系统处理
            if let Some(ref callback) = self.on_suggestion {
                callback(s);
            } else {
                // 回退：仅记录日志
                if let Some(ref tool) = s.target_tool {
                    tracing::info!(
                        target_tool = tool,
                        suggestion = %s.title,
                        "flywheel: auto-update tool description"
                    );
                } else {
                    tracing::info!(
                        suggestion = %s.title,
                        "flywheel: plan-level suggestion (no target tool)"
                    );
                }
            }
        }
        !suggestions.is_empty()
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
        turn_count: usize,
        message: &str,
    ) -> ErrorTrace {
        ErrorTrace {
            error_type: error_type.into(),
            tool_name: tool_name.into(),
            language: language.into(),
            file_pattern: "*.rs".into(),
            turn_count,
            message: message.into(),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn pipeline_run_with_traces() {
        let traces = vec![
            trace("timeout", "bash", "rust", 2, "timed out"),
            trace("timeout", "bash", "rust", 5, "timed out again"),
            trace("permission_denied", "read_file", "rust", 1, "denied"),
        ];

        let pipeline = NightlyPipeline::new("test_traces.json");
        let report = pipeline.run(traces);

        assert_eq!(report.traces_processed, 3);
        assert!(report.clusters_found >= 2); // at least 2 unique error_type+tool combos
        assert!(report.suggestions_count > 0);
        assert!(report.tool_descriptions_updated);
        assert!(report.summary.contains("Processed 3 traces"));
    }

    #[test]
    fn pipeline_run_empty_traces() {
        let pipeline = NightlyPipeline::new("empty.json");
        let report = pipeline.run(vec![]);

        assert_eq!(report.traces_processed, 0);
        assert_eq!(report.clusters_found, 0);
        assert_eq!(report.suggestions_count, 0);
        assert!(!report.tool_descriptions_updated);
    }

    #[test]
    fn pipeline_with_min_cluster_size() {
        let config = PipelineConfig {
            min_cluster_size: 3,
            ..Default::default()
        };
        let pipeline = NightlyPipeline::with_config(config);

        let traces = vec![
            trace("timeout", "bash", "rust", 1, "msg"),
            trace("timeout", "bash", "rust", 2, "msg"),
            // Only 2 traces with same error_type+tool_name   ?below threshold
        ];

        let report = pipeline.run(traces);
        assert_eq!(report.clusters_found, 0);
        assert_eq!(report.suggestions_count, 0);
    }

    #[test]
    fn pipeline_with_custom_dimensions() {
        let config = PipelineConfig {
            cluster_dimensions: vec!["language".into()],
            ..Default::default()
        };
        let pipeline = NightlyPipeline::with_config(config);

        let traces = vec![
            trace("timeout", "bash", "rust", 1, "msg"),
            trace("timeout", "bash", "python", 1, "msg"),
        ];

        let report = pipeline.run(traces);
        assert_eq!(report.clusters_found, 2); // rust + python
    }

    #[test]
    fn pipeline_report_serde_roundtrip() {
        let report = PipelineReport {
            traces_processed: 10,
            clusters_found: 3,
            suggestions_count: 5,
            suggestions: vec![],
            tool_descriptions_updated: true,
            summary: "test".into(),
        };

        let json = serde_json::to_string(&report).unwrap();
        let parsed: PipelineReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, parsed);
    }

    #[test]
    fn pipeline_config_default() {
        let config = PipelineConfig::default();
        assert_eq!(config.cluster_dimensions, vec!["error_type", "tool_name"]);
        assert!(config.auto_update);
        assert_eq!(config.min_cluster_size, 1);
    }

    #[test]
    fn pipeline_auto_update_disabled() {
        let config = PipelineConfig {
            auto_update: false,
            ..Default::default()
        };
        let pipeline = NightlyPipeline::with_config(config);

        let traces = vec![trace("timeout", "bash", "rust", 1, "msg")];
        let report = pipeline.run(traces);

        assert!(!report.tool_descriptions_updated);
        assert!(report.suggestions_count > 0); // suggestions still generated
    }

    #[test]
    fn pipeline_integration_full_flow() {
        // Simulate a realistic set of traces from a nightly run
        let traces: Vec<ErrorTrace> = vec![
            trace("timeout", "bash", "rust", 1, "cargo build timed out"),
            trace("timeout", "bash", "rust", 2, "cargo test timed out"),
            trace("timeout", "bash", "rust", 3, "cargo check timed out"),
            trace("permission_denied", "read_file", "typescript", 1, "EACCES"),
            trace("permission_denied", "read_file", "typescript", 2, "EACCES"),
            trace("syntax_error", "write_file", "python", 1, "invalid syntax"),
            trace("tool_not_found", "bash", "unknown", 1, "not found"),
        ];

        let pipeline = NightlyPipeline::new("nightly_traces.json");
        let report = pipeline.run(traces);

        assert_eq!(report.traces_processed, 7);
        assert!(report.clusters_found >= 4); // 4 unique error_type+tool combos
        assert!(report.suggestions_count >= 4);
        assert!(report.tool_descriptions_updated);
        assert!(report.summary.contains("7 traces"));
    }
}
