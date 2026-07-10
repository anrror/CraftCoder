//! ImprovementSuggester -- generates actionable suggestions from failure clusters.
//!
//! Each cluster is analyzed to produce one or more suggestions that could
//! prevent the same class of errors in the future. Suggestions target tool
//! descriptions, agent behavior, or configuration changes.

use crate::flywheel::{ClusterKey, FailureCluster, Suggestion};
use std::collections::HashMap;

/// Generates improvement suggestions from failure clusters.
///
/// Uses pattern matching on cluster dimensions to produce targeted,
/// actionable suggestions.
/// ImprovementSuggester generates actionable suggestions from failure clusters.
#[derive(Clone, Debug, Default)]
pub struct ImprovementSuggester {
    /// Custom suggestion rules: dimension value -> suggestion template.
    /// Overrides built-in rules when present.
    custom_rules: HashMap<String, String>,
}

impl ImprovementSuggester {
    /// Create a new suggester with built-in rules.

    pub fn new() -> Self {
        Self {
            custom_rules: HashMap::new(),
        }
    }

    /// Add a custom suggestion rule.
    ///
    /// The key is a dimension value pattern (e.g., `"timeout"`), and the value
    /// is a suggestion template.

    /// key is a dimension value pattern (e.g. "timeout"), value is a suggestion template.
    pub fn add_rule(&mut self, pattern: String, template: String) {
        self.custom_rules.insert(pattern, template);
    }

    /// Generate suggestions from a list of failure clusters.
    ///
    /// Returns one or more suggestions per cluster, sorted by priority
    /// (highest first).

    pub fn suggest(&self, clusters: &[FailureCluster]) -> Vec<Suggestion> {
        let mut suggestions: Vec<Suggestion> = Vec::new();

        for cluster in clusters {
            suggestions.extend(self.suggest_for_cluster(cluster));
        }

        // Sort by priority descending
        suggestions.sort_by_key(|b| std::cmp::Reverse(b.priority));

        suggestions
    }

    /// Generate suggestions for a single cluster.
    fn suggest_for_cluster(&self, cluster: &FailureCluster) -> Vec<Suggestion> {
        let mut suggestions = Vec::new();

        let error_type = cluster
            .key
            .dimensions
            .get("error_type")
            .map(|s| s.as_str())
            .unwrap_or("");
        let tool_name = cluster
            .key
            .dimensions
            .get("tool_name")
            .map(|s| s.as_str())
            .unwrap_or("");
        let language = cluster
            .key
            .dimensions
            .get("language")
            .map(|s| s.as_str())
            .unwrap_or("");

        // Check custom rules first
        if let Some(template) = self.custom_rules.get(error_type) {
            suggestions.push(self.make_suggestion(
                template,
                tool_name,
                cluster.count,
                &cluster.key,
            ));
        }
        if let Some(template) = self.custom_rules.get(tool_name) {
            suggestions.push(self.make_suggestion(
                template,
                tool_name,
                cluster.count,
                &cluster.key,
            ));
        }

        // Built-in rules
        match error_type {
            "timeout" => suggestions.push(self.make_suggestion(
                &format!(
                    "Add timeout configuration hint to '{}' tool description. \
                     Consider mentioning default timeout and how to increase it.",
                    tool_name
                ),
                tool_name,
                cluster.count,
                &cluster.key,
            )),
            "permission_denied" => suggestions.push(self.make_suggestion(
                &format!(
                    "Add permission requirements section to '{}' tool description. \
                     List required permissions and common fixes.",
                    tool_name
                ),
                tool_name,
                cluster.count,
                &cluster.key,
            )),
            "syntax_error" => suggestions.push(self.make_suggestion(
                &format!(
                    "Add syntax examples to '{}' tool description. \
                     Include common patterns and anti-patterns.",
                    tool_name
                ),
                tool_name,
                cluster.count,
                &cluster.key,
            )),
            "build_failure" => suggestions.push(self.make_suggestion(
                &format!(
                    "Add build troubleshooting section to '{}' tool description. \
                     Include common build errors and resolutions.",
                    tool_name
                ),
                tool_name,
                cluster.count,
                &cluster.key,
            )),
            "tool_not_found" => suggestions.push(self.make_suggestion(
                &format!(
                    "Add installation prerequisites section to '{}' tool description. \
                     List required system dependencies.",
                    tool_name
                ),
                tool_name,
                cluster.count,
                &cluster.key,
            )),
            _ => {}
        }

        // Language-specific suggestions
        if !language.is_empty() && language != "unknown" {
            suggestions.push(self.make_suggestion(
                &format!(
                    "Consider adding language-specific guidance for '{}' in tool descriptions. \
                     Common patterns for {} projects may need documentation.",
                    language, language
                ),
                tool_name,
                cluster.count.saturating_div(2).max(1),
                &cluster.key,
            ));
        }

        // High-frequency clusters get a general improvement suggestion
        if cluster.count >= 5 {
            suggestions.push(self.make_suggestion(
                &format!(
                    "High-frequency error cluster detected ({} occurrences). \
                     Review '{}' tool for systemic issues. \
                     Consider adding pre-flight checks or validation.",
                    cluster.count, tool_name
                ),
                tool_name,
                cluster.count,
                &cluster.key,
            ));
        }

        suggestions
    }

