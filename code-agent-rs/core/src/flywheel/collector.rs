//! FlywheelCollector — integrates error trace collection into the Agent loop.
//!
//! This collector lives inside `Session` and accumulates `ErrorTrace`s
//! during ReAct loop execution. After each turn, it runs the flywheel
//! pipeline to produce improvement suggestions.
//!
//! # Usage (inside Session)
//!
//! ```rust,ignore
//! // Record a tool error
//! self.flywheel.record(ErrorTrace {
//!     error_type: "timeout".into(),
//!     tool_name: "bash".into(),
//!     language: "rust".into(),
//!     file_pattern: "src/**/*.rs".into(),
//!     turn_count: self.turn_counter as usize,
//!     message: error_msg,
//!     timestamp: Utc::now(),
//! });
//!
//! // After turn completes, run analysis
//! let report = self.flywheel.analyze();
//! for suggestion in &report.suggestions {
//!     tracing::info!(target = "flywheel", suggestion = %suggestion.title, "flywheel suggestion");
//! }
//! ```

use super::analyzer::FailureAnalyzer;
use super::pipeline::{NightlyPipeline, PipelineReport};
use super::suggester::ImprovementSuggester;
use super::ErrorTrace;

/// Accumulates error traces during Agent execution and runs the flywheel
/// pipeline to produce improvement suggestions.
///
/// This is the glue between the Agent ReAct loop and the flywheel modules.
#[derive(Clone, Debug)]
pub struct FlywheelCollector {
    /// Accumulated error traces from the current session.
    traces: Vec<ErrorTrace>,
    /// Suggestions generated from the most recent analysis.
    suggestions: Vec<super::Suggestion>,
    /// Total number of analyses run so far.
    analysis_count: usize,
    /// Whether auto-update of tool descriptions is enabled.
    auto_update: bool,
}

impl Default for FlywheelCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl FlywheelCollector {
    /// Create a new collector with auto-update disabled by default.
    pub fn new() -> Self {
        Self {
            traces: Vec::new(),
            suggestions: Vec::new(),
            analysis_count: 0,
            auto_update: false,
        }
    }

    /// Create a collector with auto-update enabled.
    pub fn with_auto_update() -> Self {
        Self {
            auto_update: true,
            ..Self::new()
        }
    }

    /// Record a single error trace from tool execution.
    pub fn record(&mut self, trace: ErrorTrace) {
        self.traces.push(trace);
    }

    /// Run the flywheel pipeline on all accumulated traces.
    ///
    /// Returns a `PipelineReport` with clusters and suggestions.
    /// Clears accumulated traces after analysis.
    /// Stores suggestions internally for later retrieval.
    pub fn analyze(&mut self) -> PipelineReport {
        if self.traces.is_empty() {
            return PipelineReport {
                traces_processed: 0,
                clusters_found: 0,
                suggestions_count: 0,
                suggestions: vec![],
                tool_descriptions_updated: false,
                summary: "No traces to analyze.".into(),
            };
        }

        let traces = std::mem::take(&mut self.traces);
        let pipeline = NightlyPipeline::new("in_memory");
        let report = pipeline.run(traces);

        self.suggestions = report.suggestions.clone();
        self.analysis_count += 1;

        report
    }

    /// Run incremental analysis: cluster + suggest without the NightlyPipeline wrapper.
    ///
    /// Useful for lightweight analysis between turns without triggering
    /// full pipeline features (auto-update, file I/O, etc.).
    pub fn analyze_incremental(&mut self) -> Vec<super::Suggestion> {
        if self.traces.is_empty() {
            return vec![];
        }

        let traces = std::mem::take(&mut self.traces);
        let analyzer = FailureAnalyzer::new();
        let clusters = analyzer.cluster_by(&traces);
        let suggester = ImprovementSuggester::new();
        let suggestions = suggester.suggest(&clusters);

        self.suggestions = suggestions.clone();
        self.analysis_count += 1;

        suggestions
    }

    /// Get the most recent suggestions (if any).
    pub fn suggestions(&self) -> &[super::Suggestion] {
        &self.suggestions
    }

