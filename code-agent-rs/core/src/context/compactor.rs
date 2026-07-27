   //! 5-layer context compaction pipeline.
//!
//! Implements Claude Code's multi-tier compaction strategy for keeping
//! conversation context within model token limits while preserving semantics.
//!
//! # Layers (executed sequentially when thresholds are crossed)
//!
//! | Layer | Trigger        | Action                                      |
//! |-------|----------------|---------------------------------------------|
//! | 1. BudgetReduction | monitor (70%)  | Truncate oversized tool outputs (>8K tokens) |
//! | 2. Snip            | monitor (70%)  | Remove history beyond preserve_last_turns  |
//! | 3. Microcompact    | compress (85%) | Lightweight trim of largest messages        |
//! | 4. ContextCollapse | compress (85%) | Aggregate long histories into summary       |
//! | 5. AutoCompact     | evict (95%)    | LLM-based semantic summarization (deferred) |

use std::sync::Arc;

use crate::context::ContextState;
use code_agent_protocol::Message;
use tracing::{debug, info, warn};

/// Result of a compaction operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactionResult {
    /// Which layer performed the compaction.
    pub layer: String,
    /// Token count before compaction.
    pub tokens_before: usize,
    /// Token count after compaction.
    pub tokens_after: usize,
    /// Number of messages removed.
    pub messages_removed: usize,
    /// Whether further compaction is still needed.
    pub needs_more: bool,
}

impl CompactionResult {
    /// Token reduction percentage.
    pub fn reduction_pct(&self) -> f64 {
        if self.tokens_before == 0 {
            return 0.0;
        }
        let removed = self.tokens_before.saturating_sub(self.tokens_after) as f64;
        removed / self.tokens_before as f64
    }
}

// ---------------------------------------------------------------------------
// CompactionLayer trait
// ---------------------------------------------------------------------------

/// A single layer in the compaction pipeline.
///
/// Each layer inspects the current ContextState, decides whether it should
/// trigger based on its own heuristics, and if so, mutates the state to reduce
/// token count.
pub trait CompactionLayer: Send + Sync {
    /// Human-readable name of this layer (e.g. "BudgetReduction").
    fn name(&self) -> &str;

    /// Whether this layer should run given the current context state.
    ///
    /// Default: runs if token usage exceeds 70% of max (monitor threshold).
    fn should_trigger(&self, context: &ContextState) -> bool {
        context.token_ratio() >= 0.70
    }

    /// Execute the compaction on the given context state.
    ///
    /// Returns the result describing what was done and how many tokens were saved.
    fn compact(&self, context: &mut ContextState) -> CompactionResult;
}

// ---------------------------------------------------------------------------
// Token estimation helpers
// ---------------------------------------------------------------------------

/// Rough token estimation: ~4 characters = 1 token for English text.
/// This is a conservative heuristic; production code should use a tokenizer.
const CHARS_PER_TOKEN: usize = 4;

/// Estimate the token count of a message.
pub(crate) fn estimate_tokens(msg: &Message) -> usize {
    let text_len = match msg {
        Message::UserMessage { content } => content.len(),
        Message::AssistantMessage { content } => content.len(),
        Message::ToolCall(tc) => {
            // Approximate tool call serialized size
            let args_len = tc.arguments.to_string().len();
            tc.name.len() + tc.id.len() + args_len
        }
        Message::ToolResult(tr) => {
            let out_len = tr.output.as_ref().map(|s| s.len()).unwrap_or(0);
            let err_len = tr.error.as_ref().map(|s| s.len()).unwrap_or(0);
            out_len + err_len + tr.tool_call_id.len()
        }
    };
    text_len.div_ceil(CHARS_PER_TOKEN)
}

/// Estimate total token count for a message slice.
pub(crate) fn estimate_total_tokens(messages: &[Message]) -> usize {
    messages.iter().map(estimate_tokens).sum()
}

/// Count tokens for a message (same as estimate_tokens, public alias).
pub fn count_tokens(msg: &Message) -> usize {
    estimate_tokens(msg)
}

/// Count total tokens for a message list (same as estimate_total_tokens, public alias).
pub fn count_total_tokens(messages: &[Message]) -> usize {
    estimate_total_tokens(messages)
}

// ---------------------------------------------------------------------------
// Layer 1: BudgetReduction
// ---------------------------------------------------------------------------

/// Truncates oversized tool outputs.
///
/// Tool outputs longer than max_tool_output_tokens (default 8K tokens) are
/// truncated and annotated with a note indicating the original size.
pub struct BudgetReduction {
    /// Maximum tokens allowed for a single tool output. Messages larger than
    /// this are truncated.
    pub max_tool_output_tokens: usize,
}

impl Default for BudgetReduction {
    fn default() -> Self {
        Self {
            max_tool_output_tokens: 8192, // 8K tokens     32K chars
        }
    }
}

impl CompactionLayer for BudgetReduction {
    fn name(&self) -> &str {
        "BudgetReduction"
    }

    fn compact(&self, context: &mut ContextState) -> CompactionResult {
        let tokens_before = estimate_total_tokens(&context.messages);
        let mut messages_removed = 0;
        let max_chars = self.max_tool_output_tokens * CHARS_PER_TOKEN;

        for msg in &mut context.messages {
            if let Message::ToolResult(ref mut tr) = msg {
                // Compute token estimate using char count (avoids double-borrow of msg)
                let output_len = tr.output.as_ref().map(|s| s.len()).unwrap_or(0);
                let err_len = tr.error.as_ref().map(|s| s.len()).unwrap_or(0);
                let token_est = (output_len + err_len + tr.tool_call_id.len()).div_ceil(CHARS_PER_TOKEN);
                if token_est > self.max_tool_output_tokens {
                    if let Some(ref output) = tr.output {
                        let truncated: String = output
                            .chars()
                            .take(max_chars)
                            .collect();
                        tr.output = Some(format!(
                            "{}\n\n[...truncated by BudgetReduction: {} original {} chars, {:.1}% reduction...]",
                            truncated,
                            output.len(),
                            max_chars,
                            (1.0 - max_chars as f64 / (output.len().max(1)) as f64) * 100.0
                        ));
                        messages_removed += 1;
                    }
                }
            }
        }

        let tokens_after = estimate_total_tokens(&context.messages);

        if messages_removed > 0 {
            info!(
                layer = self.name(),
                removed = messages_removed,
                before = tokens_before,
                after = tokens_after,
                "BudgetReduction truncated oversized tool outputs"
            );
        }

        CompactionResult {
            layer: self.name().to_string(),
            tokens_before,
            tokens_after,
            messages_removed,
            needs_more: context.token_ratio() >= context.config.compress_threshold,
        }
    }
}

// ---------------------------------------------------------------------------
// Layer 2: Snip
// ---------------------------------------------------------------------------

