//! Plan 引擎数据模型 —— 任务分解图、节点、状态与报告
//!
//! 【领域含义】定义结构化任务分解（三级分解）的完整数据模型：
//! - `TaskPlan` — 完整的 DAG 执行计划，包含所有任务节点和阶段
//! - `PlanNode` — 单个任务节点，含依赖关系和执行状态
//! - `NodeStatus` — 节点的生命周期状态（Pending→Ready→Running→Completed/Failed/Cancelled）
//! - `PlanConfig` — Plan 引擎的配置参数
//! - `ExecutionReport` — 完整计划执行后的结果报告

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::agent::sub_agent::AgentResult;

use super::quality::GateVerdict;

// ---------------------------------------------------------------------------
// NodeStatus
// ---------------------------------------------------------------------------

/// 计划节点的生命周期状态。
///
/// 【状态转换】
/// - `Pending`     → `Ready`（所有上游依赖完成）
/// - `Ready`       → `Running`（被调度执行）
/// - `Running`     → `Completed` / `Failed` / `Cancelled`（执行结果）
/// - `Failed`      → `Ready`（重试时重新变为就绪）
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    /// 尚未就绪（存在未满足的依赖）
    #[default]
    Pending,
    /// 所有依赖已满足，等待调度
    Ready,
    /// 正在执行
    Running,
    /// 执行成功
    Completed,
    /// 执行失败
    Failed,
    /// 被取消（级联取消）
    Cancelled,
}

impl NodeStatus {
    /// 是否为终态（不会再变化）
    pub fn is_terminal(&self) -> bool {
        matches!(self, NodeStatus::Completed | NodeStatus::Failed | NodeStatus::Cancelled)
    }

    /// 是否为成功终态
    pub fn is_success(&self) -> bool {
        matches!(self, NodeStatus::Completed)
    }

    /// 是否为失败终态（可重试）
    pub fn is_failure(&self) -> bool {
        matches!(self, NodeStatus::Failed)
    }
}

// ---------------------------------------------------------------------------
// PlanNode
// ---------------------------------------------------------------------------

/// DAG 中的一个任务节点。
///
/// 【领域含义】代表三级分解中的一个原子任务。包含该任务的唯一标识、
/// 可读名称、所属阶段、上游依赖列表、可选的 Spec 描述、执行状态和结果。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PlanNode {
    /// 唯一标识符（如 "auth-register-001"）
    pub id: String,
    /// 可读的任务名称（如 "实现用户注册接口"）
    pub label: String,
    /// 所属阶段名称（如 "Phase 1: 认证层"、"Phase 0: 数据层"）
    pub phase: String,
    /// 上游依赖的节点 ID 列表
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// 可选的任务规格说明（Spec），指导 Coder 执行的详细描述
    #[serde(default)]
    pub spec: Option<String>,
    /// 当前执行状态
    #[serde(default)]
    pub status: NodeStatus,
    /// 执行结果（完成后才有值）
    #[serde(default)]
    pub result: Option<AgentResult>,
    /// 重试次数
    #[serde(default)]
    pub retry_count: u32,
    /// 最大重试次数
    #[serde(default)]
    pub max_retries: u32,
    /// 质量门禁裁决结果（执行后填充）
    #[serde(default)]
    pub gate_verdict: Option<GateVerdict>,
}

