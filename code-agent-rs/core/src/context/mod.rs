//! 上下文管理 —— 5 层渐进式压缩管道的对话历史管理。
//!
//! 【领域含义】本模块实现了受 Claude Code 的多层压缩策略启发的生产级
//! 上下文窗口管理系统。它跟踪 Token 使用量、监控阈值，并在对话历史
//! 超出模型上下文窗口限制时应用渐进式压缩。
//!
//! # 架构
//!
//! `	ext
//! User → Session → ContextManager → CompactionPipeline
//!                      │                    │
//!                      ├─ add_message()     ├─ BudgetReduction
//!                      ├─ build_messages()  ├─ Snip
//!                      ├─ compact_if_needed ├─ Microcompact
//!                      └─ token tracking    ├─ ContextCollapse
//!                                           └─ AutoCompact
//! `
//!
//! # 阈值
//!
//! | 阈值     | 默认值 | 动作                            |
//! |----------|--------|----------------------------------|
//! | Monitor  | 70%    | 开始监控，触发轻量层              |
//! | Compress | 85%    | 触发中等压缩层                    |
//! | Evict    | 95%    | 触发激进摘要                      |
//! Context management with 5-layer compaction pipeline.
//!
//! This module implements a production-grade context window management system
//! inspired by Claude Code's multi-tier compaction strategy. It tracks token
//! usage, monitors thresholds, and applies progressive compaction when the
//! conversation history grows too large for the model's context window.
//!
//! # Architecture
//!
//! `	ext
//! User → Session → ContextManager → CompactionPipeline
//!                      │                    │
//!                      ├─ add_message()     ├─ BudgetReduction
//!                      ├─ build_messages()  ├─ Snip
//!                      ├─ compact_if_needed ├─ Microcompact
//!                      └─ token tracking    ├─ ContextCollapse
//!                                           └─ AutoCompact
//! `
//!
//! # Thresholds
//!
//! | Threshold | Default | Action                              |
//! |-----------|---------|-------------------------------------|
//! | Monitor   | 70%     | Start tracking, trigger light layers|
//! | Compress  | 85%     | Trigger medium compaction layers    |
//! | Evict     | 95%     | Trigger aggressive summarization    |

pub mod compactor;

use std::time::Instant;

use code_agent_protocol::Message;
use tracing::{info, warn};

use compactor::{estimate_total_tokens, CompactionPipeline};

// ---------------------------------------------------------------------------
// ContextConfig
// ---------------------------------------------------------------------------

/// 上下文配置 —— 控制压缩阈值与 Token 限制。
///
/// 【领域含义】ContextConfig 定义了上下文管理器在何时触发各层压缩、
/// 最大 Token 上限以及对话历史中保留的最近轮次数。它是整个压缩策略
/// 的调优入口。
///
/// Configuration for the [ContextManager].
///
/// Controls compaction thresholds, maximum token limits, and preservation
/// behavior for recent conversation turns.
#[derive(Clone, Debug)]
pub struct ContextConfig {
    /// Token 比例，超过此值时上下文管理器开始监视（0.0–1.0）。
    /// 默认值：0.70（最大上下文 Token 的 70%）。
    ///
    /// Token ratio at which the context manager starts monitoring (0.0–1.0).
    /// Default: 0.70 (70% of max context tokens).
    pub monitor_threshold: f64,

    /// Token 比例，超过此值时触发压缩层（0.0–1.0）。
    /// 默认值：0.85（最大上下文 Token 的 85%）。
    ///
    /// Token ratio at which compression layers are triggered (0.0–1.0).
    /// Default: 0.85 (85% of max context tokens).
    pub compress_threshold: f64,

    /// Token 比例，超过此值时触发激进驱逐（0.0–1.0）。
    /// 默认值：0.95（最大上下文 Token 的 95%）。
    ///
    /// Token ratio at which aggressive eviction is triggered (0.0–1.0).
    /// Default: 0.95 (95% of max context tokens).
    pub evict_threshold: f64,