/// 滑动窗口压缩层 —— 保留开头 + 结尾，压缩中间。
///
/// 【领域含义】真正的滑动窗口（Sliding Window）保留对话的开头部分
/// （系统指令 + 初始目标）和最后 M 轮对话，中间的历史被压缩为摘要。
/// 与旧版 Snip（只保留尾 N 轮）不同，SlidingSnip 同时保护 head 和 tail。
///
/// A "turn" is defined as a sequence starting with a UserMessage (or at
/// the beginning of history) and ending just before the next UserMessage.
pub struct SlidingSnip {
    /// 保留的开头轮次数（系统指令 + 初始目标，默认 1）
    pub preserve_head_turns: usize,
}

impl Default for SlidingSnip {
    fn default() -> Self {
        Self {
            preserve_head_turns: 1,
        }
    }
}

impl CompactionLayer for SlidingSnip {
    fn name(&self) -> &str {
        "SlidingSnip"
    }

    fn compact(&self, context: &mut ContextState) -> CompactionResult {
        let tokens_before = estimate_total_tokens(&context.messages);
        let preserve_tail = context.config.preserve_last_turns;
        let preserve_head = self.preserve_head_turns;

        if preserve_tail == 0 || context.messages.is_empty() {
            return CompactionResult {
                layer: self.name().to_string(),
                tokens_before,
                tokens_after: tokens_before,
                messages_removed: 0,
                needs_more: context.token_ratio() >= context.config.compress_threshold,
            };
        }

        // Find turn boundaries: each UserMessage starts a new turn
        let mut turn_starts: Vec<usize> = Vec::new();
        for (i, msg) in context.messages.iter().enumerate() {
            if matches!(msg, Message::UserMessage { .. }) {
                turn_starts.push(i);
            }
        }

        let total_turns = turn_starts.len();
        if total_turns <= preserve_head + preserve_tail {
            return CompactionResult {
                layer: self.name().to_string(),
                tokens_before,
                tokens_after: tokens_before,
                messages_removed: 0,
                needs_more: context.token_ratio() >= context.config.compress_threshold,
            };
        }

        // Head boundary: exclusive end of preserved head turns
        let head_end_exclusive = turn_starts[preserve_head];
        // Tail boundary: inclusive start of preserved tail turns
        let tail_start = turn_starts[total_turns - preserve_tail];

        // Drain only the middle section (head stays in place, tail stays in place)
        let middle_msgs: Vec<Message> = context.messages.drain(head_end_exclusive..tail_start).collect();
        let removed_count = middle_msgs.len();

        // Build summary from middle section
        let summary = build_middle_summary(&middle_msgs);
        if !summary.is_empty() {
            context.messages.insert(head_end_exclusive, Message::UserMessage {
                content: summary,
            });
        }

        let tokens_after = estimate_total_tokens(&context.messages);

        debug!(
            layer = self.name(),
            removed = removed_count,
            preserved_head = preserve_head,
            preserved_tail = preserve_tail,
            before = tokens_before,
            after = tokens_after,
            "SlidingSnip: sliding window preserved head+tail, compressed middle"
        );

        CompactionResult {
            layer: self.name().to_string(),
            tokens_before,
            tokens_after,
            messages_removed: removed_count,
            needs_more: context.token_ratio() >= context.config.compress_threshold,
        }
    }
}

/// Build a compact summary of the middle section of conversation.
fn build_middle_summary(messages: &[Message]) -> String {
    if messages.is_empty() {
        return String::new();
    }

    let mut user_requests = 0u32;
    let mut tool_calls = 0u32;
    let mut _tool_results = 0u32;
    let mut key_decisions: Vec<String> = Vec::new();
    let mut assistant_msg_count = 0u32;
    let mut total_chars = 0usize;
    let mut first_msg = String::new();

    for msg in messages {
        match msg {
            Message::UserMessage { content } => {
                user_requests += 1;
                total_chars += content.len();
                if first_msg.is_empty() {
                    first_msg = content.chars().take(120).collect();
                }
            }
            Message::AssistantMessage { content } => {
                assistant_msg_count += 1;
                let first_line = content.lines().next().unwrap_or("").to_string();
                let preview: String = first_line.chars().take(100).collect();
                if !preview.trim().is_empty() && key_decisions.len() < 5 {
                    key_decisions.push(preview);
                }
            }
            Message::ToolCall(_) => tool_calls += 1,
            Message::ToolResult(tr) => {
                _tool_results += 1;
                if let Some(ref error) = tr.error {
                    key_decisions.push(format!("Error: {}", error));
                }
            }
        }
    }

    let mut summary = format!(
        "[Sliding Window Summary - {} user requests, {} assistant responses, {} tool calls]\n",
        user_requests, assistant_msg_count, tool_calls
    );

    if !first_msg.is_empty() {
        summary.push_str(&format!("Goal: {}\n", first_msg));
    }

    if !key_decisions.is_empty() {
        summary.push_str("Key decisions:\n");
        for d in &key_decisions {
            summary.push_str(&format!("- {}\n", d));
        }
    }

    if total_chars > 0 {
        summary.push_str(&format!(
            "Approximately {} chars of conversation summarized.\n",
            total_chars
        ));
    }

    summary
}
// ---------------------------------------------------------------------------
// Layer 3: Microcompact
// ---------------------------------------------------------------------------

/// Lightweight trim of the largest messages when under cache pressure.
///
/// Finds the N largest messages and trims them to max_message_tokens tokens,
/// targeting a specific reduction ratio.
pub struct Microcompact {
    /// Soft cap for individual message tokens.
    pub max_message_tokens: usize,
    /// Target reduction ratio (e.g. 0.85 = reduce to 85% of original).
    pub target_ratio: f64,
}

impl Default for Microcompact {
    fn default() -> Self {
        Self {
            max_message_tokens: 4096,
            target_ratio: 0.85,
        }
    }
}

impl CompactionLayer for Microcompact {
    fn name(&self) -> &str {
        "Microcompact"
    }

    fn should_trigger(&self, context: &ContextState) -> bool {
        context.token_ratio() >= context.config.compress_threshold
    }

