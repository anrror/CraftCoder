//! Kahn 拓扑排序 —— 将 TaskPlan DAG 转换为可并行执行的阶段批次
//!
//! 【领域含义】Kahn 算法是 Plan 引擎的核心调度组件。它将包含依赖关系的
//! DAG 任务图转换为"并列批次"（parallel batches），每个批次内的任务
//! 没有相互依赖关系，可以安全地并行执行。
//!
//! # 算法流程
//!
//! ```text
//! 输入: TaskPlan (nodes[] + edges[])
//! 1. 计算每个节点的入度（未满足的依赖数）
//! 2. 入度为 0 的节点 → 当前批次
//! 3. 执行当前批次（并行 Dispatch）
//! 4. 完成时减少下游节点入度
//! 5. 重复 2-4 直到所有节点完成
//! ```
//!
//! # 使用示例
//!
//! ```rust,ignore
//! let plan = build_my_plan();
//! let batches = kahn::parallel_batches(&plan);
//! for (i, batch) in batches.iter().enumerate() {
//!     println!("Batch {}: {} tasks", i, batch.len());
//!     for node in batch {
//!         println!("  - {} ({})", node.label, node.id);
//!     }
//! }
//! ```

use std::collections::{HashMap, VecDeque};

use crate::agent::plan::types::{PlanNode, TaskPlan};

// ---------------------------------------------------------------------------
// InDegreeTracker — 运行时入度追踪
// ---------------------------------------------------------------------------

/// 运行时入度追踪器 —— 在执行过程中动态更新节点入度。
///
/// 【领域含义】当某个节点完成执行时，其下游节点的入度会减少。
/// 入度变为 0 的节点变为"就绪"状态，可以被调度执行。
pub struct InDegreeTracker {
    /// 节点 ID → 当前入度（未满足的依赖数）
    in_degrees: HashMap<String, usize>,
}

impl InDegreeTracker {
    /// 从任务计划构建入度追踪器。
    pub fn from_plan(plan: &TaskPlan) -> Self {
        let mut in_degrees = HashMap::new();
        for node in &plan.nodes {
            in_degrees.insert(node.id.clone(), node.depends_on.len());
        }
        Self { in_degrees }
    }

    /// 获取指定节点的当前入度。
    pub fn get(&self, node_id: &str) -> usize {
        self.in_degrees.get(node_id).copied().unwrap_or(0)
    }

    /// 标记一个节点为已完成，减少其所有下游节点的入度。
    /// 返回本次调用中变为就绪（入度降为 0）的节点 ID 列表。
    pub fn mark_completed(&mut self, node_id: &str, plan: &TaskPlan) -> Vec<String> {
        let mut newly_ready = Vec::new();

        for node in &plan.nodes {
            if node.depends_on.contains(&node_id.to_string()) {
                if let Some(degree) = self.in_degrees.get_mut(&node.id) {
                    *degree = degree.saturating_sub(1);
                    if *degree == 0 {
                        newly_ready.push(node.id.clone());
                    }
                }
            }
        }

        newly_ready
    }