impl PlanNode {
    /// 创建新的计划节点。
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        phase: impl Into<String>,
        depends_on: Vec<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            phase: phase.into(),
            depends_on,
            spec: None,
            status: NodeStatus::Pending,
            result: None,
            retry_count: 0,
            max_retries: 2,
            gate_verdict: None,
        }
    }

    /// 设置任务规格说明。
    pub fn with_spec(mut self, spec: impl Into<String>) -> Self {
        self.spec = Some(spec.into());
        self
    }

    /// 设置最大重试次数。
    pub fn with_max_retries(mut self, max: u32) -> Self {
        self.max_retries = max;
        self
    }

    /// 检查此节点是否可以执行（所有依赖已完成）。
    pub fn is_ready(&self, completed_ids: &[String]) -> bool {
        if self.status != NodeStatus::Pending && self.status != NodeStatus::Failed {
            return false;
        }
        self.depends_on
            .iter()
            .all(|dep| completed_ids.contains(dep))
    }

    /// 构建发送给 Delegator 的消息内容。
    pub fn build_message(&self) -> String {
        match &self.spec {
            Some(spec) => format!("{} - Spec:\n{}", self.label, spec),
            None => self.label.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// TaskPlan
// ---------------------------------------------------------------------------

/// 完整的三级任务分解计划（DAG）。
///
/// 【领域含义】包含所有计划节点和阶段顺序信息，
/// 是 Plan 引擎的核心数据结构。支持序列化为 JSON/YAML，
/// 可保存、传输和恢复。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskPlan {
    /// 计划名称（如"用户认证模块开发"）
    pub name: String,
    /// 所有任务节点
    pub nodes: Vec<PlanNode>,
    /// 按执行顺序排列的阶段名称列表
    pub phases: Vec<String>,
    /// 创建时间戳（Unix millis）
    pub created_at: u64,
}

impl TaskPlan {
    /// 创建新的空计划。
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            nodes: Vec::new(),
            phases: Vec::new(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }
    }

    /// 添加一个节点。
    pub fn add_node(&mut self, node: PlanNode) {
        self.nodes.push(node);
    }

    /// 添加一个阶段。
    pub fn add_phase(&mut self, phase: impl Into<String>) {
        let phase = phase.into();
        if !self.phases.contains(&phase) {
            self.phases.push(phase);
        }
    }

    /// 按 ID 查找节点。
    pub fn get_node(&self, id: &str) -> Option<&PlanNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// 按 ID 获取可变引用。
    pub fn get_node_mut(&mut self, id: &str) -> Option<&mut PlanNode> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// 获取指定状态的所有节点。
    pub fn nodes_by_status(&self, status: NodeStatus) -> Vec<&PlanNode> {
        self.nodes.iter().filter(|n| n.status == status).collect()
    }

    /// 获取指定阶段的所有节点。
    pub fn nodes_by_phase(&self, phase: &str) -> Vec<&PlanNode> {
        self.nodes.iter().filter(|n| n.phase == phase).collect()
    }

    /// 获取所有已完成节点的 ID 列表。
    pub fn completed_ids(&self) -> Vec<String> {
        self.nodes
            .iter()
            .filter(|n| n.status == NodeStatus::Completed)
            .map(|n| n.id.clone())
            .collect()
    }

    /// 获取当前就绪（所有依赖已满足）的节点。
    pub fn ready_nodes(&self) -> Vec<&PlanNode> {
        let completed = self.completed_ids();
        self.nodes
            .iter()
            .filter(|n| n.is_ready(&completed))
            .collect()
    }

    /// 获取节点数量。
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// 获取已完成节点数量。
    pub fn completed_count(&self) -> usize {
        self.nodes_by_status(NodeStatus::Completed).len()
    }

    /// 获取失败节点数量。
    pub fn failed_count(&self) -> usize {
        self.nodes_by_status(NodeStatus::Failed).len()
    }

    /// 验证计划是否有循环依赖。
    ///
    /// 使用 DFS 检测环。返回第一个检测到的环中的节点 ID 列表。
    pub fn detect_cycle(&self) -> Option<Vec<String>> {
        // 0 = unvisited, 1 = visiting, 2 = visited
        let state: HashMap<&str, u8> = self.nodes.iter().map(|n| (n.id.as_str(), 0u8)).collect();
        let mut state = state;

        fn dfs<'a>(
            node_id: &'a str,
            nodes: &'a [PlanNode],
            state: &mut HashMap<&'a str, u8>,
            path: &mut Vec<String>,
        ) -> Option<Vec<String>> {
            match state.get(node_id)? {
                1 => {
                    // Found cycle — extract the cycle from path
                    let pos = path.iter().position(|id| id == node_id)?;
                    return Some(path[pos..].to_vec());
                }
                2 => return None, // Already fully explored
                _ => {}
            }

            state.insert(node_id, 1);
            path.push(node_id.to_string());

            let node = nodes.iter().find(|n| n.id == node_id)?;
            for dep in &node.depends_on {
                if let Some(cycle) = dfs(dep, nodes, state, path) {
                    return Some(cycle);
                }
            }

            path.pop();
            state.insert(node_id, 2);
            None
        }

        let mut path = Vec::new();
        for node in &self.nodes {
            if state.get(node.id.as_str()) == Some(&0) {
                if let Some(cycle) = dfs(&node.id, &self.nodes, &mut state, &mut path) {
                    return Some(cycle);
                }
            }
        }
        None
    }

    /// 验证计划是否包含孤立节点（无入度也无出度的非独立节点）。
    pub fn validate(&self) -> Result<(), String> {
        // 检查循环依赖
        if let Some(cycle) = self.detect_cycle() {
            return Err(format!("Circular dependency detected: {}", cycle.join(" → ")));
        }

        // 检查 depends_on 引用的节点是否存在
        let ids: Vec<&str> = self.nodes.iter().map(|n| n.id.as_str()).collect();
        for node in &self.nodes {
            for dep in &node.depends_on {
                if !ids.contains(&dep.as_str()) {
                    return Err(format!(
                        "Node '{}' depends on '{}' which does not exist",
                        node.id, dep
                    ));
                }
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PlanConfig
// ---------------------------------------------------------------------------

/// Plan 引擎配置参数。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanConfig {
    /// 最大并行 Coder 数（默认 6）
    pub max_parallel: usize,
    /// Coder 最大迭代次数（默认 15）
    pub coder_max_iterations: usize,
    /// 默认重试次数（默认 2）
    pub default_max_retries: u32,
    /// 是否启用动态重规划（运行时分支）
    pub enable_replan: bool,
    /// 是否输出阶段摘要
    pub verbose_phases: bool,
    /// 是否启用质量门禁（Phase D，默认 true）
    pub gate_enabled: bool,
    /// Spec 门禁的详细规则配置（None = 使用 SpecConfig::default()）
    #[serde(default)]
    pub spec_config: Option<super::quality::SpecConfig>,
}

impl Default for PlanConfig {
    fn default() -> Self {
        Self {
            max_parallel: 6,
            coder_max_iterations: 15,
            default_max_retries: 2,
            enable_replan: true,
            verbose_phases: false,
            gate_enabled: true,
            spec_config: None,
        }
    }
}

// ---------------------------------------------------------------------------
// ExecutionReport
// ---------------------------------------------------------------------------

/// 计划执行后的完整报告。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionReport {
    /// 计划名称
    pub plan_name: String,
    /// 总节点数
    pub total_nodes: usize,
    /// 成功节点数
    pub completed_nodes: usize,
    /// 失败节点数
    pub failed_nodes: usize,
    /// 取消节点数
    pub cancelled_nodes: usize,
    /// 总执行时间（毫秒）
    pub duration_ms: u128,
    /// 每个节点的执行状态摘要
    pub node_summaries: Vec<NodeSummary>,
    /// 总体是否成功
    pub success: bool,
}

/// 单个节点的执行摘要。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeSummary {
    /// 节点 ID
    pub id: String,
    /// 节点标签
    pub label: String,
    /// 所属阶段
    pub phase: String,
    /// 最终状态
    pub status: NodeStatus,
    /// 错误消息（如果有）
    pub error: Option<String>,
    /// 最终消息摘要
    pub final_message_summary: Option<String>,
    /// 重试次数
    pub retry_count: u32,
    /// 质量门禁裁决结果
    pub gate_verdict: Option<String>,
}