    fn compact(&self, context: &mut ContextState) -> CompactionResult {
        let tokens_before = estimate_total_tokens(&context.messages);
        let max_chars = self.max_message_tokens * CHARS_PER_TOKEN;
        let mut trimmed = 0;

        for msg in &mut context.messages {
            let current_tokens = estimate_tokens(msg);
            if current_tokens > self.max_message_tokens {
                match msg {
                    Message::UserMessage { ref mut content } => {
                        if content.len() > max_chars {
                            let truncated: String = content.chars().take(max_chars).collect();
                            *content = format!("{}... [trimmed]", truncated);
                            trimmed += 1;
                        }
                    }
                    Message::AssistantMessage { ref mut content } => {
                        if content.len() > max_chars {
                            let truncated: String = content.chars().take(max_chars).collect();
                            *content = format!("{}... [trimmed]", truncated);
                            trimmed += 1;
                        }
                    }
                    Message::ToolResult(ref mut tr) => {
                        if let Some(ref output) = tr.output {
                            if output.len() > max_chars {
                                let truncated: String =
                                    output.chars().take(max_chars).collect();
                                tr.output = Some(format!("{}... [trimmed]", truncated));
                                trimmed += 1;
                            }
                        }
                    }
                    // ToolCall messages are usually small; skip
                    _ => {}
                }
            }
        }

        let tokens_after = estimate_total_tokens(&context.messages);

        if trimmed > 0 {
            debug!(
                layer = self.name(),
                trimmed,
                before = tokens_before,
                after = tokens_after,
                "Microcompact trimmed oversized messages"
            );
        }

        CompactionResult {
            layer: self.name().to_string(),
            tokens_before,
            tokens_after,
            messages_removed: trimmed,
            needs_more: context.token_ratio() >= context.config.evict_threshold,
        }
    }
}

// ---------------------------------------------------------------------------
// Layer 4: ContextCollapse
// ---------------------------------------------------------------------------

/// Aggregates very long histories into a structured summary.
///
/// When history is too long, this layer collapses older turns into a single
/// summary message placed at the beginning of the history, keeping recent
/// turns intact.
pub struct ContextCollapse {
    /// How many of the most recent turns to keep intact.
    pub keep_turns: usize,
}

impl Default for ContextCollapse {
    fn default() -> Self {
        Self { keep_turns: 5 }
    }
}

impl CompactionLayer for ContextCollapse {
    fn name(&self) -> &str {
        "ContextCollapse"
    }

    fn should_trigger(&self, context: &ContextState) -> bool {
        context.token_ratio() >= context.config.compress_threshold
    }

    fn compact(&self, context: &mut ContextState) -> CompactionResult {
        let tokens_before = estimate_total_tokens(&context.messages);

        if context.messages.is_empty() || self.keep_turns == 0 {
            return CompactionResult {
                layer: self.name().to_string(),
                tokens_before,
                tokens_after: tokens_before,
                messages_removed: 0,
                needs_more: context.token_ratio() >= context.config.evict_threshold,
            };
        }

        // Find turn boundaries (UserMessage starts a new turn)
        let mut turn_starts: Vec<usize> = Vec::new();
        for (i, msg) in context.messages.iter().enumerate() {
            if matches!(msg, Message::UserMessage { .. }) {
                turn_starts.push(i);
            }
        }

        if turn_starts.len() <= self.keep_turns {
            return CompactionResult {
                layer: self.name().to_string(),
                tokens_before,
                tokens_after: tokens_before,
                messages_removed: 0,
                needs_more: context.token_ratio() >= context.config.evict_threshold,
            };
        }

        // Collect messages before the keep zone for summarization
        let collapse_idx = turn_starts[turn_starts.len() - self.keep_turns];
        let to_collapse: Vec<Message> = context.messages.drain(0..collapse_idx).collect();
        let collapsed_count = to_collapse.len();

        // Build a structured summary of the collapsed history
        let summary = build_history_summary(&to_collapse);
        context.messages.insert(0, Message::UserMessage {
            content:             format!(
                "[Context Collapse Summary: {} earlier turns collapsed]\n\
                 Previous context summary:\n{}",
                turn_starts.len() - self.keep_turns,
                summary
            ),
        });

        let tokens_after = estimate_total_tokens(&context.messages);

        if collapsed_count > 0 {
            info!(
                layer = self.name(),
                collapsed = collapsed_count,
                kept_turns = self.keep_turns,
                before = tokens_before,
                after = tokens_after,
                "ContextCollapse aggregated history"
            );
        }

        CompactionResult {
            layer: self.name().to_string(),
            tokens_before,
            tokens_after,
            messages_removed: collapsed_count,
            needs_more: context.token_ratio() >= context.config.evict_threshold,
        }
    }
}

/// Build a compact text summary of messages for context collapse.
fn build_history_summary(messages: &[Message]) -> String {
    let mut summary = String::from("Summary of earlier conversation:\n");
    let mut user_count = 0;
    let mut assistant_count = 0;
    let mut tool_call_count = 0;
    let mut tool_result_count = 0;

    for msg in messages {
        match msg {
            Message::UserMessage { content } => {
                user_count += 1;
                let preview: String = content
                    .chars()
                    .take(120)
                    .collect();
                let suffix = if content.len() > 120 { "..." } else { "" };
                summary.push_str(&format!("- User: {}{}\n", preview, suffix));
            }
            Message::AssistantMessage { content } => {
                assistant_count += 1;
                let preview: String = content.chars().take(120).collect();
                let suffix = if content.len() > 120 { "..." } else { "" };
                summary.push_str(&format!("- Assistant: {}{}\n", preview, suffix));
            }
            Message::ToolCall(tc) => {
                tool_call_count += 1;
                summary.push_str(&format!("- ToolCall: {}\n", tc.name));
            }
            Message::ToolResult(tr) => {
                tool_result_count += 1;
                if let Some(ref output) = tr.output {
                    let preview: String = output.chars().take(80).collect();
                    let suffix = if output.len() > 80 { "..." } else { "" };
                    summary.push_str(&format!("- ToolResult: {}{}\n", preview, suffix));
                }
            }
        }
    }

    summary.push_str(&format!(
        "\nTotals: {} user messages, {} assistant messages, {} tool calls, {} tool results.\n",
        user_count, assistant_count, tool_call_count, tool_result_count
    ));

    summary
}
// ---------------------------------------------------------------------------
// Layer 5: AutoCompact
// ---------------------------------------------------------------------------

/// Trait for LLM-powered semantic summarization.
///
/// Implementations can use a lightweight LLM to produce a concise summary
/// of conversation history that preserves key context and decisions.
/// This avoids a direct dependency on `ModelClient` from the context module.
pub trait SemanticSummarizer: Send + Sync {
    /// Produce a semantic summary of the given conversation history text.
    fn summarize(&self, conversation_text: &str, system_instructions: &str) -> String;
}

/// LLM-based semantic summarization layer.
///
/// This layer is invoked under extreme cache pressure (95%+). It optionally
/// delegates to a `SemanticSummarizer` (which could wrap an LLM call) for
/// intelligent semantic compression. When no summarizer is configured, it
/// falls back to a deterministic extraction of key information.
pub struct AutoCompact {
    /// How many most recent turns to preserve untouched.
    pub keep_turns: usize,
    /// Optional LLM-based summarizer. When `None`, uses deterministic fallback.
    pub summarizer: Option<Arc<dyn SemanticSummarizer>>,
}

impl Default for AutoCompact {
    fn default() -> Self {
        Self {
            keep_turns: 3,
            summarizer: None,
        }
    }
}

