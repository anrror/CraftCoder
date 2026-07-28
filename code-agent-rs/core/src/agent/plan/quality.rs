//! 质量门禁系统 —— Spec 驱动的节点级质量检查。
//!
//! 【领域含义】Phase D 的核心：每个 Plan 节点执行完成后，在标记为 Completed
//! 之前先通过质量门禁验证。门禁检查结果决定节点的最终状态：
//! - `Pass`: 通过，正常标记为 Completed
//! - `Retry`: 未通过但可重试，触发重试逻辑
//! - `Fail`: 未通过且不可重试，标记为 Failed
//!
//! # 架构
//!
//! ```text
//! PlanNode (Running)
//!     │ wait_task() → AgentResult
//!     ▼
//! QualityGate::check(&AgentResult, &PlanNode)
//!     │
//!     ├─ Pass ──→ scheduler.mark_completed()
//!     ├─ Retry ─→ scheduler.mark_retry()
//!     └─ Fail ──→ node.status = Failed
//! ```
//!
//! # 扩展性
//!
//! `QualityGate` trait 允许不同的门禁实现：
//! - `SpecGate`: 基于 SpecConfig 的规则检查（默认）
//! - 未来可扩展：LLM 驱动的代码审查门禁、测试通过率门禁等

use serde::{Deserialize, Serialize};

use super::types::{PlanNode, PlanConfig};
use crate::agent::sub_agent::AgentResult;

// ---------------------------------------------------------------------------
// GateVerdict — 门禁裁决
// ---------------------------------------------------------------------------

/// 质量门禁对单个节点的裁决结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateVerdict {
    /// 通过质量检查，节点应标记为 Completed
    Pass,
    /// 未通过但允许重试，节点应重新调度
    Retry {
        /// 重试原因（用于日志和报告）
        reason: String,
    },
    /// 未通过且不可重试，节点应标记为 Failed
    Fail {
        /// 失败原因
        reason: String,
    },
}

impl GateVerdict {
    /// 是否通过检查。
    pub fn is_pass(&self) -> bool {
        matches!(self, GateVerdict::Pass)
    }

    /// 是否需要重试。
    pub fn is_retry(&self) -> bool {
        matches!(self, GateVerdict::Retry { .. })
    }

    /// 是否最终失败。
    pub fn is_fail(&self) -> bool {
        matches!(self, GateVerdict::Fail { .. })
    }

    /// 提取原因字符串（仅 Retry 或 Fail 时有效）。
    pub fn reason(&self) -> Option<&str> {
        match self {
            GateVerdict::Pass => None,
            GateVerdict::Retry { reason } => Some(reason),
            GateVerdict::Fail { reason } => Some(reason),
        }
    }
}

// ---------------------------------------------------------------------------
// QualityGate trait — 门禁接口
// ---------------------------------------------------------------------------

/// 质量门禁接口。
///
/// 实现该 trait 的类型可以对子 Agent 的执行结果进行质量检查，
/// 并给出通过/重试/失败的三态裁决。
#[async_trait::async_trait]
pub trait QualityGate: Send + Sync {
    /// 门禁名称（用于日志和报告）。
    fn name(&self) -> &str;

    /// 对单个节点的执行结果执行质量检查。
    ///
    /// # Arguments
    /// * `result` — 子 Agent 返回的执行结果
    /// * `node` — 正在检查的 Plan 节点
    /// * `config` — Plan 引擎配置
    async fn check(
        &self,
        result: &AgentResult,
        node: &PlanNode,
        config: &PlanConfig,
    ) -> GateVerdict;
}

// ---------------------------------------------------------------------------
// SpecConfig — 门禁规则配置
// ---------------------------------------------------------------------------

/// Spec 驱动的质量门禁规则配置。
///
/// 定义每个节点执行完成后需要满足的质量标准。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpecConfig {
    /// 是否启用门禁（默认 true）
    pub enabled: bool,

    /// 要求 AgentResult 中不包含 error（默认 true）
    pub require_no_error: bool,

    /// 要求 AgentResult 的 events 非空（默认 true）
    pub require_events: bool,

    /// 要求 final_message 存在（默认 false）
    pub require_final_message: bool,

    /// 自动重试失败节点（默认 true，受限于 default_max_retries）
    pub auto_retry: bool,

    /// 重试时是否使用不同的 Coder prompt（默认 false，仅重试相同任务）
    pub retry_with_variant: bool,

    /// 要求 PlanNode 的 spec 字段必须存在（默认 false）
    ///
    /// 启用后，没有 spec 描述的节点会直接 Fail（不自动重试，
    /// 因为 spec 缺失是规划阶段的问题，不是执行阶段的问题）。
    pub require_spec: bool,
}

impl Default for SpecConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            require_no_error: true,
            require_events: true,
            require_final_message: false,
            auto_retry: true,
            retry_with_variant: false,
            require_spec: false,
        }
    }
}

// ---------------------------------------------------------------------------
// SpecGate — 默认门禁实现
// ---------------------------------------------------------------------------