// ---------------------------------------------------------------------------
// PlanProgressEvent
// ---------------------------------------------------------------------------

/// 计划执行的进度事件 —— 用于将执行状态实时推送给外层调用者（如 SessionRunner）。
///
/// 【领域含义】PlanExecutor 在执行过程中将关键节点事件通过 mpsc channel 发送出去，
/// 外层组件（SessionRunner / UI / IDE）可以订阅这些事件并实时更新进度。
///
/// # 事件序列示例
///
/// ```text
/// PhaseChanged { phase: "Phase 0: 数据层" }
///   → NodeStarted { node: "task-1" }
///   → NodeCompleted { node: "task-1" }
///   → PhaseChanged { phase: "Phase 1: 业务层" }
///   → NodeStarted { node: "task-2" }
///   → NodeCompleted { node: "task-2" }
///   → PlanCompleted { success: true }
/// ```
#[derive(Clone, Debug)]
pub enum PlanProgressEvent {
    /// 阶段切换（首次进入某阶段时触发）
    PhaseChanged {
        /// 新阶段名称
        phase: String,
        /// 当前已完成的节点数
        completed: usize,
        /// 总节点数
        total: usize,
    },
    /// 节点开始执行
    NodeStarted {
        node_id: String,
        label: String,
        phase: String,
    },
    /// 节点执行成功
    NodeCompleted {
        node_id: String,
        label: String,
        phase: String,
    },
    /// 节点执行失败（重试耗尽后）
    NodeFailed {
        node_id: String,
        label: String,
        phase: String,
        error: String,
    },
    /// 节点被取消（上游失败导致级联取消，或外部取消信号）
    NodeCancelled {
        node_id: String,
        label: String,
        phase: String,
    },
    /// 计划执行完成
    PlanCompleted {
        success: bool,
        completed: usize,
        failed: usize,
        cancelled: usize,
        total: usize,
    },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_status_cycle() {
        assert!(!NodeStatus::Pending.is_terminal());
        assert!(!NodeStatus::Ready.is_terminal());
        assert!(!NodeStatus::Running.is_terminal());
        assert!(NodeStatus::Completed.is_terminal());
        assert!(NodeStatus::Failed.is_terminal());
        assert!(NodeStatus::Cancelled.is_terminal());

        assert!(NodeStatus::Completed.is_success());
        assert!(!NodeStatus::Failed.is_success());
    }