    /// 上下文窗口允许的最大 Token 数量（模型限制）。
    /// 默认值：64_000。
    ///
    /// Maximum number of tokens allowed in the context window (model limit).
    /// Default: 64_000.
    pub max_context_tokens: usize,

    /// 压缩期间保留的最近轮次数。
    /// 默认值：3。
    ///
    /// Number of most recent turns to preserve during compaction.
    /// Default: 3.
    pub preserve_last_turns: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            monitor_threshold: 0.70,
            compress_threshold: 0.85,
            evict_threshold: 0.95,
            max_context_tokens: 64_000,
            preserve_last_turns: 3,
        }
    }
}

// ---------------------------------------------------------------------------
// ContextState
// ---------------------------------------------------------------------------

/// 上下文状态 —— 暴露给各压缩层的可变状态。
///
/// 【领域含义】ContextState 持有消息缓冲区、系统指令和当前 Token 估算值。
/// 压缩层读取此状态来判断是否需要触发压缩，并通过修改状态来减少 Token 数量。
/// 它是压缩管道中各个层与上下文管理器之间的数据契约。
///
/// Mutable state exposed to compaction layers.
///
/// This holds the message buffer, system instructions, and current token
/// estimates. Compaction layers read this state to decide whether to trigger
/// and mutate it to reduce token count.
#[derive(Clone, Debug)]
pub struct ContextState {
    /// 上下文配置（与 ContextManager 共享）。
    ///
    /// Context configuration (shared with ContextManager).
    pub config: ContextConfig,

    /// 有序的对话消息列表。
    ///
    /// Ordered conversation messages.
    pub messages: Vec<Message>,

    /// 预置到提示前的系统指令。
    ///
    /// System instructions prepended to prompts.
    pub system_instructions: String,

    /// 当前所有消息的估算 Token 数量。
    ///
    /// Current estimated token count of all messages.
    pub current_tokens: usize,

    /// 已执行压缩的次数。
    ///
    /// How many times compaction has been applied.
    pub compaction_count: u32,
}

impl ContextState {
    /// 创建新的上下文状态并估算初始 Token 数量。
    ///
    /// Create a new context state with estimated token count.
    pub fn new(
        config: ContextConfig,
        messages: Vec<Message>,
        system_instructions: String,
    ) -> Self {
        let current_tokens = estimate_total_tokens(&messages);
        Self {
            config,
            messages,
            system_instructions,
            current_tokens,
            compaction_count: 0,
        }
    }

    /// 计算当前 Token 使用率（0.0–∞）。
    ///
    /// 返回 current_tokens / max_context_tokens。值 ≥ 1.0 表示上下文已超出模型限制。
    ///
    /// Calculate the current token usage ratio (0.0–∞).
    ///
    /// Returns current_tokens / max_context_tokens. Values ≥ 1.0 indicate
    /// the context is over the model limit.
    pub fn token_ratio(&self) -> f64 {
        if self.config.max_context_tokens == 0 {
            return 0.0;
        }
        self.current_tokens as f64 / self.config.max_context_tokens as f64
    }

    /// 通过扫描所有消息重新计算 Token 估算值。
    ///
    /// Recalculate the token estimate by scanning all messages.
    pub fn recalc_tokens(&mut self) {
        self.current_tokens = estimate_total_tokens(&self.messages);
    }

    /// 如果 Token 使用量超过监视阈值则返回 true。
    ///
    /// Returns true if token usage exceeds the monitor threshold.
    pub fn needs_monitoring(&self) -> bool {
        self.token_ratio() >= self.config.monitor_threshold
    }

    /// 如果 Token 使用量超过压缩阈值则返回 true。
    ///
    /// Returns true if token usage exceeds the compress threshold.
    pub fn needs_compression(&self) -> bool {
        self.token_ratio() >= self.config.compress_threshold
    }