/// 基于 SpecConfig 规则的质量门禁实现。
///
/// 这是 Phase D 的默认门禁，检查子 Agent 返回的 `AgentResult`
/// 是否满足预定义的质量规则（无错误、有事件、有最终消息等）。
pub struct SpecGate {
    /// 门禁规则配置
    pub spec: SpecConfig,
}

impl SpecGate {
    /// 使用默认配置创建 SpecGate。
    pub fn new() -> Self {
        Self {
            spec: SpecConfig::default(),
        }
    }

    /// 使用自定义配置创建 SpecGate。
    pub fn with_config(spec: SpecConfig) -> Self {
        Self { spec }
    }

    /// 对结果执行规则检查，返回裁决结果。
    fn evaluate(&self, result: &AgentResult, node: &PlanNode) -> GateVerdict {
        // Rule 1: 无错误检查
        if self.spec.require_no_error && result.error.is_some() {
            let reason = format!(
                "Node '{}' has error: {}",
                node.label,
                result.error.as_deref().unwrap_or("unknown")
            );
            return if self.spec.auto_retry && node.retry_count < node.max_retries {
                GateVerdict::Retry { reason }
            } else {
                GateVerdict::Fail { reason }
            };
        }

        // Rule 2: 事件非空检查
        if self.spec.require_events && result.events.is_empty() {
            let reason = format!(
                "Node '{}' returned empty events",
                node.label
            );
            return if self.spec.auto_retry && node.retry_count < node.max_retries {
                GateVerdict::Retry { reason }
            } else {
                GateVerdict::Fail { reason }
            };
        }

        // Rule 3: final_message 存在性检查
        if self.spec.require_final_message && result.final_message.is_none() {
            let reason = format!(
                "Node '{}' has no final message",
                node.label
            );
            return if self.spec.auto_retry && node.retry_count < node.max_retries {
                GateVerdict::Retry { reason }
            } else {
                GateVerdict::Fail { reason }
            };
        }

        // Rule 4: spec 存在性检查
        if self.spec.require_spec && node.spec.is_none() {
            let reason = format!(
                "Node '{}' has no spec description (require_spec enabled)",
                node.label
            );
            // spec 缺失是规划阶段的问题，不自动重试
            return GateVerdict::Fail { reason };
        }

        // 全部通过
        GateVerdict::Pass
    }
}

impl Default for SpecGate {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl QualityGate for SpecGate {
    fn name(&self) -> &str {
        "SpecGate"
    }

    async fn check(
        &self,
        result: &AgentResult,
        node: &PlanNode,
        _config: &PlanConfig,
    ) -> GateVerdict {
        self.evaluate(result, node)
    }
}

// ---------------------------------------------------------------------------
// NoopGate — 空门禁（始终 Pass）
// ---------------------------------------------------------------------------

/// 空操作门禁 —— 始终返回 Pass，不执行任何检查。
///
/// 用于关闭门禁场景或测试环境。
pub struct NoopGate;

#[async_trait::async_trait]
impl QualityGate for NoopGate {
    fn name(&self) -> &str {
        "NoopGate"
    }

    async fn check(
        &self,
        _result: &AgentResult,
        _node: &PlanNode,
        _config: &PlanConfig,
    ) -> GateVerdict {
        GateVerdict::Pass
    }
}

// ---------------------------------------------------------------------------
// QualityGatePipeline — 门禁管道
// ---------------------------------------------------------------------------

/// 门禁管道 —— 依次执行多个 QualityGate，返回第一个非 Pass 的结果。
///
/// 如果所有门禁都返回 Pass，则最终结果为 Pass。
pub struct QualityGatePipeline {
    /// 按顺序执行的门禁列表
    gates: Vec<Box<dyn QualityGate>>,
}

impl QualityGatePipeline {
    /// 从门禁列表创建管道。
    pub fn new(gates: Vec<Box<dyn QualityGate>>) -> Self {
        Self { gates }
    }

    /// 创建一个只包含 SpecGate 的默认管道。
    pub fn default_with_spec(spec: SpecConfig) -> Self {
        Self {
            gates: vec![Box::new(SpecGate::with_config(spec))],
        }
    }

    /// 创建一个空管道（所有检查通过）。
    pub fn empty() -> Self {
        Self { gates: vec![] }
    }

    /// 门禁数量。
    pub fn len(&self) -> usize {
        self.gates.len()
    }

    /// 管道是否为空。
    pub fn is_empty(&self) -> bool {
        self.gates.is_empty()
    }
}

#[async_trait::async_trait]
impl QualityGate for QualityGatePipeline {
    fn name(&self) -> &str {
        "QualityGatePipeline"
    }