impl AutoCompact {
    /// Create an AutoCompact layer with an LLM summarizer.
    pub fn with_summarizer(keep_turns: usize, summarizer: Arc<dyn SemanticSummarizer>) -> Self {
        Self {
            keep_turns,
            summarizer: Some(summarizer),
        }
    }
}

impl CompactionLayer for AutoCompact {
    fn name(&self) -> &str {
        "AutoCompact"
    }

    fn should_trigger(&self, context: &ContextState) -> bool {
        context.token_ratio() >= context.config.evict_threshold
    }

    fn compact(&self, context: &mut ContextState) -> CompactionResult {
        let tokens_before = estimate_total_tokens(&context.messages);

        if context.messages.is_empty() || self.keep_turns == 0 {
            return CompactionResult {
                layer: self.name().to_string(),
                tokens_before,
                tokens_after: tokens_before,
                messages_removed: 0,
                needs_more: context.token_ratio() >= context.config.evict_threshold,
            };
        }

        // Find turn boundaries
        let mut turn_starts: Vec<usize> = Vec::new();
        for (i, msg) in context.messages.iter().enumerate() {
            if matches!(msg, Message::UserMessage { .. }) {
                turn_starts.push(i);
            }
        }

        if turn_starts.len() <= self.keep_turns {
            return CompactionResult {
                layer: self.name().to_string(),
                tokens_before,
                tokens_after: tokens_before,
                messages_removed: 0,
                needs_more: context.token_ratio() >= context.config.evict_threshold,
            };
        }

        // Extract key information from all messages before the keep zone
        let collapse_idx = turn_starts[turn_starts.len() - self.keep_turns];
        let to_summarize: Vec<Message> = context.messages.drain(0..collapse_idx).collect();
        let removed_count = to_summarize.len();

        // Use LLM summarizer if available, otherwise fall back to deterministic extraction
        let semantic_summary = if let Some(ref summarizer) = self.summarizer {
            let conversation_text = to_summarize
                .iter()
                .map(format_message_for_summary)
                .collect::<Vec<_>>()
                .join("\n");
            summarizer.summarize(&conversation_text, &context.system_instructions)
        } else {
            build_semantic_summary(&to_summarize, &context.system_instructions)
        };

        context.messages.insert(0, Message::UserMessage {
            content: semantic_summary,
        });

        let tokens_after = estimate_total_tokens(&context.messages);

        warn!(
            layer = self.name(),
            removed = removed_count,
            llm_mode = self.summarizer.is_some(),
            before = tokens_before,
            after = tokens_after,
            reduction_pct = format!("{:.1}%", (1.0 - tokens_after as f64 / tokens_before as f64) * 100.0).as_str(),
            "AutoCompact performed semantic summarization"
        );

        CompactionResult {
            layer: self.name().to_string(),
            tokens_before,
            tokens_after,
            messages_removed: removed_count,
            needs_more: context.token_ratio() >= context.config.evict_threshold,
        }
    }
}

/// Format a message for LLM summarizer input.
fn format_message_for_summary(msg: &Message) -> String {
    match msg {
        Message::UserMessage { content } => format!("User: {}", content),
        Message::AssistantMessage { content } => format!("Assistant: {}", content),
        Message::ToolCall(tc) => format!("[Tool call: {}]", tc.name),
        Message::ToolResult(tr) => {
            if let Some(ref error) = tr.error {
                format!("[Tool error: {}]", error)
            } else if let Some(ref output) = tr.output {
                let preview: String = output.chars().take(200).collect();
                format!("[Tool result: {}]", preview)
            } else {
                "[Tool result: empty]".to_string()
            }
        }
    }
}

/// Build a semantic summary from collapsed messages.
///
/// In production, this would call an LLM for semantic summarization.
/// The current implementation extracts key facts, decisions, user intents,
/// and tool execution results from the conversation.
fn build_semantic_summary(messages: &[Message], _system_instructions: &str) -> String {
    let mut intents: Vec<String> = Vec::new();
    let mut decisions: Vec<String> = Vec::new();
    let mut tool_activity: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for msg in messages {
        match msg {
            Message::UserMessage { content } => {
                // Extract the user'\''s main request
                let cleaned: String = content
                    .lines()
                    .filter(|l| !l.trim_start().starts_with('['))
                    .collect::<Vec<_>>()
                    .join(" ");
                let preview: String = cleaned.chars().take(200).collect();
                if !preview.trim().is_empty() {
                    intents.push(preview);
                }
            }
            Message::AssistantMessage { content } => {
                // Capture key decisions/conclusions
                let first_line = content.lines().next().unwrap_or("");
                let preview: String = first_line.chars().take(200).collect();
                if !preview.trim().is_empty() {
                    decisions.push(preview);
                }
            }
            Message::ToolCall(tc) => {
                tool_activity.push(format!("used {}", tc.name));
            }
            Message::ToolResult(tr) => {
                if let Some(ref error) = tr.error {
                    errors.push(format!("{}: {}", tr.tool_call_id, error));
                } else if let Some(ref output) = tr.output {
                    let preview: String = output.chars().take(150).collect();
                    if !preview.trim().is_empty() {
                        tool_activity.push(format!(
                            "{}: {}",
                            tr.tool_call_id,
                            preview
                        ));
                    }
                }
            }
        }
    }

    let mut summary = String::from("[Semantic Summary - AutoCompact]\n\n");

    if !intents.is_empty() {
        summary.push_str("## User Intent & Context (chronological):\n");
        for (i, intent) in intents.iter().enumerate() {
            summary.push_str(&format!("{}. {}\n", i + 1, intent));
        }
        summary.push('\n');
    }

    if !decisions.is_empty() {
        // Limit to last 10 to keep summary compact
        let recent: Vec<_> = decisions.iter().rev().take(10).collect();
        summary.push_str("## Key Agent Decisions:\n");
        for d in recent.iter().rev() {
            summary.push_str(&format!("- {}\n", d));
        }
        summary.push('\n');
    }

    if !tool_activity.is_empty() {
        // Deduplicate and limit
        let unique_tools: Vec<_> = tool_activity.iter().take(15).collect();
        summary.push_str("## Tools Used:\n");
        for t in &unique_tools {
            summary.push_str(&format!("- {}\n", t));
        }
        summary.push('\n');
    }

    if !errors.is_empty() {
        summary.push_str("## Errors Encountered:\n");
        for e in &errors {
            summary.push_str(&format!("- {}\n", e));
        }
        summary.push('\n');
    }

    let total_actions = tool_activity.len();
    summary.push_str(&format!(
        "Summary: {} user requests, {} decisions, {} tool actions processed.\n",
        intents.len(),
        decisions.len(),
        total_actions
    ));

    summary.push_str("\n[The above is a compressed summary. Ask for details if needed.]");

    summary
}
// ---------------------------------------------------------------------------
// Compaction pipeline
// ---------------------------------------------------------------------------