    /// 获取当前入度为 0 的节点。
    pub fn zero_degree_nodes(&self) -> Vec<String> {
        self.in_degrees
            .iter()
            .filter(|(_, &degree)| degree == 0)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// 是否所有节点都已变为就绪或完成（无正入度节点）。
    pub fn all_zero(&self) -> bool {
        self.in_degrees.values().all(|&d| d == 0)
    }

    /// 获取尚未就绪的节点数量。
    pub fn remaining_count(&self) -> usize {
        self.in_degrees.values().filter(|&&d| d > 0).count()
    }
}

// ---------------------------------------------------------------------------
// parallel_batches — 静态批次划分
// ---------------------------------------------------------------------------

/// 将任务计划划分为可并行执行的阶段批次。
///
/// 【领域含义】执行 Kahn 拓扑排序，返回按执行顺序排列的批次。
/// 每个批次内的节点无相互依赖关系，可以安全地并行执行。
///
/// # 返回值
///
/// `Vec<Vec<&PlanNode>>` — 按顺序排列的批次。
/// 每个批次是一个可并行执行的节点列表。
///
/// # 示例
///
/// ```text
/// 依赖: A → B, A → C, B → D, C → D
/// 结果: [[A], [B, C], [D]]
/// ```
pub fn parallel_batches(plan: &TaskPlan) -> Vec<Vec<&PlanNode>> {
    let mut in_degrees: HashMap<&str, usize> = HashMap::new();
    for node in &plan.nodes {
        in_degrees.entry(&node.id).or_insert(0);
        for dep in &node.depends_on {
            *in_degrees.entry(dep.as_str()).or_insert(0) += 0;
        }
    }

    // Build in-degree map: for each node, count unfulfilled dependencies
    for node in &plan.nodes {
        let entry = in_degrees.entry(node.id.as_str()).or_insert(0);
        *entry = node.depends_on.len();
    }

    // Collect initial zero-degree nodes
    let mut queue: VecDeque<&str> = in_degrees
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&id, _)| id)
        .collect();

    let mut batches: Vec<Vec<&PlanNode>> = Vec::new();
    let mut visited: HashMap<&str, usize> = HashMap::new(); // node_id → batch_index

    // Build a map for fast node lookup
    let node_map: HashMap<&str, &PlanNode> =
        plan.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    let _current_batch: Vec<&PlanNode> = Vec::new();

    // Track batch for each queued node
    let mut batch_for: HashMap<&str, usize> = HashMap::new();
    for &id in &queue {
        batch_for.insert(id, 0usize);
    }

    while let Some(node_id) = queue.pop_front() {
        let batch_idx = batch_for.remove(node_id).unwrap_or(0);

        // Add to current batch
        if let Some(node) = node_map.get(node_id) {
            // Ensure current_batch corresponds to this batch_idx
            while batches.len() <= batch_idx {
                batches.push(Vec::new());
            }
            batches[batch_idx].push(node);
            visited.insert(node_id, batch_idx);
        }

        // Decrease in-degree for dependents
        for node in &plan.nodes {
            if node.depends_on.iter().any(|d| d == node_id) {
                if let Some(degree) = in_degrees.get_mut(node.id.as_str()) {
                    *degree = degree.saturating_sub(1);
                    if *degree == 0 {
                        queue.push_back(&node.id);
                        batch_for.insert(&node.id, batch_idx + 1);
                    }
                }
            }
        }
    }

    batches
}

// ---------------------------------------------------------------------------
// Dynamic scheduling (for PlanExecutor)
// ---------------------------------------------------------------------------

/// 动态调度器 —— 支持 PlanExecutor 逐步派发。
///
/// 与静态 `parallel_batches` 不同，动态调度器允许：
/// 1. 按完成情况逐步派发新就绪的节点
/// 2. 处理重试（失败节点重新变为就绪）
/// 3. 处理取消（跳过被取消节点的下游）
pub struct DynamicScheduler {
    /// 节点 ID → 当前入度
    in_degrees: Vec<(String, usize)>,
    /// 已完成节点列表
    completed: Vec<String>,
    /// 已取消节点列表（取消的节点不触发下游）
    cancelled: Vec<String>,
}

impl DynamicScheduler {
    /// 从任务计划创建动态调度器。
    pub fn new(plan: &TaskPlan) -> Self {
        let in_degrees: Vec<(String, usize)> = plan
            .nodes
            .iter()
            .map(|n| (n.id.clone(), n.depends_on.len()))
            .collect();

        Self {
            in_degrees,
            completed: Vec::new(),
            cancelled: Vec::new(),
        }
    }