    async fn check(
        &self,
        result: &AgentResult,
        node: &PlanNode,
        config: &PlanConfig,
    ) -> GateVerdict {
        for gate in &self.gates {
            let verdict = gate.check(result, node, config).await;
            if !verdict.is_pass() {
                return verdict;
            }
        }
        GateVerdict::Pass
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sub_agent::AgentStatus;
    use code_agent_protocol::{Message, ResponseEvent, TurnId};

    fn make_agent_result(status: AgentStatus) -> AgentResult {
        AgentResult {
            agent_id: "test-agent".into(),
            status,
            events: vec![ResponseEvent::TurnComplete {
                turn_id: TurnId("t1".into()),
                final_message: Message::UserMessage {
                    content: "done".into(),
                },
            }],
            final_message: Some("done".into()),
            error: None,
        }
    }

    fn make_plan_node() -> PlanNode {
        PlanNode {
            id: "test-node".into(),
            label: "Test Node".into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_spec_gate_passes_clean_result() {
        let gate = SpecGate::new();
        let result = make_agent_result(AgentStatus::Completed);
        let node = make_plan_node();
        let config = PlanConfig::default();

        let verdict = gate.check(&result, &node, &config).await;
        assert_eq!(verdict, GateVerdict::Pass);
    }

    #[tokio::test]
    async fn test_spec_gate_rejects_error() {
        let gate = SpecGate::new();
        let mut result = make_agent_result(AgentStatus::Failed);
        result.error = Some("task failed".into());
        let node = make_plan_node();
        let config = PlanConfig::default();

        let verdict = gate.check(&result, &node, &config).await;
        assert!(verdict.is_retry() || verdict.is_fail());
    }

    #[tokio::test]
    async fn test_spec_gate_rejects_empty_events() {
        let gate = SpecGate::new();
        let result = AgentResult {
            agent_id: "test-agent".into(),
            status: AgentStatus::Completed,
            events: vec![],
            final_message: Some("done".into()),
            error: None,
        };
        let node = make_plan_node();
        let config = PlanConfig::default();

        let verdict = gate.check(&result, &node, &config).await;
        assert!(!verdict.is_pass(), "empty events should not pass");
    }

    #[test]
    fn test_gate_verdict_helpers() {
        assert!(GateVerdict::Pass.is_pass());
        assert!(!GateVerdict::Pass.is_retry());
        assert!(!GateVerdict::Pass.is_fail());
        assert_eq!(GateVerdict::Pass.reason(), None);

        let r = GateVerdict::Retry { reason: "timeout".into() };
        assert!(r.is_retry());
        assert_eq!(r.reason(), Some("timeout"));

        let f = GateVerdict::Fail { reason: "hard error".into() };
        assert!(f.is_fail());
        assert_eq!(f.reason(), Some("hard error"));
    }

    #[tokio::test]
    async fn test_noop_gate_always_passes() {
        let gate = NoopGate;
        let result = make_agent_result(AgentStatus::Failed);
        let node = make_plan_node();
        let config = PlanConfig::default();

        assert_eq!(gate.check(&result, &node, &config).await, GateVerdict::Pass);
    }

    #[tokio::test]
    async fn test_pipeline_passes_when_all_pass() {
        let pipeline = QualityGatePipeline::new(vec![
            Box::new(NoopGate),
            Box::new(NoopGate),
        ]);
        let result = make_agent_result(AgentStatus::Completed);
        let node = make_plan_node();
        let config = PlanConfig::default();

        assert_eq!(pipeline.check(&result, &node, &config).await, GateVerdict::Pass);
    }

    #[tokio::test]
    async fn test_pipeline_stops_at_first_fail() {
        let gate = SpecGate::new();
        let pipeline = QualityGatePipeline::new(vec![
            Box::new(gate),
            Box::new(NoopGate),
        ]);
        let mut result = make_agent_result(AgentStatus::Failed);
        result.error = Some("broken".into());
        let node = make_plan_node();
        let config = PlanConfig::default();

        let verdict = pipeline.check(&result, &node, &config).await;
        assert!(!verdict.is_pass(), "should fail at first gate");
        assert_eq!(
            verdict.reason(),
            Some("Node 'Test Node' has error: broken")
        );
    }

    #[test]
    fn test_spec_config_default_enabled() {
        let spec = SpecConfig::default();
        assert!(spec.enabled);
        assert!(spec.require_no_error);
        assert!(spec.require_events);
    }

    #[test]
    fn test_pipeline_len_and_empty() {
        let empty = QualityGatePipeline::empty();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);

        let full = QualityGatePipeline::default_with_spec(SpecConfig::default());
        assert!(!full.is_empty());
        assert_eq!(full.len(), 1);
    }

    #[tokio::test]
    async fn test_spec_gate_with_disabled_rules() {
        let config = SpecConfig {
            require_no_error: false,
            require_events: false,
            ..Default::default()
        };
        let gate = SpecGate::with_config(config);
        let result = make_agent_result(AgentStatus::Completed);
        let node = make_plan_node();
        let plan_config = PlanConfig::default();

        let verdict = gate.check(&result, &node, &plan_config).await;
        assert_eq!(verdict, GateVerdict::Pass);
    }
}