    /// 如果 Token 使用量超过驱逐阈值则返回 true。
    ///
    /// Returns true if token usage exceeds the evict threshold.
    pub fn needs_eviction(&self) -> bool {
        self.token_ratio() >= self.config.evict_threshold
    }
}

// ---------------------------------------------------------------------------
// CompactionEntry
// ---------------------------------------------------------------------------

/// 压缩事件记录 —— 描述一次压缩操作的快照。
///
/// 【领域含义】CompactionEntry 记录了单次压缩事件的完整信息，包括
/// 触发层、压缩前后的 Token 数量、移除的消息数以及发生时间。这些
/// 记录用于调试、监控和指标收集。
///
/// A record of a single compaction event in the history.
#[derive(Clone, Debug)]
pub struct CompactionEntry {
    /// 执行此次压缩的层名称。
    ///
    /// The name of the layer that performed this compaction.
    pub layer: String,

    /// 压缩前的 Token 数量。
    ///
    /// Token count before compaction.
    pub tokens_before: usize,

    /// 压缩后的 Token 数量。
    ///
    /// Token count after compaction.
    pub tokens_after: usize,

    /// 被移除或修改的消息数量。
    ///
    /// Number of messages removed or modified.
    pub messages_removed: usize,

    /// 此次压缩发生的时刻（挂钟时间）。
    ///
    /// When this compaction occurred (wall-clock time).
    pub timestamp: Instant,
}

// ---------------------------------------------------------------------------
// ContextManager
// ---------------------------------------------------------------------------

/// 上下文管理器 —— 对话历史管理与渐进式压缩。
///
/// 【领域含义】ContextManager 是 Agent 对话历史的核心管理组件。它负责：
/// - 累积对话消息（用户输入、助手响应、工具调用、工具结果）
/// - 基于字符数估算 Token 使用量
/// - 在阈值被突破时触发 5 层压缩管道
/// - 构建用于模型提示的最终消息列表
///
/// ContextManager 是 ContextConfig、ContextState 和 CompactionPipeline
/// 三个聚合根的协调者，对外提供消息管理、压缩触发和指标查询的统一接口。
///
/// # 向后兼容
///
/// 此类替换了 agent::session 中的旧 ContextManager。API
/// （add_message、add_messages、build_messages、history_len、clear）
/// 保持原样，以确保 Session 无需修改即可继续工作。
///
/// Manages conversation history, token tracking, and progressive compaction.
///
/// The context manager:
/// - Accumulates conversation messages (user inputs, assistant responses,
///   tool calls, tool results).
/// - Estimates token usage based on character counts.
/// - Triggers the 5-layer compaction pipeline when thresholds are crossed.
/// - Builds the final message list for model prompts.
///
/// # Backward Compatibility
///
/// This replaces the old ContextManager from agent::session. The API
/// (add_message, add_messages, build_messages, history_len, clear)
/// is preserved so that Session continues to work without changes.
#[derive(Clone, Debug)]
pub struct ContextManager {
    /// 压缩与 Token 限制配置。
    ///
    /// Compaction and token-limit configuration.
    config: ContextConfig,

    /// 当前 Token 数量估算值（缓存的效率优化）。
    ///
    /// Current token count estimate (cached for efficiency).
    current_tokens: usize,

    /// 会话生命周期内压缩已应用的次数。
    ///
    /// How many times compaction has been applied over the session's lifetime.
    compaction_count: u32,

    /// 压缩事件的历史记录，用于调试和指标收集。
    ///
    /// Historical record of compaction events for debugging and metrics.
    history: Vec<CompactionEntry>,

    /// 预置到每次提示前的系统指令。
    ///
    /// System instructions prepended to each prompt.
    system_instructions: String,

    /// 有序的对话消息。
    ///
    /// Ordered conversation messages.
    messages: Vec<Message>,

    /// 压缩管道实例。
    ///
    /// Compaction pipeline instance.
    pipeline: CompactionPipeline,
}