    /// Build a single suggestion struct.
    fn make_suggestion(
        &self,
        description: &str,
        tool_name: &str,
        priority: usize,
        source_cluster: &ClusterKey,
    ) -> Suggestion {
        let title = if tool_name.is_empty() {
            "General improvement".into()
        } else {
            format!("Improve '{}' tool description", tool_name)
        };

        Suggestion {
            title,
            description: description.into(),
            target_tool: if tool_name.is_empty() {
                None
            } else {
                Some(tool_name.into())
            },
            priority,
            source_cluster: source_cluster.clone(),
        }
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

    fn cluster(error_type: &str, tool_name: &str, count: usize) -> FailureCluster {
        cluster_with_lang(error_type, tool_name, "rust", count)
    }

    fn cluster_with_lang(
        error_type: &str,
        tool_name: &str,
        language: &str,
        count: usize,
    ) -> FailureCluster {
        let traces: Vec<ErrorTrace> = (0..count)
            .map(|i| trace(error_type, tool_name, language, i + 1, "error msg"))
            .collect();
        let mut dims = std::collections::BTreeMap::new();
        dims.insert("error_type".into(), error_type.into());
        dims.insert("tool_name".into(), tool_name.into());
        dims.insert("language".into(), language.into());
        FailureCluster::new(ClusterKey::new(dims), traces)
    }

    #[test]
    fn suggest_timeout_cluster() {
        let suggester = ImprovementSuggester::new();
        let clusters = vec![cluster("timeout", "bash", 3)];
        let suggestions = suggester.suggest(&clusters);

        assert!(!suggestions.is_empty());
        assert!(suggestions[0].description.contains("timeout"));
        assert_eq!(suggestions[0].target_tool.as_deref(), Some("bash"));
    }

    #[test]
    fn suggest_permission_denied() {
        let suggester = ImprovementSuggester::new();
        let clusters = vec![cluster("permission_denied", "read_file", 2)];
        let suggestions = suggester.suggest(&clusters);

        assert!(!suggestions.is_empty());
        assert!(suggestions[0].description.contains("permission"));
    }

    #[test]
    fn suggest_syntax_error() {
        let suggester = ImprovementSuggester::new();
        let clusters = vec![cluster("syntax_error", "write_file", 1)];
        let suggestions = suggester.suggest(&clusters);

        assert!(!suggestions.is_empty());
        assert!(suggestions[0].description.contains("syntax"));
    }

    #[test]
    fn suggest_build_failure() {
        let suggester = ImprovementSuggester::new();
        let clusters = vec![cluster("build_failure", "bash", 2)];
        let suggestions = suggester.suggest(&clusters);

        assert!(!suggestions.is_empty());
        assert!(suggestions[0].description.contains("build"));
    }

    #[test]
    fn suggest_tool_not_found() {
        let suggester = ImprovementSuggester::new();
        let clusters = vec![cluster("tool_not_found", "bash", 1)];
        let suggestions = suggester.suggest(&clusters);

        assert!(!suggestions.is_empty());
        assert!(suggestions[0].description.contains("prerequisites"));
    }

    #[test]
    fn suggest_unknown_error_type() {
        let suggester = ImprovementSuggester::new();
        // Use cluster_with_lang so language is in the dimensions
        let clusters = vec![cluster_with_lang("unknown_error", "bash", "rust", 1)];
        let suggestions = suggester.suggest(&clusters);

        // Should get language-specific suggestion
        assert!(!suggestions.is_empty());
        assert!(suggestions[0].description.contains("language-specific"));
    }

    #[test]
    fn suggest_high_frequency_cluster() {
        let suggester = ImprovementSuggester::new();
        let clusters = vec![cluster("timeout", "bash", 10)];
        let suggestions = suggester.suggest(&clusters);

        // Should have timeout suggestion + language suggestion + high-frequency suggestion
        let high_freq = suggestions
            .iter()
            .find(|s| s.description.contains("High-frequency"));
        assert!(high_freq.is_some());
        assert_eq!(
            high_freq.unwrap().description,
            "High-frequency error cluster detected (10 occurrences). \
             Review 'bash' tool for systemic issues. \
             Consider adding pre-flight checks or validation."
        );
    }

    #[test]
    fn suggest_custom_rule_overrides() {
        let mut suggester = ImprovementSuggester::new();
        suggester.add_rule("timeout".into(), "Custom timeout fix".into());

        let clusters = vec![cluster("timeout", "bash", 1)];
        let suggestions = suggester.suggest(&clusters);

        // Custom rule should appear
        let custom = suggestions
            .iter()
            .find(|s| s.description == "Custom timeout fix");
        assert!(custom.is_some());
    }

    #[test]
    fn suggest_empty_clusters() {
        let suggester = ImprovementSuggester::new();
        let suggestions = suggester.suggest(&[]);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn suggest_priority_ordering() {
        let suggester = ImprovementSuggester::new();
        let clusters = vec![
            cluster("timeout", "bash", 1),
            cluster("timeout", "bash", 10),
        ];
        let suggestions = suggester.suggest(&clusters);

        // High-frequency (10) should have higher priority than low-frequency (1)
        // Suggestions are sorted by priority descending
        for i in 1..suggestions.len() {
            assert!(
                suggestions[i - 1].priority >= suggestions[i].priority,
                "suggestions must be sorted by priority descending"
            );
        }
    }

    #[test]
    fn suggest_language_specific() {
        let suggester = ImprovementSuggester::new();
        let clusters = vec![cluster_with_lang("timeout", "bash", "python", 2)];
        let suggestions = suggester.suggest(&clusters);

        let lang_suggestion = suggestions
            .iter()
            .find(|s| s.description.contains("language-specific"));
        assert!(lang_suggestion.is_some());
        assert!(lang_suggestion.unwrap().description.contains("python"));
    }
}