    #[test]
    fn test_plan_node_ready_check() {
        let node = PlanNode::new("task-1", "Task 1", "Phase 0", vec!["dep-a".into(), "dep-b".into()]);
        // Not ready when deps not completed
        assert!(!node.is_ready(&[]));
        assert!(!node.is_ready(&["dep-a".into()]));
        // Ready when all deps completed
        assert!(node.is_ready(&["dep-a".into(), "dep-b".into()]));
    }

    #[test]
    fn test_plan_node_ready_check_no_deps() {
        let node = PlanNode::new("task-1", "Task 1", "Phase 0", vec![]);
        // No deps → always ready
        assert!(node.is_ready(&[]));
    }

    #[test]
    fn test_plan_node_ready_does_not_reset_completed() {
        let mut node = PlanNode::new("task-1", "Task 1", "Phase 0", vec![]);
        node.status = NodeStatus::Completed;
        // Already completed nodes are not "ready" (no double execution)
        assert!(!node.is_ready(&[]));
    }

    #[test]
    fn test_task_plan_add_and_query() {
        let mut plan = TaskPlan::new("test-plan");
        plan.add_phase("Phase 0");
        plan.add_phase("Phase 1");

        let node1 = PlanNode::new("task-1", "First", "Phase 0", vec![]);
        let node2 = PlanNode::new("task-2", "Second", "Phase 1", vec!["task-1".into()]);
        plan.add_node(node1);
        plan.add_node(node2);

        assert_eq!(plan.node_count(), 2);
        assert!(plan.get_node("task-1").is_some());
        assert!(plan.get_node("nonexistent").is_none());
    }

    #[test]
    fn test_task_plan_ready_nodes() {
        let mut plan = TaskPlan::new("test");
        plan.add_node(PlanNode::new("a", "A", "P0", vec![]));
        plan.add_node(PlanNode::new("b", "B", "P0", vec!["a".into()]));
        plan.add_node(PlanNode::new("c", "C", "P0", vec!["a".into()]));

        // Initially only 'a' is ready
        let ready = plan.ready_nodes();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "a");

        // Mark 'a' as completed
        plan.get_node_mut("a").unwrap().status = NodeStatus::Completed;

        // Now 'b' and 'c' should be ready
        let ready = plan.ready_nodes();
        assert_eq!(ready.len(), 2);
        let ready_ids: Vec<&str> = ready.iter().map(|n| n.id.as_str()).collect();
        assert!(ready_ids.contains(&"b"));
        assert!(ready_ids.contains(&"c"));
    }

    #[test]
    fn test_cycle_detection_no_cycle() {
        let mut plan = TaskPlan::new("test");
        plan.add_node(PlanNode::new("a", "A", "P0", vec![]));
        plan.add_node(PlanNode::new("b", "B", "P0", vec!["a".into()]));
        plan.add_node(PlanNode::new("c", "C", "P0", vec!["b".into()]));

        assert!(plan.detect_cycle().is_none());
        assert!(plan.validate().is_ok());
    }

    #[test]
    fn test_cycle_detection_with_cycle() {
        let mut plan = TaskPlan::new("test");
        plan.add_node(PlanNode::new("a", "A", "P0", vec!["c".into()]));
        plan.add_node(PlanNode::new("b", "B", "P0", vec!["a".into()]));
        plan.add_node(PlanNode::new("c", "C", "P0", vec!["b".into()]));

        assert!(plan.detect_cycle().is_some());
        assert!(plan.validate().is_err());
    }

    #[test]
    fn test_validate_missing_dependency() {
        let mut plan = TaskPlan::new("test");
        plan.add_node(PlanNode::new("a", "A", "P0", vec!["nonexistent".into()]));
        assert!(plan.validate().is_err());
    }

    #[test]
    fn test_node_build_message_with_and_without_spec() {
        let node_no_spec = PlanNode::new("t1", "Do something", "P0", vec![]);
        assert_eq!(node_no_spec.build_message(), "Do something");

        let node_with_spec = PlanNode::new("t2", "Do something", "P0", vec![])
            .with_spec("Create a POST /api/register endpoint");
        assert!(node_with_spec.build_message().contains("Create a POST"));
    }

    #[test]
    fn test_plan_phase_deduplication() {
        let mut plan = TaskPlan::new("test");
        plan.add_phase("Phase 0");
        plan.add_phase("Phase 0"); // duplicate
        assert_eq!(plan.phases.len(), 1);
    }

    #[test]
    fn test_node_status_is_failure() {
        assert!(NodeStatus::Failed.is_failure());
        assert!(!NodeStatus::Completed.is_failure());
        assert!(!NodeStatus::Pending.is_failure());
    }
}