    /// 获取当前就绪（入度为 0 且未完成）的节点。
    pub fn ready_nodes<'a>(&self, plan: &'a TaskPlan) -> Vec<&'a PlanNode> {
        let completed_set: Vec<&str> = self.completed.iter().map(|s| s.as_str()).collect();
        let cancelled_set: Vec<&str> = self.cancelled.iter().map(|s| s.as_str()).collect();

        plan.nodes
            .iter()
            .filter(|n| {
                let is_done = completed_set.contains(&n.id.as_str())
                    || cancelled_set.contains(&n.id.as_str());
                if is_done {
                    return false;
                }

                let degree = self
                    .in_degrees
                    .iter()
                    .find(|(id, _)| id == &n.id)
                    .map(|(_, d)| *d)
                    .unwrap_or(usize::MAX);

                degree == 0
            })
            .collect()
    }

    /// 标记一个节点为已完成，更新下游入度。
    /// 返回新变为就绪的节点 ID 列表。
    pub fn mark_completed(&mut self, node_id: &str, plan: &TaskPlan) -> Vec<String> {
        self.completed.push(node_id.to_string());

        let mut newly_ready = Vec::new();
        for node in &plan.nodes {
            if node.depends_on.contains(&node_id.to_string()) {
                if let Some((_, degree)) =
                    self.in_degrees.iter_mut().find(|(id, _)| id == &node.id)
                {
                    *degree = degree.saturating_sub(1);
                    if *degree == 0 {
                        newly_ready.push(node.id.clone());
                    }
                }
            }
        }
        newly_ready
    }

    /// 标记一个节点为已取消（跳过下游但不改变入度）。
    pub fn mark_cancelled(&mut self, node_id: &str) {
        self.cancelled.push(node_id.to_string());
    }

    /// 将失败节点重置为"重新就绪"状态（重试时使用）。
    pub fn mark_retry(&mut self, node_id: &str, _plan: &TaskPlan) {
        // For retry, we don't need to change in-degrees since the node
        // was already ready (degree 0) when it started. Just remove from completed.
        self.completed.retain(|id| id != node_id);
    }

    /// 是否所有节点已完成或已取消。
    pub fn is_done(&self, plan: &TaskPlan) -> bool {
        plan.nodes.iter().all(|n| {
            let id = &n.id;
            self.completed.contains(id) || self.cancelled.contains(id)
        })
    }

    /// 返回已完成节点 ID 列表。
    pub fn completed_ids(&self) -> &[String] {
        &self.completed
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::plan::types::PlanNode;

    fn make_plan(nodes: Vec<(&str, Vec<&str>)>) -> TaskPlan {
        let mut plan = TaskPlan::new("test");
        for (id, deps) in nodes {
            plan.add_node(PlanNode::new(
                id,
                format!("Task {id}"),
                "Phase 0",
                deps.into_iter().map(String::from).collect(),
            ));
        }
        plan
    }

    #[test]
    fn test_kahn_empty_plan() {
        let plan = TaskPlan::new("empty");
        let batches = parallel_batches(&plan);
        assert!(batches.is_empty() || batches.iter().all(|b| b.is_empty()));
    }

    #[test]
    fn test_kahn_single_node() {
        let plan = make_plan(vec![("a", vec![])]);
        let batches = parallel_batches(&plan);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 1);
        assert_eq!(batches[0][0].id, "a");
    }

    #[test]
    fn test_kahn_linear_dependency() {
        let plan = make_plan(vec![("a", vec![]), ("b", vec!["a"]), ("c", vec!["b"])]);
        let batches = parallel_batches(&plan);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0][0].id, "a");
        assert_eq!(batches[1][0].id, "b");
        assert_eq!(batches[2][0].id, "c");
    }

    #[test]
    fn test_kahn_parallel_nodes() {
        let plan = make_plan(vec![
            ("a", vec![]),
            ("b", vec![]),
            ("c", vec![]),
        ]);
        let batches = parallel_batches(&plan);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 3);
    }

    #[test]
    fn test_kahn_diamond_dependency() {
        let plan = make_plan(vec![
            ("a", vec![]),
            ("b", vec!["a"]),
            ("c", vec!["a"]),
            ("d", vec!["b", "c"]),
        ]);
        let batches = parallel_batches(&plan);
        // a → [b,c] → d
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0][0].id, "a");
        assert_eq!(batches[1].len(), 2); // b and c are parallel
        assert_eq!(batches[2][0].id, "d");
    }

    #[test]
    fn test_in_degree_tracker() {
        let plan = make_plan(vec![
            ("a", vec![]),
            ("b", vec!["a"]),
            ("c", vec!["a"]),
        ]);
        let tracker = InDegreeTracker::from_plan(&plan);
        assert_eq!(tracker.get("a"), 0);
        assert_eq!(tracker.get("b"), 1);
        assert_eq!(tracker.get("c"), 1);
        assert!(!tracker.all_zero());
    }

    #[test]
    fn test_in_degree_tracker_mark_completed() {
        let plan = make_plan(vec![
            ("a", vec![]),
            ("b", vec!["a"]),
            ("c", vec!["a"]),
        ]);
        let mut tracker = InDegreeTracker::from_plan(&plan);

        let ready = tracker.mark_completed("a", &plan);
        assert_eq!(ready.len(), 2);
        assert!(ready.contains(&"b".to_string()));
        assert!(ready.contains(&"c".to_string()));

        assert_eq!(tracker.get("b"), 0);
        assert_eq!(tracker.get("c"), 0);
        assert!(tracker.all_zero());
    }

    #[test]
    fn test_dynamic_scheduler_initial_state() {
        let plan = make_plan(vec![
            ("a", vec![]),
            ("b", vec!["a"]),
        ]);
        let sched = DynamicScheduler::new(&plan);
        let ready = sched.ready_nodes(&plan);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "a");
        assert!(!sched.is_done(&plan));
    }

    #[test]
    fn test_dynamic_scheduler_mark_completed() {
        let plan = make_plan(vec![
            ("a", vec![]),
            ("b", vec!["a"]),
            ("c", vec!["a"]),
        ]);
        let mut sched = DynamicScheduler::new(&plan);

        let newly = sched.mark_completed("a", &plan);
        assert_eq!(newly.len(), 2);

        let ready = sched.ready_nodes(&plan);
        assert_eq!(ready.len(), 2);
    }

    #[test]
    fn test_dynamic_scheduler_retry() {
        let plan = make_plan(vec![
            ("a", vec![]),
            ("b", vec!["a"]),
        ]);
        let mut sched = DynamicScheduler::new(&plan);

        sched.mark_completed("a", &plan);
        sched.mark_completed("b", &plan);
        assert!(sched.is_done(&plan));

        // Retry 'b' — should become ready again
        sched.mark_retry("b", &plan);
        assert!(!sched.is_done(&plan));
        let ready = sched.ready_nodes(&plan);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "b");
    }

    #[test]
    fn test_kahn_complex_graph() {
        // A more complex graph:
        // A → B → D → F
        // A → C → E → F
        // A → C → D
        let plan = make_plan(vec![
            ("a", vec![]),
            ("b", vec!["a"]),
            ("c", vec!["a"]),
            ("d", vec!["b", "c"]),
            ("e", vec!["c"]),
            ("f", vec!["d", "e"]),
        ]);
        let batches = parallel_batches(&plan);

        // Batch 0: [a]
        // Batch 1: [b, c]
        // Batch 2: [d, e]
        // Batch 3: [f]
        assert_eq!(batches.len(), 4);
        assert_eq!(batches[0][0].id, "a");
        assert_eq!(batches[1].len(), 2); // b, c
        assert_eq!(batches[2].len(), 2); // d, e
        assert_eq!(batches[3][0].id, "f");
    }
}