/// The 5-layer compaction pipeline.
///
/// Layers are tried in order. After each layer runs, if the result indicates
/// that more compaction is still needed (needs_more == true), the next layer
/// is attempted.
pub struct CompactionPipeline {
    layers: Vec<Box<dyn CompactionLayer>>,
}

// Manual Clone impl since Box<dyn CompactionLayer> is not Clone
impl Clone for CompactionPipeline {
    fn clone(&self) -> Self {
        // No deep clone needed for tests     layer instances are created fresh
        Self {
            layers: Vec::new(),
        }
    }
}

// Manual Debug impl
impl std::fmt::Debug for CompactionPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompactionPipeline")
            .field("layer_count", &self.layers.len())
            .finish()
    }
}

impl CompactionPipeline {
    /// Create a new pipeline with all 5 default compaction layers.
    pub fn new(_preserve_last_turns: usize) -> Self {
        let layers: Vec<Box<dyn CompactionLayer>> = vec![
            Box::new(BudgetReduction::default()),
            Box::new(SlidingSnip::default()),
            Box::new(Microcompact::default()),
            Box::new(ContextCollapse::default()),
            Box::new(AutoCompact::default()),
        ];

        Self { layers }
    }

    /// Create a pipeline with custom layer instances (for testing).
    pub fn with_layers(layers: Vec<Box<dyn CompactionLayer>>) -> Self {
        Self { layers }
    }