    /// Get the number of analyses performed.
    pub fn analysis_count(&self) -> usize {
        self.analysis_count
    }

    /// Get the number of accumulated traces (before next analysis).
    pub fn pending_traces(&self) -> usize {
        self.traces.len()
    }

    /// Whether auto-update is enabled.
    pub fn auto_update_enabled(&self) -> bool {
        self.auto_update
    }

    /// Reset all state (traces + suggestions).
    pub fn reset(&mut self) {
        self.traces.clear();
        self.suggestions.clear();
        self.analysis_count = 0;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flywheel::ErrorTrace;
    use chrono::Utc;

    fn make_trace(error_type: &str, tool_name: &str, message: &str) -> ErrorTrace {
        ErrorTrace {
            error_type: error_type.into(),
            tool_name: tool_name.into(),
            language: "rust".into(),
            file_pattern: "*.rs".into(),
            turn_count: 1,
            message: message.into(),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn collector_new_is_empty() {
        let collector = FlywheelCollector::new();
        assert_eq!(collector.pending_traces(), 0);
        assert!(collector.suggestions().is_empty());
        assert_eq!(collector.analysis_count(), 0);
        assert!(!collector.auto_update_enabled());
    }

    #[test]
    fn collector_records_traces() {
        let mut collector = FlywheelCollector::new();
        collector.record(make_trace("timeout", "bash", "timed out"));
        collector.record(make_trace("timeout", "bash", "timed out again"));
        assert_eq!(collector.pending_traces(), 2);
    }

    #[test]
    fn collector_analyze_empty() {
        let mut collector = FlywheelCollector::new();
        let report = collector.analyze();
        assert_eq!(report.traces_processed, 0);
        assert!(report.suggestions.is_empty());
        assert_eq!(collector.analysis_count(), 0); // no analysis run for empty
    }

    #[test]
    fn collector_analyze_with_traces() {
        let mut collector = FlywheelCollector::new();
        collector.record(make_trace("timeout", "bash", "timed out"));
        collector.record(make_trace("timeout", "bash", "timed out"));
        collector.record(make_trace("permission_denied", "read_file", "denied"));

        let report = collector.analyze();

        assert_eq!(report.traces_processed, 3);
        assert!(report.clusters_found >= 2);
        assert!(report.suggestions_count > 0);
        assert_eq!(collector.analysis_count(), 1);
        assert!(collector.suggestions().len() > 0);

        // Traces should be cleared
        assert_eq!(collector.pending_traces(), 0);
    }

    #[test]
    fn collector_analyze_incremental() {
        let mut collector = FlywheelCollector::new();
        collector.record(make_trace("timeout", "bash", "timed out"));

        let suggestions = collector.analyze_incremental();
        assert!(!suggestions.is_empty());
        assert_eq!(collector.analysis_count(), 1);

        // After incremental analyze, traces are cleared
        assert_eq!(collector.pending_traces(), 0);
    }

    #[test]
    fn collector_reset() {
        let mut collector = FlywheelCollector::new();
        collector.record(make_trace("timeout", "bash", "msg"));
        collector.record(make_trace("timeout", "bash", "msg"));
        collector.analyze();

        assert_eq!(collector.analysis_count(), 1);
        assert!(collector.suggestions().len() > 0);

        collector.reset();
        assert_eq!(collector.analysis_count(), 0);
        assert!(collector.suggestions().is_empty());
        assert_eq!(collector.pending_traces(), 0);
    }

    #[test]
    fn collector_with_auto_update() {
        let collector = FlywheelCollector::with_auto_update();
        assert!(collector.auto_update_enabled());
    }

    #[test]
    fn collector_suggestions_stored_after_analyze() {
        let mut collector = FlywheelCollector::new();
        collector.record(make_trace("syntax_error", "write_file", "bad syntax"));

        let report = collector.analyze();
        assert!(!report.suggestions.is_empty());

        // Suggestions should also be accessible via the collector
        assert!(!collector.suggestions().is_empty());
    }
}