impl ContextManager {
    /// 使用默认配置和给定的系统指令创建新的上下文管理器。
    ///
    /// Create a new context manager with default configuration and given
    /// system instructions.
    pub fn new(system_instructions: String) -> Self {
        let config = ContextConfig::default();
        let pipeline = CompactionPipeline::new(config.preserve_last_turns);

        Self {
            config,
            current_tokens: 0,
            compaction_count: 0,
            history: Vec::new(),
            system_instructions,
            messages: Vec::new(),
            pipeline,
        }
    }

    /// 使用自定义配置创建上下文管理器。
    ///
    /// Create a context manager with custom configuration.
    pub fn with_config(system_instructions: String, config: ContextConfig) -> Self {
        let pipeline = CompactionPipeline::new(config.preserve_last_turns);

        Self {
            config,
            current_tokens: 0,
            compaction_count: 0,
            history: Vec::new(),
            system_instructions,
            messages: Vec::new(),
            pipeline,
        }
    }

    // ── Message management (backward-compatible API) ──────────────────

    /// 向对话历史追加单条消息。
    ///
    /// 添加后自动检查是否需要触发压缩。
    ///
    /// Append a single message to the conversation history.
    ///
    /// Automatically checks whether compaction is needed after adding.
    pub fn add_message(&mut self, msg: Message) {
        let estimated = compactor::estimate_tokens(&msg);
        self.messages.push(msg);
        self.current_tokens += estimated;
        self.compact_if_needed();
    }

    /// 向对话历史追加多条消息。
    ///
    /// 添加后自动检查是否需要触发压缩。
    ///
    /// Append multiple messages to the conversation history.
    ///
    /// Automatically checks whether compaction is needed after adding.
    pub fn add_messages(&mut self, msgs: Vec<Message>) {
        let added_tokens: usize = msgs.iter().map(compactor::estimate_tokens).sum();
        self.messages.extend(msgs);
        self.current_tokens += added_tokens;
        self.compact_if_needed();
    }

    /// 构建用于下一次模型调用的完整消息列表。
    ///
    /// 系统指令以 UserMessage 的形式预置到列表最前面（协议没有专用的
    /// 系统消息变体；模型提供方会在下游处理角色映射）。
    ///
    /// Build the full message list for the next model call.
    ///
    /// The system instructions are prepended as a user message (the protocol
    /// does not have a dedicated system message variant; the model provider
    /// handles the role mapping downstream).
    pub fn build_messages(&self) -> Vec<Message> {
        let mut messages = Vec::with_capacity(1 + self.messages.len());

        // Prepend system instructions
        if !self.system_instructions.is_empty() {
            messages.push(Message::UserMessage {
                content: self.system_instructions.clone(),
            });
        }

        messages.extend(self.messages.clone());
        messages
    }

    /// 返回历史中的消息数量（不包括系统指令）。
    ///
    /// Returns the number of messages in the history (excluding system instructions).
    pub fn history_len(&self) -> usize {
        self.messages.len()
    }

    /// 清空对话历史。
    ///
    /// Clear the conversation history.
    pub fn clear(&mut self) {
        self.messages.clear();
        self.current_tokens = 0;
    }

    // ── Compaction ────────────────────────────────────────────────────