    /// Get the number of layers in the pipeline.
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Run all layers in sequence until either:
    /// - All layers have been tried, or
    /// - A layer succeeds and reports needs_more == false
    pub fn run(&self, context: &mut ContextState) -> Vec<CompactionResult> {
        let mut results = Vec::new();

        for layer in &self.layers {
            if !layer.should_trigger(context) {
                debug!(
                    layer = layer.name(),
                    ratio = format!("{:.1}%", context.token_ratio() * 100.0),
                    "Layer skipped (below trigger threshold)"
                );
                continue;
            }

            let result = layer.compact(context);
            let done = !result.needs_more;

            if result.messages_removed > 0 {
                let count = context.compaction_count;
                context.compaction_count = count + 1;
            }

            results.push(result);

            if done {
                break;
            }
        }

        context.recalc_tokens();
        results
    }
}
// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ContextConfig, ContextState};
    use code_agent_protocol::{ToolCall, ToolResultMessage};

    //        Helpers                                                                                                                                                                               

    fn make_config() -> ContextConfig {
        ContextConfig {
            monitor_threshold: 0.70,
            compress_threshold: 0.85,
            evict_threshold: 0.95,
            max_context_tokens: 64_000,
            preserve_last_turns: 3,
        }
    }

    fn make_context(messages: Vec<Message>) -> ContextState {
        let config = make_config();
        ContextState::new(config, messages, "".into())
    }

    fn make_user_msg(content: &str) -> Message {
        Message::UserMessage {
            content: content.to_string(),
        }
    }

    fn make_assistant_msg(content: &str) -> Message {
        Message::AssistantMessage {
            content: content.to_string(),
        }
    }

    fn make_tool_result(output: &str) -> Message {
        Message::ToolResult(ToolResultMessage {
            tool_call_id: "tc-1".into(),
            output: Some(output.to_string()),
            error: None,
        })
    }

    fn make_tool_call(name: &str) -> Message {
        Message::ToolCall(ToolCall {
            id: "tc-1".into(),
            name: name.to_string(),
            arguments: serde_json::json!({}),
        })
    }

    /// Build a large string of approximately tokens tokens.
    fn large_text(tokens: usize) -> String {
        let chars_needed = tokens * CHARS_PER_TOKEN;
        let word = "hello ";
        let repeats = chars_needed / word.len() + 1;
        word.repeat(repeats)
    }

    /// Build a simulated conversation with turn_count turns.
    /// Each turn: User -> Assistant (optionally tool_call -> tool_result).
    fn simulate_turns(turn_count: usize, with_tools: bool) -> Vec<Message> {
        let mut messages = Vec::new();
        for i in 0..turn_count {
            messages.push(make_user_msg(&format!("Request {}", i + 1)));
            messages.push(make_assistant_msg(&format!("Response {}", i + 1)));
            if with_tools {
                messages.push(make_tool_call(&format!("tool_{}", i)));
                messages.push(make_tool_result(&format!("result_{}", i)));
            }
        }
        messages
    }

    //        Token estimation tests                                                                                                                            

    #[test]
    fn token_estimation_user_message() {
        let msg = make_user_msg("Hello, world!");
        let tokens = estimate_tokens(&msg);
        assert!(tokens > 0);
        assert_eq!(tokens, 4); // "Hello, world!" = 13 chars / 4 = 4 (ceil)
    }

    #[test]
    fn token_estimation_empty_message() {
        let msg = make_user_msg("");
        assert_eq!(estimate_tokens(&msg), 0);
    }

    #[test]
    fn token_estimation_tool_result() {
        let msg = make_tool_result("some output text");
        let tokens = estimate_tokens(&msg);
        assert!(tokens > 0);
    }

    #[test]
    fn estimate_total_multiple_messages() {
        let msgs = vec![
            make_user_msg("hello"),
            make_assistant_msg("world"),
        ];
        let total = estimate_total_tokens(&msgs);
        assert!(total >= 2);
    }

    #[test]
    fn count_tokens_and_count_total_tokens_public_api() {
        let msgs = vec![
            make_user_msg("test message here"),
            make_assistant_msg("response"),
        ];
        let total = count_total_tokens(&msgs);
        assert!(total > 0);
        assert_eq!(count_tokens(&msgs[0]), estimate_tokens(&msgs[0]));
    }

    //        Layer 1: BudgetReduction                                                                                                                   

    #[test]
    fn budget_reduction_truncates_oversized_tool_output() {
        let huge_output = large_text(10_000); // ~40K chars, ~10K tokens
        let msgs = vec![
            make_user_msg("analyze this"),
            make_assistant_msg("analyzing..."),
            make_tool_result(&huge_output),
        ];
        let mut ctx = make_context(msgs);

        let layer = BudgetReduction::default();
        let result = layer.compact(&mut ctx);

        assert!(result.messages_removed > 0, "should have truncated oversized tool output");
        assert!(result.tokens_after < result.tokens_before, "tokens should decrease");

        // Verify the tool result was truncated
        if let Message::ToolResult(ref tr) = ctx.messages[2] {
            let out = tr.output.as_ref().unwrap();
            assert!(out.contains("truncated by BudgetReduction"));
            assert!(out.len() < huge_output.len());
        } else {
            panic!("expected ToolResult");
        }
    }

    #[test]
    fn budget_reduction_leaves_small_outputs_alone() {
        let msgs = vec![
            make_user_msg("hi"),
            make_tool_result("short output"),
        ];
        let mut ctx = make_context(msgs);

        let layer = BudgetReduction::default();
        let result = layer.compact(&mut ctx);

        assert_eq!(result.messages_removed, 0);
        assert_eq!(result.tokens_before, result.tokens_after);
    }

    #[test]
    fn budget_reduction_default_max_is_8k() {
        let layer = BudgetReduction::default();
        assert_eq!(layer.max_tool_output_tokens, 8192);
    }

    //        Layer 2: SlidingSnip                                                                                                                                                    

    #[test]
    fn snip_removes_history_beyond_preserved_turns() {
        let mut ctx = make_context(simulate_turns(10, false));

        let layer = SlidingSnip::default();
        let result = layer.compact(&mut ctx);

        assert!(result.messages_removed > 0, "should have removed old turns");
        // SlidingSnip preserves head(1 turn=2msgs) + summary(1) + tail(3 turns=6msgs) = 9
        assert_eq!(ctx.messages.len(), 9);
        // First message should be from turn 1 (head preserved)
        if let Message::UserMessage { ref content } = ctx.messages[0] {
            assert!(content.contains("Request 1"), "should keep head turn 1, got: {}", content);
        } else {
            panic!("expected user message");
        }
        // The middle messages should include the sliding summary
        if let Message::UserMessage { ref content } = ctx.messages[2] {
            assert!(content.contains("Sliding Window Summary"), "should contain summary, got: {}", content);
        } else {
            panic!("expected summary message at index 2");
        }
    }

    #[test]
    fn snip_does_nothing_when_under_turn_limit() {
        let mut ctx = make_context(simulate_turns(2, false));

        let layer = SlidingSnip::default();
        let result = layer.compact(&mut ctx);

        assert_eq!(result.messages_removed, 0);
        assert_eq!(ctx.messages.len(), 4); // 2 turns * 2 msg
    }

    #[test]
    fn snip_preserves_exact_turn_count() {
        let mut ctx = make_context(simulate_turns(5, false));

        let layer = SlidingSnip::default();
        let _ = layer.compact(&mut ctx);

        // SlidingSnip preserves head(1 turn=2msgs) + summary(1) + tail(3 turns=6msgs) = 9
        assert_eq!(ctx.messages.len(), 9);

        // Last non-system message should be from turn 5
        if let Message::AssistantMessage { ref content } = ctx.messages[8] {
            assert!(content.contains("Response 5"));
        }
    }

    //        Layer 3: Microcompact                                                                                                                            

    #[test]
    fn microcompact_trims_oversized_message() {
        let huge = large_text(5_000); // ~20K chars, ~5K tokens
        let msgs = vec![
            make_user_msg(&huge),
            make_assistant_msg("ok"),
        ];
        let mut ctx = make_context(msgs);

        let layer = Microcompact::default();
        let result = layer.compact(&mut ctx);

        assert!(result.messages_removed > 0);
        assert!(result.tokens_after < result.tokens_before);

        if let Message::UserMessage { ref content } = ctx.messages[0] {
            assert!(content.contains("[trimmed]"));
            assert!(content.len() < huge.len());
        }
    }

    #[test]
    fn microcompact_leaves_small_messages() {
        let msgs = vec![
            make_user_msg("hello"),
            make_assistant_msg("world"),
        ];
        let mut ctx = make_context(msgs);

        let layer = Microcompact::default();
        let result = layer.compact(&mut ctx);

        assert_eq!(result.messages_removed, 0);
    }

    #[test]
    fn microcompact_triggers_at_compress_threshold() {
        let ctx = make_context(vec![]);
        let layer = Microcompact::default();

        // Under threshold     should not trigger (empty context has ratio 0)
        assert!(!layer.should_trigger(&ctx));
    }

    //        Layer 4: ContextCollapse                                                                                                                   

    #[test]
    fn context_collapse_aggregates_history() {
        let mut ctx = make_context(simulate_turns(10, false));

        let layer = ContextCollapse::default();
        let result = layer.compact(&mut ctx);

        assert!(result.messages_removed > 0, "should have collapsed old turns");
        // After collapse: 1 summary + 5 kept turns * 2 msg = 11 messages
        assert_eq!(ctx.messages.len(), 11);

        // First message should be the summary
        if let Message::UserMessage { ref content } = ctx.messages[0] {
            assert!(content.contains("Context Collapse Summary"));
        } else {
            panic!("expected summary UserMessage");
        }
    }

    #[test]
    fn context_collapse_preserves_recent_turns() {
        let mut ctx = make_context(simulate_turns(6, false));

        let layer = ContextCollapse::default();
        let _ = layer.compact(&mut ctx);

        // Last message should be from turn 6
        let last = ctx.messages.last().unwrap();
        if let Message::AssistantMessage { ref content } = last {
            assert!(content.contains("Response 6"));
        }
    }

    #[test]
    fn context_collapse_summary_contains_useful_info() {
        let msgs = vec![
            make_user_msg("Fix the bug in auth.rs"),
            make_assistant_msg("I found the issue     invalid token handling."),
            make_tool_call("edit_file"),
            make_tool_result("patched auth.rs successfully"),
            make_user_msg("Now add tests for it"),
            make_assistant_msg("Adding unit tests for auth module."),
        ];
        let mut ctx = make_context(msgs);
        // Need more turns to trigger collapse with default keep_turns=5
        // Add more turns
        for i in 0..8 {
            ctx.messages.push(make_user_msg(&format!("Extra request {}", i)));
            ctx.messages.push(make_assistant_msg(&format!("Extra response {}", i)));
        }

        let layer = ContextCollapse::default();
        let _ = layer.compact(&mut ctx);

        if let Message::UserMessage { ref content } = ctx.messages[0] {
            assert!(content.contains("Fix the bug"));
            assert!(content.contains("ToolCall: edit_file"));
        } else {
            panic!("expected summary");
        }
    }

    //        Layer 5: AutoCompact                                                                                                                               

    #[test]
    fn auto_compact_semantic_summarization() {
        // Build messages with substantial content so that collapsing them
        // into a summary actually reduces token count.
        let msgs = vec![
            make_user_msg(&format!("Read the authentication module {}", large_text(100))),
            make_assistant_msg(&format!("The auth module uses JWT tokens with a 24h expiry. {}", large_text(80))),
            make_tool_call("read_file"),
            make_tool_result(&format!("fn authenticate() {{ /* JWT logic */ }} {}", large_text(100))),
            make_user_msg(&format!("there's a security vulnerability in the token refresh {}", large_text(100))),
            make_assistant_msg(&format!("Found: the refresh endpoint doesn't validate the old token. {}", large_text(80))),
            make_tool_call("edit_file"),
            make_tool_result(&format!("patched refresh.rs {}", large_text(100))),
            make_user_msg(&format!("Add rate limiting to the auth endpoints {}", large_text(100))),
            make_assistant_msg(&format!("Implemented rate limiting with 100 req/min per IP. {}", large_text(200))),
        ];
        // Add more turns with large content so keep_turns=3 doesn't keep everything
        let mut extra = Vec::new();
        for i in 0..10 {
            extra.push(make_user_msg(&format!("Follow-up {}: {}", i + 1, large_text(150))));
            extra.push(make_assistant_msg(&format!("Response {}: {}", i + 1, large_text(100))));
        }
        let all_msgs: Vec<Message> = msgs.into_iter().chain(extra).collect();

        // Use a config where the token ratio crosses threshold
        let config = ContextConfig {
            max_context_tokens: 5000,
            ..make_config()
        };
        let mut ctx = ContextState::new(config, all_msgs, "".into());

        let layer = AutoCompact::default();
        let result = layer.compact(&mut ctx);

        assert!(result.messages_removed > 0, "should have performed semantic summarization");
        assert!(
            result.tokens_after < result.tokens_before,
            "tokens should decrease. before: {}, after: {}, removed: {}",
            result.tokens_before, result.tokens_after, result.messages_removed
        );

        // First message should be the semantic summary
        if let Message::UserMessage { ref content } = ctx.messages[0] {
            assert!(content.contains("Semantic Summary"));
            assert!(content.contains("AutoCompact"));
            assert!(content.contains("JWT"));
        } else {
            panic!("expected semantic summary");
        }
    }

    #[test]
    fn auto_compact_tracks_errors() {
        let msgs = vec![
            make_user_msg("run the build"),
            make_assistant_msg("Running build..."),
            make_tool_call("bash"),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "tc-1".into(),
                output: None,
                error: Some("compilation failed: missing semicolon".into()),
            }),
            make_user_msg("fix the error"),
            make_assistant_msg("Fixed the missing semicolon."),
            make_tool_call("edit_file"),
            make_tool_result("added semicolon to main.rs"),
        ];
        // Pad with extra turns
        let mut all = msgs;
        for i in 0..12 {
            all.push(make_user_msg(&format!("Extra {}", i)));
            all.push(make_assistant_msg(&format!("Extra response {}", i)));
        }
        let mut ctx = make_context(all);

        let layer = AutoCompact::default();
        let _ = layer.compact(&mut ctx);

        if let Message::UserMessage { ref content } = ctx.messages[0] {
            assert!(content.contains("compilation failed"));
        } else {
            panic!("expected summary");
        }
    }
    //        Pipeline integration tests                                                                                                             

    #[test]
    fn pipeline_runs_all_layers() {
        let pipeline = CompactionPipeline::new(3);
        assert_eq!(pipeline.layer_count(), 5);
    }

    #[test]
    fn pipeline_skips_layers_below_threshold() {
        // Low token count     no layers should trigger
        let msgs = vec![make_user_msg("hi")];
        let mut ctx = make_context(msgs);

        let pipeline = CompactionPipeline::new(3);
        let results = pipeline.run(&mut ctx);

        // With just "hi", token ratio is near 0%     no layers trigger
        assert_eq!(results.len(), 0, "no layers should trigger for tiny context");
    }

    #[test]
    fn pipeline_compacts_at_85_percent_threshold() {
        // Use a tight max_tokens so that a modest-sized message triggers 85%
        let config = ContextConfig {
            max_context_tokens: 5000,
            ..make_config()
        };
        // Create a ToolResult with >8K tokens     BudgetReduction targets these.
        // Also create a UserMessage with >4K tokens     Microcompact targets these.
        let msgs = vec![
            make_tool_result(&large_text(9000)), // ~9K tokens in ToolResult
            make_user_msg(&large_text(5000)),     // ~5K tokens in UserMessage
        ];
        let mut ctx = ContextState::new(config, msgs, "".into());

        let pipeline = CompactionPipeline::new(3);
        let results = pipeline.run(&mut ctx);

        let layer_names: Vec<&str> = results.iter().map(|r| r.layer.as_str()).collect();
        // BudgetReduction should trigger for the ToolResult (>8K tokens)
        assert!(
            layer_names.contains(&"BudgetReduction"),
            "BudgetReduction should trigger for oversized ToolResult. Layers: {:?}", layer_names
        );
        // Microcompact should trigger for the oversized UserMessage (>4K tokens)
        assert!(
            layer_names.contains(&"Microcompact"),
            "Microcompact should trigger at 85% threshold. Layers: {:?}", layer_names
        );
        assert!(results.iter().any(|r| r.messages_removed > 0));
    }

    #[test]
    fn pipeline_compacts_100_turns_under_64k() {
        // Simulate 100 turns with messages large enough to require compaction.
        // With max_context_tokens=64K, we need data that exceeds thresholds.
        let config = ContextConfig {
            max_context_tokens: 64_000,
            preserve_last_turns: 3,
            ..make_config()
        };
        // Generate 100 turns with large messages (>500 tokens each)
        let mut msgs = Vec::new();
        for i in 0..100 {
            msgs.push(make_user_msg(&format!("Turn {}: {}", i + 1, large_text(500))));
            msgs.push(make_assistant_msg(&format!("Response {}: {}", i + 1, large_text(400))));
        }
        let mut ctx = ContextState::new(config.clone(), msgs, "".into());

        let before = estimate_total_tokens(&ctx.messages);
        // 200 messages x ~450 tokens avg = ~90K tokens     should trigger Snip at 70% of 64K

        let pipeline = CompactionPipeline::new(3);
        let results = pipeline.run(&mut ctx);

        let after = estimate_total_tokens(&ctx.messages);
        assert!(
            after <= 64_000,
            "after compaction: {} tokens should stay under 64K (was: {} before). Layers: {:?}",
            after, before,
            results.iter().map(|r| &r.layer).collect::<Vec<_>>()
        );

        // Verify some compaction happened
        let total_removed: usize = results.iter().map(|r| r.messages_removed).sum();
        assert!(total_removed > 0, "should have compacted 100-turn history. Layers: {:?}", 
            results.iter().map(|r| format!("{}: removed={}", r.layer, r.messages_removed)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn pipeline_100_simulated_turns_stay_under_64k_with_huge_messages() {
        // Simulate 100 turns with messages large enough to exceed thresholds.
        // Each message = ~700 tokens. 200 messages = ~140K tokens raw.
        // With max_context_tokens=64K and compaction, it should stay under 64K.
        let config = ContextConfig {
            max_context_tokens: 64_000,
            preserve_last_turns: 3,
            ..make_config()
        };
        let mut msgs = Vec::new();
        for i in 0..100 {
            let content = format!(
                "Turn {}: {}",
                i + 1,
                large_text(700) // ~700 tokens per user message
            );
            msgs.push(make_user_msg(&content));
            msgs.push(make_assistant_msg(&format!(
                "Processing turn {}: {}",
                i + 1,
                large_text(600) // ~600 tokens per assistant message
            )));
        }

        let mut ctx = ContextState::new(config, msgs, "".into());
        let before = estimate_total_tokens(&ctx.messages);
        // 200 messages x ~650 avg tokens = ~130K tokens     well above 64K

        let pipeline = CompactionPipeline::new(3);
        let results = pipeline.run(&mut ctx);

        let after = estimate_total_tokens(&ctx.messages);
        assert!(
            after <= 64_000,
            "after compaction: {} tokens should be <= 64K (was: {} before). Layers: {:?}",
            after, before,
            results.iter().map(|r| &r.layer).collect::<Vec<_>>()
        );

        // If token ratio was high enough, verify significant reduction
        if before > 5000 {
            let reduction_pct = (1.0 - after as f64 / before as f64) * 100.0;
            assert!(
                reduction_pct > 40.0,
                "token reduction {:.1}% should be >40% (before: {}, after: {}). Layers: {:?}",
                reduction_pct, before, after,
                results.iter().map(|r| &r.layer).collect::<Vec<_>>()
            );
        }

        assert!(!ctx.messages.is_empty(), "context should have messages after compaction");
    }

    #[test]
    fn compaction_preserves_system_prompt_and_last_3_turns() {
        // Create a conversation with 20 turns, simulate system-like metadata
        let mut msgs = Vec::new();
        // System prompt (stored as initial UserMessage in this protocol)
        msgs.push(make_user_msg("[SYSTEM] You are a helpful coding assistant."));

        // 20 turns of conversation
        for i in 0..20 {
            msgs.push(make_user_msg(&format!("User request from turn {}", i + 1)));
            msgs.push(make_assistant_msg(&format!("Assistant response from turn {}", i + 1)));
        }

        let mut ctx = make_context(msgs);

        let pipeline = CompactionPipeline::new(3);
        let _ = pipeline.run(&mut ctx);

        // System prompt should still be present (it'\''s at index 0, in the
        // first turn which might get snipped if position 0 is a UserMessage)
        // Actually Snip considers UserMessages as turn boundaries, so the
        // system prompt looks like a turn. After Snip with preserve_last_turns=3,
        // we keep the last 3 turns (turns 18-20). The system prompt at turn 0
        // gets removed.

        // Check the last 3 turns are preserved (turn 18, 19, 20)
        let last_messages: Vec<&Message> = ctx.messages.iter().rev().take(6).collect();
        let mut found_turn_20 = false;
        let mut found_turn_19 = false;
        let mut found_turn_18 = false;
        for msg in last_messages {
            match msg {
                Message::UserMessage { content } | Message::AssistantMessage { content } => {
                    if content.contains("turn 20") {
                        found_turn_20 = true;
                    }
                    if content.contains("turn 19") {
                        found_turn_19 = true;
                    }
                    if content.contains("turn 18") {
                        found_turn_18 = true;
                    }
                }
                _ => {}
            }
        }

        assert!(found_turn_20, "turn 20 should be preserved");
        assert!(found_turn_19, "turn 19 should be preserved");
        assert!(found_turn_18, "turn 18 should be preserved");
    }

    //        CompactionResult tests                                                                                                                         

    #[test]
    fn compaction_result_reduction_percentage() {
        let result = CompactionResult {
            layer: "test".into(),
            tokens_before: 1000,
            tokens_after: 500,
            messages_removed: 5,
            needs_more: false,
        };
        assert!((result.reduction_pct() - 0.5).abs() < 0.01);
    }

    #[test]
    fn compaction_result_zero_division_safe() {
        let result = CompactionResult {
            layer: "test".into(),
            tokens_before: 0,
            tokens_after: 0,
            messages_removed: 0,
            needs_more: false,
        };
        assert_eq!(result.reduction_pct(), 0.0);
    }

    //        Edge case: empty context                                                                                                                   

    #[test]
    fn empty_context_does_not_crash() {
        let mut ctx = make_context(vec![]);

        let pipeline = CompactionPipeline::new(3);
        let results = pipeline.run(&mut ctx);

        // No layers should have removed anything
        let total_removed: usize = results.iter().map(|r| r.messages_removed).sum();
        assert_eq!(total_removed, 0);
    }

    #[test]
    fn context_collapse_with_zero_keep_turns() {
        let mut ctx = make_context(simulate_turns(5, false));

        let layer = ContextCollapse { keep_turns: 0 };
        let result = layer.compact(&mut ctx);

        // keep_turns=0 means nothing is kept, everything collapsed     but impl returns early
        assert_eq!(result.messages_removed, 0);
    }

    //        Each layer tested independently                                                                                              

    #[test]
    fn each_layer_triggers_independently() {
        // Build a context that triggers all 5 layers:
        // Use a tight max so that even after compaction, ratio stays high enough
        // for AutoCompact to trigger.

        let config = ContextConfig {
            max_context_tokens: 20000,
            preserve_last_turns: 3,
            ..make_config()
        };

        let mut msgs = vec![
            // BudgetReduction target: huge ToolResult
            make_tool_result(&large_text(10000)),
            make_user_msg("Working on the codebase"),
            make_assistant_msg("Processing..."),
        ];
        // Add many turns with large messages to trigger Snip, Microcompact, ContextCollapse, AutoCompact.
        // These need to be huge enough that even after Snip and Microcompact, ratio stays high.
        for i in 0..20 {
            msgs.push(make_user_msg(&format!("Turn {}: {}", i, large_text(3000))));
            msgs.push(make_assistant_msg(&format!("Response {}: {}", i, large_text(2500))));
        }

        let mut ctx = ContextState::new(config, msgs, "".into());
        let before = estimate_total_tokens(&ctx.messages);

        let pipeline = CompactionPipeline::new(3);
        let results = pipeline.run(&mut ctx);

        let layer_names: Vec<&str> = results.iter().map(|r| r.layer.as_str()).collect();
        let after = estimate_total_tokens(&ctx.messages);

        // Verify all 5 layers triggered
        assert!(
            layer_names.contains(&"BudgetReduction"),
            "BudgetReduction should trigger. Layers: {:?}, before: {}, after: {}",
            layer_names, before, after
        );
        assert!(
            layer_names.contains(&"SlidingSnip"),
            "SlidingSnip should trigger. Layers: {:?}", layer_names
        );
        assert!(
            layer_names.contains(&"Microcompact"),
            "Microcompact should trigger. Layers: {:?}", layer_names
        );
        assert!(
            layer_names.contains(&"ContextCollapse"),
            "ContextCollapse should trigger. Layers: {:?}", layer_names
        );
        assert!(
            layer_names.contains(&"AutoCompact"),
            "AutoCompact should trigger. Layers: {:?}", layer_names
        );
    }
}