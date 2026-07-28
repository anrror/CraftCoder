//! LLM-powered semantic summarizer for the AutoCompact layer.
//!
//! Wraps a ModelClient to produce intelligent summaries of old conversation
//! turns under extreme context pressure (evict threshold, 95%+).

use std::sync::Arc;

use code_agent_protocol::Message;

use crate::context::compactor::SemanticSummarizer;
use crate::model::ModelClient;

/// LLM-powered summarizer via a ModelClient.
///
/// When AutoCompact triggers (95%+ token usage), this summarizer sends old
/// conversation turns to the model for intelligent semantic compression.
/// On LLM failure or when no tokio runtime is active, it falls back to a
/// deterministic line-counting summarizer.
pub struct LlmSummarizer {
    model: Arc<dyn ModelClient>,
}

impl LlmSummarizer {
    /// Create a new LLM summarizer wrapping the given model client.
    pub fn new(model: Arc<dyn ModelClient>) -> Self {
        Self { model }
    }
}

impl SemanticSummarizer for LlmSummarizer {
    fn summarize(&self, conversation_text: &str, system_instructions: &str) -> String {
        let prompt = format!(
            "Please provide a concise summary of the following conversation, \
             preserving key decisions, code changes, and important context. \
             Keep the summary focused and actionable.\n\n\
             System: {}\n\n---\n\n{}",
            system_instructions, conversation_text
        );
        let messages = vec![Message::UserMessage { content: prompt }];

        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                match handle.block_on(self.model.complete(&messages, &[], None)) {
                    Ok(summary) => {
                        let trimmed = summary.trim();
                        format!("[LLM Summary - AutoCompact]\n\n{}", trimmed)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "LlmSummarizer failed, using deterministic fallback");
                        deterministic_summary(conversation_text)
                    }
                }
            }
            Err(_) => deterministic_summary(conversation_text),
        }
    }
}

/// Deterministic fallback summarizer when LLM is unavailable.
fn deterministic_summary(conversation_text: &str) -> String {
    let line_count = conversation_text.lines().count();
    let preview: String = conversation_text
        .lines()
        .take(20)
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "[Deterministic Fallback - AutoCompact]\n\
         Total lines: {}\n\n\
         Preview:\n{}\n",
        line_count, preview
    )
}