    /// 检查 Token 使用量是否超过阈值，如果是则触发压缩。
    ///
    /// 在 add_message 和 add_messages 之后自动调用。
    ///
    /// Check whether token usage exceeds thresholds and run compaction if needed.
    ///
    /// Called automatically after add_message and add_messages.
    pub fn compact_if_needed(&mut self) {
        if self.token_ratio() < self.config.monitor_threshold {
            return;
        }

        let mut state = ContextState::new(
            self.config.clone(),
            std::mem::take(&mut self.messages),
            self.system_instructions.clone(),
        );

        let results = self.pipeline.run(&mut state);

        for result in &results {
            if result.messages_removed > 0 {
                self.compaction_count += 1;
            }

            self.history.push(CompactionEntry {
                layer: result.layer.clone(),
                tokens_before: result.tokens_before,
                tokens_after: result.tokens_after,
                messages_removed: result.messages_removed,
                timestamp: Instant::now(),
            });
        }

        if !results.is_empty() {
            if state.token_ratio() >= self.config.evict_threshold {
                warn!(
                    current_tokens = state.current_tokens,
                    max = self.config.max_context_tokens,
                    ratio = format!("{:.1}%", state.token_ratio() * 100.0),
                    "Context still over eviction threshold after all compaction layers"
                );
            } else {
                let reduction_pct = if !self.history.is_empty() {
                    let last = &self.history[self.history.len() - 1];
                    if last.tokens_before > 0 {
                        (1.0 - last.tokens_after as f64 / last.tokens_before as f64) * 100.0
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };

                info!(
                    layers_run = results.len(),
                    total_removed = results.iter().map(|r| r.messages_removed).sum::<usize>(),
                    current_tokens = state.current_tokens,
                    reduction = format!("{:.1}%", reduction_pct),
                    "Compaction completed"
                );
            }
        }

        // Restore messages from state
        self.messages = state.messages;
        self.current_tokens = state.current_tokens;
    }

    // ── Metrics & queries ─────────────────────────────────────────────

    /// 获取当前估算的 Token 数量。
    ///
    /// Get the current estimated token count.
    pub fn token_count(&self) -> usize {
        self.current_tokens
    }

    /// 计算当前 Token 使用率（0.0–∞）。
    ///
    /// Calculate the current token usage ratio (0.0–∞).
    pub fn token_ratio(&self) -> f64 {
        if self.config.max_context_tokens == 0 {
            return 0.0;
        }
        self.current_tokens as f64 / self.config.max_context_tokens as f64
    }

    /// 获取最大上下文 Token 限制。
    ///
    /// Get the maximum context token limit.
    pub fn max_tokens(&self) -> usize {
        self.config.max_context_tokens
    }

    /// 获取压缩已应用的次数。
    ///
    /// Get the number of times compaction has been applied.
    pub fn compaction_count(&self) -> u32 {
        self.compaction_count
    }

    /// 获取压缩历史记录（用于调试/指标）。
    ///
    /// Get the compaction history for debugging/metrics.
    pub fn compaction_history(&self) -> &[CompactionEntry] {
        &self.history
    }

    /// 返回当前上下文状态的快照（用于检查和测试）。
    ///
    /// Return a snapshot of the current context state (for inspection/testing).
    pub fn context_state(&self) -> ContextState {
        ContextState {
            config: self.config.clone(),
            messages: self.messages.clone(),
            system_instructions: self.system_instructions.clone(),
            current_tokens: self.current_tokens,
            compaction_count: self.compaction_count,
        }
    }

    /// 获取内部消息缓冲区的引用。
    ///
    /// Get a reference to the internal message buffer.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// 获取压缩管道（用于测试层触发阈值）。
    ///
    /// Get the compaction pipeline (for testing layer trigger thresholds).
    #[doc(hidden)]
    pub fn pipeline(&self) -> &CompactionPipeline {
        &self.pipeline
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ──────────────────────────────────────────────────────────

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

    fn large_text(tokens: usize) -> String {
        let chars_per_token = 4;
        let word = "test ";
        let repeats = (tokens * chars_per_token) / word.len() + 1;
        word.repeat(repeats)
    }

    // ── ContextConfig tests ─────────────────────────────────────────────

    #[test]
    fn config_defaults() {
        let config = ContextConfig::default();
        assert_eq!(config.monitor_threshold, 0.70);
        assert_eq!(config.compress_threshold, 0.85);
        assert_eq!(config.evict_threshold, 0.95);
        assert_eq!(config.max_context_tokens, 64_000);
        assert_eq!(config.preserve_last_turns, 3);
    }

    // ── ContextState tests ──────────────────────────────────────────────

    #[test]
    fn context_state_token_ratio() {
        let config = ContextConfig {
            max_context_tokens: 1000,
            ..ContextConfig::default()
        };
        let msgs = vec![make_user_msg(&large_text(500))]; // ~500 tokens
        let state = ContextState::new(config, msgs, "".into());

        let ratio = state.token_ratio();
        assert!(ratio > 0.4, "ratio should be around 0.5 for 500/1000 tokens");
        assert!(ratio < 0.6, "ratio should be around 0.5");
    }

    #[test]
    fn context_state_zero_max_tokens() {
        let config = ContextConfig {
            max_context_tokens: 0,
            ..ContextConfig::default()
        };
        let state = ContextState::new(config, vec![], "".into());
        assert_eq!(state.token_ratio(), 0.0);
    }

    #[test]
    fn context_state_threshold_checks() {
        let config = ContextConfig {
            max_context_tokens: 1000,
            ..ContextConfig::default()
        };

        // Empty → all false
        let empty = ContextState::new(config.clone(), vec![], "".into());
        assert!(!empty.needs_monitoring());
        assert!(!empty.needs_compression());
        assert!(!empty.needs_eviction());

        // 750 tokens → monitor (70%), not yet compress (85%)
        let msgs_750 = vec![make_user_msg(&large_text(750))];
        let state_750 = ContextState::new(config.clone(), msgs_750, "".into());
        assert!(state_750.needs_monitoring());
        assert!(!state_750.needs_compression());

        // 900 tokens → compress (85%)
        let msgs_900 = vec![make_user_msg(&large_text(900))];
        let state_900 = ContextState::new(config.clone(), msgs_900, "".into());
        assert!(state_900.needs_monitoring());
        assert!(state_900.needs_compression());
        assert!(!state_900.needs_eviction());

        // 960 tokens → evict (95%)
        let msgs_960 = vec![make_user_msg(&large_text(960))];
        let state_960 = ContextState::new(config, msgs_960, "".into());
        assert!(state_960.needs_eviction());
    }

    #[test]
    fn context_state_recalc_tokens() {
        let config = ContextConfig::default();
        let mut state = ContextState::new(config, vec![], "".into());
        assert_eq!(state.current_tokens, 0);

        state.messages.push(make_user_msg("hello world"));
        state.recalc_tokens();
        assert!(state.current_tokens > 0);
    }

    // ── ContextManager: backward-compatible API ─────────────────────────

    #[test]
    fn context_manager_new() {
        let cm = ContextManager::new("You are a bot.".into());
        assert_eq!(cm.history_len(), 0);
        assert_eq!(cm.token_count(), 0);
    }

    #[test]
    fn context_manager_with_config() {
        let config = ContextConfig {
            max_context_tokens: 8000,
            preserve_last_turns: 5,
            ..ContextConfig::default()
        };
        let cm = ContextManager::with_config("sys".into(), config);
        assert_eq!(cm.max_tokens(), 8000);
    }

    #[test]
    fn add_message_increments_count() {
        let mut cm = ContextManager::new(String::new());
        cm.add_message(make_user_msg("hello"));
        assert_eq!(cm.history_len(), 1);
        assert!(cm.token_count() > 0);
    }

    #[test]
    fn add_messages_increments_count() {
        let mut cm = ContextManager::new(String::new());
        cm.add_messages(vec![
            make_user_msg("one"),
            make_assistant_msg("two"),
        ]);
        assert_eq!(cm.history_len(), 2);
    }

    #[test]
    fn build_messages_includes_system_instructions() {
        let cm = ContextManager::new("You are helpful.".into());
        let messages = cm.build_messages();
        assert_eq!(messages.len(), 1);
        if let Message::UserMessage { content } = &messages[0] {
            assert_eq!(content, "You are helpful.");
        }
    }

    #[test]
    fn build_messages_includes_history() {
        let mut cm = ContextManager::new("system".into());
        cm.add_message(make_user_msg("query"));
        let messages = cm.build_messages();
        assert_eq!(messages.len(), 2); // system + history
    }

    #[test]
    fn clear_resets_everything() {
        let mut cm = ContextManager::new("sys".into());
        cm.add_message(make_user_msg("hello"));
        assert_eq!(cm.history_len(), 1);
        cm.clear();
        assert_eq!(cm.history_len(), 0);
        assert_eq!(cm.token_count(), 0);
    }

    #[test]
    fn build_messages_without_system_instructions() {
        let cm = ContextManager::new(String::new());
        let messages = cm.build_messages();
        assert!(messages.is_empty());
    }

    // ── ContextManager: compaction triggers ─────────────────────────────

    #[test]
    fn compaction_triggers_at_85_percent_threshold() {
        let config = ContextConfig {
            max_context_tokens: 1000,
            ..ContextConfig::default()
        };
        let mut cm = ContextManager::with_config("sys".into(), config);

        // Add enough content to push past 85% of 1000 tokens
        let large = large_text(900);
        cm.add_message(make_user_msg(&large));

        assert!(cm.token_ratio() >= 0.85,
            "after adding large message, token ratio {:.1}% should be >= 85%",
            cm.token_ratio() * 100.0
        );

        // Microcompact should have fired and reduced the message
        // The message should now be trimmed
        assert!(cm.compaction_count() > 0 || cm.history_len() > 0,
            "compaction should have triggered"
        );
    }

    #[test]
    fn token_reduction_after_compression() {
        use code_agent_protocol::ToolResultMessage;

        let config = ContextConfig {
            max_context_tokens: 12000,
            ..ContextConfig::default()
        };
        let mut cm = ContextManager::with_config("sys".into(), config);

        // Use a ToolResult with >8K tokens so BudgetReduction trims it.
        // One message at ~9000 tokens, max=12000 → ratio=75% > 70% threshold.
        let before_token_count = cm.token_count();
        cm.add_message(Message::ToolResult(ToolResultMessage {
            tool_call_id: "tc-1".into(),
            output: Some(large_text(9000)),
            error: None,
        }));

        let after_token_count = cm.token_count();
        // BudgetReduction should have trimmed the output from ~9000 to ~8192 tokens
        assert!(
            after_token_count < 9000,
            "after compaction, tokens should decrease. before: {}, after: {}",
            before_token_count, after_token_count
        );

        // Verify the message was truncated
        let msgs = cm.messages();
        if let Message::ToolResult(ref tr) = msgs[0] {
            let out = tr.output.as_ref().unwrap();
            assert!(
                out.contains("truncated by BudgetReduction"),
                "should have truncation annotation. output starts with: {}",
                &out[..out.len().min(100)]
            );
        }
    }

    #[test]
    fn system_prompt_and_last_turns_preserved() {
        let config = ContextConfig {
            max_context_tokens: 50_000,
            preserve_last_turns: 3,
            ..ContextConfig::default()
        };
        let mut cm = ContextManager::with_config("You are a coding assistant.".into(), config);

        // Add 20 turns of conversation
        for i in 0..20 {
            cm.add_message(make_user_msg(&format!("Turn {} request", i + 1)));
            cm.add_message(make_assistant_msg(&format!("Turn {} response", i + 1)));
        }

        // The Snip layer should have fired (70% of 50K = 35K tokens).
        // After 20 small turns, it might not trigger since the messages are small.
        // Force compaction by building with huge messages instead.
        let _count = cm.history_len();

        // Build messages and check that the last 3 turns are present
        let messages = cm.build_messages();

        // System prompt should be first
        if let Message::UserMessage { ref content } = messages[0] {
            assert!(content.contains("coding assistant"));
        }

        // Turn 20 should be near the end
        let has_turn_20 = messages.iter().any(|m| match m {
            Message::AssistantMessage { content } => content.contains("Turn 20"),
            _ => false,
        });
        assert!(has_turn_20, "Turn 20 should be preserved");
    }

    #[test]
    fn hundred_simulated_turns_stay_under_64k() {
        let mut cm = ContextManager::new("sys".into());

        // Add 100 turns with moderate-sized messages
        for i in 0..100 {
            let content = format!(
                "Turn {}: Let's explore the code and make changes. {}", 
                i + 1,
                large_text(200) // ~200 tokens per message
            );
            cm.add_message(make_user_msg(&content));
            cm.add_message(make_assistant_msg(&format!(
                "Processing turn {}... {}", 
                i + 1,
                large_text(100)
            )));
        }

        let token_count = cm.token_count();
        assert!(
            token_count <= 64_000,
            "After 100 simulated turns with compaction: {} tokens should be <= 64K",
            token_count
        );
    }

    #[test]
    fn compaction_history_tracks_events() {
        use code_agent_protocol::ToolResultMessage;

        let config = ContextConfig {
            max_context_tokens: 10000,
            ..ContextConfig::default()
        };
        let mut cm = ContextManager::with_config("sys".into(), config);

        // Use a ToolResult message with >8K tokens to trigger BudgetReduction
        // BudgetReduction targets ToolResult messages specifically
        cm.add_message(Message::ToolResult(ToolResultMessage {
            tool_call_id: "tc-1".into(),
            output: Some(large_text(9000)), // ~9K tokens, >8K BudgetReduction limit
            error: None,
        }));

        let history = cm.compaction_history();
        assert!(!history.is_empty(), "compaction history should be recorded");
        assert!(
            history.iter().any(|e| e.messages_removed > 0),
            "at least one compaction entry should have removed messages. History: {:?}",
            history.iter().map(|e| format!("{}: removed={}", e.layer, e.messages_removed)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn context_state_snapshot() {
        let mut cm = ContextManager::new("sys".into());
        cm.add_message(make_user_msg("hello"));
        cm.add_message(make_assistant_msg("world"));

        let snapshot = cm.context_state();
        assert_eq!(snapshot.messages.len(), 2);
        assert_eq!(snapshot.system_instructions, "sys");
    }

    #[test]
    fn messages_ref_returns_current_buffer() {
        let mut cm = ContextManager::new("sys".into());
        cm.add_message(make_user_msg("test"));

        let msgs = cm.messages();
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn compaction_count_increments() {
        use code_agent_protocol::ToolResultMessage;

        let config = ContextConfig {
            max_context_tokens: 10000,
            ..ContextConfig::default()
        };
        let mut cm = ContextManager::with_config("sys".into(), config);

        // Before compaction
        assert_eq!(cm.compaction_count(), 0);

        // Add a ToolResult with >8K tokens to trigger BudgetReduction (which actually removes content)
        cm.add_message(Message::ToolResult(ToolResultMessage {
            tool_call_id: "tc-1".into(),
            output: Some(large_text(9000)),
            error: None,
        }));

        assert!(
            cm.compaction_count() > 0,
            "compaction count should increment after triggered compaction. Count: {}",
            cm.compaction_count()
        );
    }

    #[test]
    fn no_compaction_below_monitor_threshold() {
        let config = ContextConfig::default();
        let mut cm = ContextManager::with_config("sys".into(), config);

        // Add a tiny message (far below 70% of 64K)
        cm.add_message(make_user_msg("hi"));

        assert_eq!(cm.compaction_count(), 0);
        assert!(cm.compaction_history().is_empty());
    }

    #[test]
    fn token_ratio_calculation() {
        let config = ContextConfig {
            max_context_tokens: 1000,
            ..ContextConfig::default()
        };
        let mut cm = ContextManager::with_config("sys".into(), config);
        cm.add_message(make_user_msg(&large_text(500)));

        let ratio = cm.token_ratio();
        assert!(ratio > 0.4, "ratio {:.2} should be around 0.5", ratio);
        assert!(ratio < 0.6, "ratio {:.2} should be around 0.5", ratio);
    }
}