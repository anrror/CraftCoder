//! PlanExecutor —— DAG 感知的 Delegator 编排器
//!
//! 【领域含义】PlanExecutor 是 Plan 引擎的执行核心。它接收一个 `TaskPlan`
//! 和 `Delegator`，按照 DAG 依赖关系逐步派发任务、收集结果、处理重试/重规划。
//!
//! # 执行流程
//!
//! ```text
//! TaskPlan → validate() → DynamicScheduler
//!              ↓
//!     while !scheduler.is_done():
//!       1. ready = scheduler.ready_nodes(plan)
//!       2. for node in ready:
//!            delegator.spawn_task(node.id, node.build_message())
//!            node.status = Running
//!       3. for each spawned task:
//!            result = delegator.wait_task(agent_id)
//!            if result.success:
//!                node.status = Completed
//!                scheduler.mark_completed(node.id, plan)
//!            else if retryable:
//!                node.retry_count += 1
//!                scheduler.mark_retry(node.id, plan)
//!            else:
//!                node.status = Failed
//!                scheduler.mark_cancelled(dependents...)
//!              ↓
//!     ExecutionReport
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;
use tracing::{error, info, warn};

use super::kahn::DynamicScheduler;
use super::knowledge::KnowledgeProvider;
use super::quality::{GateVerdict, QualityGate, QualityGatePipeline, SpecConfig};
use super::types::{
    ExecutionReport, NodeStatus, NodeSummary, PlanConfig, PlanNode, PlanProgressEvent, TaskPlan,
};
use crate::agent::delegator::{Delegator, DelegatorError};
use crate::agent::sub_agent::AgentStatus;

// ---------------------------------------------------------------------------
// PlanError
// ---------------------------------------------------------------------------

/// Plan 执行过程中的错误。
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    /// 计划验证失败
    #[error("Plan validation error: {0}")]
    Validation(String),

    /// Delegator 错误
    #[error("Delegator error: {0}")]
    Delegator(#[from] DelegatorError),

    /// 执行超时
    #[error("Plan execution timed out after {0}s")]
    Timeout(u64),

    /// 所有任务都失败（无恢复可能）
    #[error("All tasks failed: {0} failures out of {1}")]
    AllFailed(usize, usize),
}

// ---------------------------------------------------------------------------
// PlanExecutor
// ---------------------------------------------------------------------------

/// DAG 感知的 Delegator 编排器。
///
/// 【领域含义】PlanExecutor 是 Plan 引擎的执行核心。它将任务计划按 DAG 依赖
/// 排序，逐步派发并行批次给 Delegator，处理执行结果，并生成执行报告。
///
/// # 使用示例
///
/// ```rust,ignore
/// let executor = PlanExecutor::new(delegator);
/// let report = executor.execute(&mut plan).await?;
/// println!("Completed: {}/{}", report.completed_nodes, report.total_nodes);
/// ```
pub struct PlanExecutor {
    /// Delegator 引用（用于派发子任务）
    delegator: Arc<Delegator>,
    /// 执行配置
    config: PlanConfig,
    /// 质量门禁管道（Phase D）
    quality_gate: Option<Box<dyn QualityGate>>,
    /// 知识提供者（Phase E —— 注入规则/技能到分解 prompt 和 Coder）
    knowledge: Option<Arc<dyn KnowledgeProvider>>,
    /// 外部取消信号（Phase F —— 用于从 SessionRunner 传播取消）
    cancel: Option<Arc<AtomicBool>>,
    /// 进度事件发送端（Phase F —— 用于实时推送执行进度）
    progress_tx: Option<mpsc::Sender<PlanProgressEvent>>,
    /// Token 预算分配器（Phase C —— 用于为 Coder 分配上下文 Token 配额）
    budget_allocator: Option<
        Arc<std::sync::Mutex<crate::context::budget::TokenBudgetAllocator>>,
    >,
}

impl PlanExecutor {
    /// 创建 Plan 执行器。
    pub fn new(delegator: Arc<Delegator>) -> Self {
        Self {
            delegator,
            config: PlanConfig::default(),
            quality_gate: Some(Box::new(
                QualityGatePipeline::default_with_spec(SpecConfig::default()),
            )),
            knowledge: None,
            cancel: None,
            progress_tx: None,
            budget_allocator: None,
        }
    }

    /// 使用自定义配置创建 Plan 执行器。
    pub fn with_config(delegator: Arc<Delegator>, config: PlanConfig) -> Self {
        let spec = config
            .spec_config
            .clone()
            .unwrap_or_default();
        Self {
            delegator,
            config,
            quality_gate: Some(Box::new(
                QualityGatePipeline::default_with_spec(spec),
            )),
            knowledge: None,
            cancel: None,
            progress_tx: None,
            budget_allocator: None,
        }
    }

    /// 设置自定义质量门禁。
    pub fn with_quality_gate(mut self, gate: Box<dyn QualityGate>) -> Self {
        self.quality_gate = Some(gate);
        self
    }

    /// 禁用质量门禁。
    pub fn without_gate(mut self) -> Self {
        self.quality_gate = None;
        self
    }

    /// 设置知识提供者（Phase E：规则/技能注入）。
    pub fn with_knowledge(mut self, provider: Arc<dyn KnowledgeProvider>) -> Self {
        self.knowledge = Some(provider);
        self
    }

    /// 禁用知识注入。
    pub fn without_knowledge(mut self) -> Self {
        self.knowledge = None;
        self
    }

    /// 设置外部取消信号（Phase F）。
    ///
    /// 当 `cancel` 被设为 `true` 时，PlanExecutor 会在下一次循环迭代中：
    /// 1. 取消所有正在通过 Delegator 运行的任务
    /// 2. 将所有尚未完成（Pending/Ready/Running）的节点标记为 Cancelled
    /// 3. 生成报告并提前返回
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// 设置进度事件发送端（Phase F）。
    ///
    /// 执行过程中会通过此 channel 发送 [`PlanProgressEvent`]，外层调用者
    /// 可以异步接收并实时更新进度（如 UI 进度条、IDE 通知等）。
    pub fn with_progress_tx(mut self, tx: mpsc::Sender<PlanProgressEvent>) -> Self {
        self.progress_tx = Some(tx);
        self
    }

    /// 设置 Token 预算分配器（Phase C）。
    ///
    /// 当配置后，PlanExecutor 会在派发每个 Coder 节点前请求 Token 预算，
    /// 并在任务完成后释放。预算上限通过 SpawnConfig.token_budget 传递，
    /// 最终到达 SessionConfig.max_context_tokens → ContextConfig。
    pub fn with_budget_allocator(
        mut self,
        allocator: Arc<
            std::sync::Mutex<crate::context::budget::TokenBudgetAllocator>,
        >,
    ) -> Self {
        self.budget_allocator = Some(allocator);
        self
    }

    /// 发送进度事件（best-effort，忽略 channel 错误）。
    fn emit_progress(&self, event: PlanProgressEvent) {
        if let Some(ref tx) = self.progress_tx {
            let _ = tx.try_send(event);
        }
    }

    /// 检查是否已被外部取消。
    fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .map(|c| c.load(Ordering::Acquire))
            .unwrap_or(false)
    }

    /// 执行完整的任务计划。
    ///
    /// 【执行流程】
    /// 1. 验证计划（循环依赖检查）
    /// 2. 初始化动态调度器
    /// 3. 循环：检查取消 → 获取就绪节点 → 派发 → 等待结果 → 更新调度器 → 处理重试
    /// 4. 清理活跃任务 → 生成执行报告
    ///
    /// # 取消处理
    ///
    /// 如果设置了 `cancel` 信号，PlanExecutor 会在每次循环迭代开始前检查。
    /// 取消后，所有正在运行的任务会被 Delegator 取消，所有尚未完成的节点
    /// 会被标记为 Cancelled，然后返回部分执行报告。
    ///
    /// # Arguments
    /// * `plan` — 要执行的任务计划（会修改节点状态）
    ///
    /// # Returns
    /// 包含执行结果的详细报告。
    pub async fn execute(&self, plan: &mut TaskPlan) -> Result<ExecutionReport, PlanError> {
        // Step 1: 验证计划
        plan.validate()
            .map_err(|e| PlanError::Validation(e))?;

        info!(
            plan = %plan.name,
            nodes = plan.node_count(),
            phases = plan.phases.len(),
            "PlanExecutor: starting execution"
        );

        let start = Instant::now();

        // Step 2: 初始化动态调度器
        let mut scheduler = DynamicScheduler::new(plan);
        let mut active_agent_ids: Vec<(String, String)> = Vec::new(); // (agent_id, node_id)
        let mut current_phase: Option<String> = None;
        let total_nodes = plan.node_count();

        // Step 3: 主执行循环
        loop {
            // Phase F: 检查外部取消信号
            if self.is_cancelled() {
                info!("PlanExecutor: external cancel signal received");
                // 取消所有正在运行的任务
                if !active_agent_ids.is_empty() {
                    self.delegator.cancel_all().await;
                    // 将 active 节点标记为 cancelled
                    for (_, node_id) in &active_agent_ids {
                        if let Some(n) = plan.get_node_mut(node_id) {
                            n.status = NodeStatus::Cancelled;
                        }
                        scheduler.mark_cancelled(node_id);
                        if let Some(n) = plan.get_node(node_id) {
                            self.emit_progress(PlanProgressEvent::NodeCancelled {
                                node_id: n.id.clone(),
                                label: n.label.clone(),
                                phase: n.phase.clone(),
                            });
                        }
                    }
                    active_agent_ids.clear();
                }
                // 将所有 Pending/Ready 节点标记为 cancelled
                let remaining_ids: Vec<String> = plan
                    .nodes
                    .iter()
                    .filter(|n| !n.status.is_terminal())
                    .map(|n| n.id.clone())
                    .collect();
                for node_id in &remaining_ids {
                    if let Some(n) = plan.get_node_mut(node_id) {
                        n.status = NodeStatus::Cancelled;
                    }
                    scheduler.mark_cancelled(node_id);
                }
                // 发出 cancel progress event
                let completed = scheduler.completed_ids().len();
                self.emit_progress(PlanProgressEvent::PlanCompleted {
                    success: false,
                    completed,
                    failed: 0,
                    cancelled: total_nodes - completed,
                    total: total_nodes,
                });
                break;
            }

            // 检查是否全部完成
            if scheduler.is_done(plan) {
                break;
            }

            // 等待当前活跃批次完成
            if !active_agent_ids.is_empty() {
                let (agent_id, node_id) = active_agent_ids.remove(0);
                self.process_task_result(plan, &mut scheduler, &node_id, &agent_id)
                    .await?;
                continue;
            }

            // 获取就绪节点（收集 ID 避免 borrow 冲突）
            let ready_ids: Vec<String> = scheduler
                .ready_nodes(plan)
                .iter()
                .map(|n| n.id.clone())
                .collect();
            if ready_ids.is_empty() {
                // 没有就绪节点且没有活跃任务 → 阻塞或失败
                if scheduler.is_done(plan) {
                    break;
                }
                // 不应该发生：有任务未完成但没有就绪节点且没有活跃任务
                let pending = plan.node_count() - scheduler.completed_ids().len();
                warn!(
                    pending,
                    "PlanExecutor: no ready nodes and no active tasks — possible deadlock"
                );
                // 检查是否有 Failed 节点阻塞了依赖链
                let failed_nodes: Vec<&PlanNode> = plan
                    .nodes
                    .iter()
                    .filter(|n| n.status == NodeStatus::Failed)
                    .collect();
                if !failed_nodes.is_empty() {
                    let failed_ids: Vec<&str> = failed_nodes.iter().map(|n| n.id.as_str()).collect();
                    error!(
                        ?failed_ids,
                        "PlanExecutor: blocked by failed nodes"
                    );
                    return Err(PlanError::AllFailed(failed_ids.len(), plan.node_count()));
                }
                break;
            }

            // Step 4: 派发就绪节点（按 config.max_parallel 限制）
            let batch_size = self.config.max_parallel.min(ready_ids.len());
            // Phase E: 构建消息并注入知识上下文
            let mut ready_tasks: Vec<(String, String)> = Vec::with_capacity(batch_size);
            for id in ready_ids.iter().take(batch_size) {
                if let Some(n) = plan.nodes.iter().find(|n| n.id == *id) {
                    let mut message = n.build_message();
                    // Phase E: 注入知识上下文到子任务消息中
                    if let Some(ref kp) = self.knowledge {
                        let query = format!(
                            "{} {}",
                            n.label,
                            n.spec.as_deref().unwrap_or("")
                        );
                        let ctx = kp.format_context(&query).await;
                        if !ctx.is_empty() {
                            message = format!(
                                "{}\n\n---\n{}",
                                message, ctx
                            );
                        }
                    }
                    ready_tasks.push((n.id.clone(), message));
                }
            }
            for (node_id, message) in &ready_tasks {
                // 发出 PhaseChanged 事件（首次进入该阶段时）
                if let Some(n) = plan.get_node(node_id) {
                    if current_phase.as_deref() != Some(&n.phase) {
                        current_phase = Some(n.phase.clone());
                        self.emit_progress(PlanProgressEvent::PhaseChanged {
                            phase: n.phase.clone(),
                            completed: scheduler.completed_ids().len(),
                            total: total_nodes,
                        });
                    }
                    // 发出 NodeStarted 事件
                    self.emit_progress(PlanProgressEvent::NodeStarted {
                        node_id: n.id.clone(),
                        label: n.label.clone(),
                        phase: n.phase.clone(),
                    });
                }

                let agent_id = self.dispatch_node_with_message(plan, node_id, message)
                    .await?;
                active_agent_ids.push((agent_id, node_id.clone()));
            }
        }

        // 取消所有仍在运行的 Delegator 任务
        if self.delegator.active_count().await > 0 {
            self.delegator.cancel_all().await;
        }

        let duration_ms = start.elapsed().as_millis();

        // Step 5: 生成报告
        let report = self.build_report(plan, duration_ms);

        // 发出 PlanCompleted 事件
        self.emit_progress(PlanProgressEvent::PlanCompleted {
            success: report.success,
            completed: report.completed_nodes,
            failed: report.failed_nodes,
            cancelled: report.cancelled_nodes,
            total: report.total_nodes,
        });

        info!(
            completed = report.completed_nodes,
            total = report.total_nodes,
            duration_ms = report.duration_ms,
            success = report.success,
            "PlanExecutor: execution complete"
        );

        Ok(report)
    }

    /// 派发单个节点到 Delegator（使用 node_id 和 message 字符串，避免 borrow 冲突）。
    /// 返回实际分配的 agent_id。
    async fn dispatch_node_with_message(
        &self,
        plan: &mut TaskPlan,
        node_id: &str,
        message: &str,
    ) -> Result<String, PlanError> {
        // Update node status
        if let Some(n) = plan.get_node_mut(node_id) {
            n.status = NodeStatus::Running;
        }

        // Request token budget if allocator is configured
        let spawn_config = self.budget_allocator.as_ref().and_then(|allocator| {
            let mut alloc = allocator.lock().unwrap();
            alloc
                .request(crate::context::budget::TokenRole::Coder, node_id)
                .map(|a| crate::agent::sub_agent::SpawnConfig {
                    token_budget: Some(a.granted),
                    ..Default::default()
                })
        });

        // Spawn via Delegator
        let spawn_result = if let Some(config) = spawn_config {
            self.delegator
                .spawn_task_with_config(
                    format!("plan-{}", node_id),
                    message.to_string(),
                    config,
                )
                .await
        } else {
            self.delegator
                .spawn_task(format!("plan-{}", node_id), message.to_string())
                .await
        };

        let agent_id = spawn_result.map_err(|e| {
            error!(
                node = %node_id,
                error = %e,
                "PlanExecutor: failed to dispatch node"
            );
            if let Some(n) = plan.get_node_mut(node_id) {
                n.status = NodeStatus::Failed;
            }
            PlanError::from(e)
        })?;

        let label = plan.get_node(node_id).map(|n| n.label.clone()).unwrap_or_default();
        info!(
            node = %node_id,
            agent_id = %agent_id,
            label = %label,
            "PlanExecutor: dispatched node"
        );

        Ok(agent_id)
    }

    /// 处理单个任务的执行结果。
    async fn process_task_result(
        &self,
        plan: &mut TaskPlan,
        scheduler: &mut DynamicScheduler,
        node_id: &str,
        agent_id: &str,
    ) -> Result<(), PlanError> {
        // Release token budget when task completes
        if let Some(ref allocator) = self.budget_allocator {
            let mut alloc = allocator.lock().unwrap();
            alloc.release(&crate::context::budget::TokenRole::Coder, node_id);
        }

        match self.delegator.wait_task(agent_id).await {
            Ok(result) => {
                let (node_label, node_phase) = plan
                    .get_node(node_id)
                    .map(|n| (n.label.clone(), n.phase.clone()))
                    .unwrap_or_default();

                match result.status {
                    AgentStatus::Completed => {
                        let node_clone = plan.get_node(node_id).cloned();
                        let result_clone = result.clone();
                        info!(
                            node = %node_id,
                            label = %node_label,
                            "PlanExecutor: node completed"
                        );

                        // Phase D: Quality Gate check
                        let gate_verdict = if self.config.gate_enabled {
                            if let (Some(ref gate), Some(ref node_ref)) =
                                (&self.quality_gate, &node_clone)
                            {
                                let v = gate.check(&result_clone, node_ref, &self.config).await;
                                Some(v)
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                        match gate_verdict {
                            Some(GateVerdict::Pass) | None => {
                                // Pass or gate disabled → normal completion
                                if let Some(n) = plan.get_node_mut(node_id) {
                                    n.status = NodeStatus::Completed;
                                    n.result = Some(result);
                                    n.gate_verdict = gate_verdict;
                                }
                                scheduler.mark_completed(node_id, plan);
                                // Phase F: emit progress event
                                self.emit_progress(PlanProgressEvent::NodeCompleted {
                                    node_id: node_id.to_string(),
                                    label: node_label,
                                    phase: node_phase,
                                });
                            }
                            Some(GateVerdict::Retry { .. }) => {
                                info!(
                                    node = %node_id,
                                    label = %node_label,
                                    verdict = ?gate_verdict,
                                    "PlanExecutor: gate triggered retry"
                                );
                                let n = plan.get_node(node_id);
                                let should_retry = n
                                    .map(|n| n.retry_count < n.max_retries)
                                    .unwrap_or(false);
                                if should_retry && self.config.enable_replan {
                                    if let Some(n) = plan.get_node_mut(node_id) {
                                        n.retry_count += 1;
                                        n.status = NodeStatus::Pending;
                                        n.gate_verdict = gate_verdict;
                                        // Don't store result — we'll retry
                                    }
                                    scheduler.mark_retry(node_id, plan);
                                } else {
                                    // Retries exhausted or replan disabled
                                    let err_reason = gate_verdict
                                        .as_ref()
                                        .and_then(|v| v.reason().map(String::from))
                                        .unwrap_or_else(|| "Retry exhausted".into());
                                    if let Some(n) = plan.get_node_mut(node_id) {
                                        n.status = NodeStatus::Failed;
                                        n.result = Some(result);
                                        n.gate_verdict = gate_verdict;
                                    }
                                    scheduler.mark_cancelled(node_id);
                                    // Phase F: emit progress event
                                    self.emit_progress(PlanProgressEvent::NodeFailed {
                                        node_id: node_id.to_string(),
                                        label: node_label,
                                        phase: node_phase,
                                        error: err_reason,
                                    });
                                }
                            }
                            Some(GateVerdict::Fail { .. }) => {
                                let err_reason = gate_verdict
                                    .as_ref()
                                    .and_then(|v| v.reason().map(String::from))
                                    .unwrap_or_else(|| "Gate rejected".into());
                                warn!(
                                    node = %node_id,
                                    label = %node_label,
                                    verdict = ?gate_verdict,
                                    "PlanExecutor: gate rejected node"
                                );
                                if let Some(n) = plan.get_node_mut(node_id) {
                                    n.status = NodeStatus::Failed;
                                    n.result = Some(result);
                                    n.gate_verdict = gate_verdict;
                                }
                                scheduler.mark_cancelled(node_id);
                                // Phase F: emit progress event
                                self.emit_progress(PlanProgressEvent::NodeFailed {
                                    node_id: node_id.to_string(),
                                    label: node_label,
                                    phase: node_phase,
                                    error: err_reason,
                                });
                            }
                        }
                    }
                    AgentStatus::Failed => {
                        let err_msg = result
                            .error
                            .clone()
                            .unwrap_or_else(|| "Unknown error".into());
                        warn!(
                            node = %node_id,
                            label = %node_label,
                            error = %err_msg,
                            "PlanExecutor: node failed"
                        );

                        let should_retry = plan
                            .get_node(node_id)
                            .map(|n| n.retry_count < n.max_retries)
                            .unwrap_or(false);

                        if should_retry && self.config.enable_replan {
                            // RETRY
                            if let Some(n) = plan.get_node_mut(node_id) {
                                n.retry_count += 1;
                                n.status = NodeStatus::Pending; // Will become Ready again
                            }
                            scheduler.mark_retry(node_id, plan);
                        } else {
                            // Max retries exceeded — mark as failed
                            if let Some(n) = plan.get_node_mut(node_id) {
                                n.status = NodeStatus::Failed;
                                n.result = Some(result);
                            }
                            scheduler.mark_cancelled(node_id);
                            // Phase F: emit progress event
                            self.emit_progress(PlanProgressEvent::NodeFailed {
                                node_id: node_id.to_string(),
                                label: node_label,
                                phase: node_phase,
                                error: err_msg,
                            });
                        }
                    }
                    AgentStatus::Cancelled => {
                        info!(
                            node = %node_id,
                            label = %node_label,
                            "PlanExecutor: node cancelled"
                        );
                        if let Some(n) = plan.get_node_mut(node_id) {
                            n.status = NodeStatus::Cancelled;
                            n.result = Some(result);
                        }
                        scheduler.mark_cancelled(node_id);
                        // Phase F: emit progress event
                        self.emit_progress(PlanProgressEvent::NodeCancelled {
                            node_id: node_id.to_string(),
                            label: node_label,
                            phase: node_phase,
                        });
                    }
                    AgentStatus::Running => {
                        // Should not happen from wait_task
                        warn!(
                            node = %node_id,
                            "PlanExecutor: received Running status from completed task"
                        );
                    }
                }
            }
            Err(e) => {
                let node_label = plan
                    .get_node(node_id)
                    .map(|n| n.label.clone())
                    .unwrap_or_default();
                let node_phase = plan
                    .get_node(node_id)
                    .map(|n| n.phase.clone())
                    .unwrap_or_default();
                error!(
                    node = %node_id,
                    error = %e,
                    "PlanExecutor: wait_task failed"
                );
                // Mark as failed if we can't wait for it
                if let Some(n) = plan.get_node_mut(node_id) {
                    n.status = NodeStatus::Failed;
                }
                scheduler.mark_cancelled(node_id);
                // Phase F: emit progress event
                self.emit_progress(PlanProgressEvent::NodeFailed {
                    node_id: node_id.to_string(),
                    label: node_label,
                    phase: node_phase,
                    error: format!("wait_task error: {e}"),
                });
            }
        }

        Ok(())
    }

    /// 构建执行报告。
    fn build_report(&self, plan: &TaskPlan, duration_ms: u128) -> ExecutionReport {
        let mut completed = 0;
        let mut failed = 0;
        let mut cancelled = 0;
        let mut summaries = Vec::new();

        for node in &plan.nodes {
            let status = node.status.clone();
            match &node.status {
                NodeStatus::Completed => completed += 1,
                NodeStatus::Failed => failed += 1,
                NodeStatus::Cancelled => cancelled += 1,
                _ => {}
            }

            let (error, final_msg, gate_str) = match &node.result {
                Some(r) => (r.error.clone(), r.final_message.clone(), None),
                None => (None, None, node.gate_verdict.as_ref().map(|v| format!("{:?}", v))),
            };
            // If a gate verdict exists and is not Pass, prefer that over actual error
            let gate_verdict_str = if node.gate_verdict.as_ref().map_or(false, |v| !v.is_pass()) {
                node.gate_verdict.as_ref().map(|v| format!("{:?}", v))
            } else {
                gate_str
            };

            summaries.push(NodeSummary {
                id: node.id.clone(),
                label: node.label.clone(),
                phase: node.phase.clone(),
                status,
                error: error.or_else(|| {
                    node.gate_verdict.as_ref().and_then(|v| v.reason().map(String::from))
                }),
                final_message_summary: final_msg.map(|m| {
                    if m.len() > 100 {
                        format!("{}...", &m[..100])
                    } else {
                        m
                    }
                }),
                retry_count: node.retry_count,
                gate_verdict: gate_verdict_str,
            });
        }

        ExecutionReport {
            plan_name: plan.name.clone(),
            total_nodes: plan.node_count(),
            completed_nodes: completed,
            failed_nodes: failed,
            cancelled_nodes: cancelled,
            duration_ms,
            node_summaries: summaries,
            success: failed == 0 && cancelled == 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::delegator::Delegator;
    use crate::agent::plan::types::PlanNode;
    use crate::agent::sub_agent::AgentResult;
    use crate::model::ModelClient;
    use crate::tools::registry::DefaultToolRegistry;
    use async_trait::async_trait;
    use code_agent_protocol::{Message, ResponseEvent, TurnId};
    use futures::stream;
    use std::sync::Arc;

    struct MockModelClient;

    #[async_trait]
    impl ModelClient for MockModelClient {
        fn model_name(&self) -> &str {
            "mock"
        }

        async fn complete_stream(
            &self,
            _messages: &[Message],
            _tools: &[crate::model::types::ToolDefinition],
        ) -> crate::model::ModelResult<
            Box<dyn futures::Stream<Item = ResponseEvent> + Send + Unpin>,
        > {
            Ok(Box::new(stream::iter(vec![ResponseEvent::TurnComplete {
                turn_id: TurnId("t".into()),
                final_message: Message::AssistantMessage {
                    content: "ok".into(),
                },
            }])))
        }

        fn last_token_usage(&self) -> Option<crate::model::types::TokenUsage> {
            None
        }
    }

    fn make_delegator() -> Arc<Delegator> {
        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient);
        let registry = Arc::new(DefaultToolRegistry::new());
        Arc::new(Delegator::new(model, registry))
    }

    /// 集成测试验证 PlanExecutor ↔ Delegator 全链路。
    /// 修复历史：
    /// 1. SubAgentManagerImpl 通道竞态：wait_agent 消费结果后清理通道，
    ///    而非 spawned task 提前清理（避免 wait_agent 读已删除的通道）。
    /// 2. PlanExecutor agent_id 不匹配：dispatch_node_with_message 现在
    ///    返回真实的 agent_id（如 subagent-0），而非构造的 plan-{node_id}，
    ///    确保 wait_task 能找到正确的通道。

    #[tokio::test]
    async fn test_executor_single_node() {
        let delegator = make_delegator();
        let executor = PlanExecutor::new(delegator);

        let mut plan = TaskPlan::new("test");
        plan.add_node(PlanNode::new("t1", "Task 1", "Phase 0", vec![]));

        let report = executor.execute(&mut plan).await.unwrap();
        assert_eq!(report.total_nodes, 1);
        assert_eq!(report.completed_nodes, 1);
        assert!(report.success);
    }

    #[tokio::test]
    async fn test_executor_linear_deps() {
        let delegator = make_delegator();
        let executor = PlanExecutor::new(delegator);

        let mut plan = TaskPlan::new("test");
        plan.add_node(PlanNode::new("a", "A", "Phase 0", vec![]));
        plan.add_node(PlanNode::new("b", "B", "Phase 1", vec!["a".into()]));
        plan.add_node(PlanNode::new("c", "C", "Phase 2", vec!["b".into()]));

        let report = executor.execute(&mut plan).await.unwrap();
        assert_eq!(report.total_nodes, 3);
        assert_eq!(report.completed_nodes, 3);
        assert!(report.success);
    }

    #[tokio::test]
    async fn test_executor_parallel_nodes() {
        let delegator = make_delegator();
        let executor = PlanExecutor::new(delegator);

        let mut plan = TaskPlan::new("test");
        plan.add_node(PlanNode::new("a", "A", "Phase 0", vec![]));
        plan.add_node(PlanNode::new("b", "B", "Phase 0", vec![]));
        plan.add_node(PlanNode::new("c", "C", "Phase 0", vec![]));

        let report = executor.execute(&mut plan).await.unwrap();
        assert_eq!(report.total_nodes, 3);
        assert_eq!(report.completed_nodes, 3);
        assert!(report.success);
    }

    #[tokio::test]
    async fn test_executor_diamond_deps() {
        let delegator = make_delegator();
        let executor = PlanExecutor::new(delegator);

        let mut plan = TaskPlan::new("test");
        plan.add_node(PlanNode::new("a", "A", "Phase 0", vec![]));
        plan.add_node(PlanNode::new("b", "B", "Phase 1", vec!["a".into()]));
        plan.add_node(PlanNode::new("c", "C", "Phase 1", vec!["a".into()]));
        plan.add_node(PlanNode::new("d", "D", "Phase 2", vec!["b".into(), "c".into()]));

        let report = executor.execute(&mut plan).await.unwrap();
        assert_eq!(report.total_nodes, 4);
        assert_eq!(report.completed_nodes, 4);
        assert!(report.success);
    }

    #[tokio::test]
    async fn test_executor_invalid_plan() {
        let delegator = make_delegator();
        let executor = PlanExecutor::new(delegator);

        let mut plan = TaskPlan::new("test");
        // Node references non-existent dependency
        plan.add_node(PlanNode::new("a", "A", "Phase 0", vec!["nonexistent".into()]));

        let result = executor.execute(&mut plan).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[tokio::test]
    async fn test_executor_empty_plan() {
        let delegator = make_delegator();
        let executor = PlanExecutor::new(delegator);

        let mut plan = TaskPlan::new("empty");
        let report = executor.execute(&mut plan).await.unwrap();
        assert_eq!(report.total_nodes, 0);
        assert!(report.success);
    }

    #[test]
    fn test_build_report() {
        let delegator = make_delegator();
        let executor = PlanExecutor::new(delegator);

        let mut plan = TaskPlan::new("test");
        plan.add_node(PlanNode::new("a", "A", "Phase 0", vec![]));
        plan.get_node_mut("a").unwrap().status = NodeStatus::Completed;

        let report = executor.build_report(&plan, 42);
        assert_eq!(report.total_nodes, 1);
        assert_eq!(report.completed_nodes, 1);
        assert_eq!(report.duration_ms, 42);
        assert!(report.success);
    }

    #[tokio::test]
    async fn test_executor_node_status_flow() {
        let delegator = make_delegator();
        let executor = PlanExecutor::new(delegator);

        let mut plan = TaskPlan::new("test");
        plan.add_node(PlanNode::new("t1", "Task 1", "Phase 0", vec![]));

        executor.execute(&mut plan).await.unwrap();

        // After execution, the node should be Completed
        let node = plan.get_node("t1").unwrap();
        assert_eq!(node.status, NodeStatus::Completed);
        assert!(node.result.is_some());
    }

    #[tokio::test]
    async fn test_executor_web_project_plan() {
        let delegator = make_delegator();
        let executor = PlanExecutor::new(delegator);

        // Use the standard web project plan
        let mut plan = super::super::decomposition::make_web_project_plan();
        assert!(plan.validate().is_ok());

        let report = executor.execute(&mut plan).await.unwrap();
        assert_eq!(report.total_nodes, 7);
        assert_eq!(report.completed_nodes, 7);
        assert!(report.success);
    }

    // ── Quality Gate Integration Tests ────────────────────────────────

    /// A gate that always rejects with Fail.
    struct RejectGate;

    #[async_trait::async_trait]
    impl QualityGate for RejectGate {
        fn name(&self) -> &str {
            "RejectGate"
        }

        async fn check(
            &self,
            _result: &AgentResult,
            node: &PlanNode,
            _config: &PlanConfig,
        ) -> GateVerdict {
            GateVerdict::Fail {
                reason: format!("RejectGate always fails node '{}'", node.label),
            }
        }
    }

    /// A gate that always triggers Retry (up to max_retries, then Fail).
    struct RetryGate;

    #[async_trait::async_trait]
    impl QualityGate for RetryGate {
        fn name(&self) -> &str {
            "RetryGate"
        }

        async fn check(
            &self,
            _result: &AgentResult,
            node: &PlanNode,
            _config: &PlanConfig,
        ) -> GateVerdict {
            GateVerdict::Retry {
                reason: format!("RetryGate wants retry for node '{}'", node.label),
            }
        }
    }

    #[tokio::test]
    async fn test_executor_gate_reject_fails_node() {
        let delegator = make_delegator();
        let executor = PlanExecutor::new(delegator)
            .with_quality_gate(Box::new(RejectGate));

        let mut plan = TaskPlan::new("test");
        plan.add_node(PlanNode::new("t1", "Task 1", "Phase 0", vec![]));

        let report = executor.execute(&mut plan).await.unwrap();
        assert_eq!(report.total_nodes, 1);
        assert_eq!(report.completed_nodes, 0);
        assert_eq!(report.failed_nodes, 1);
        assert!(!report.success);

        let node = plan.get_node("t1").unwrap();
        assert_eq!(node.status, NodeStatus::Failed);
        assert_eq!(node.retry_count, 0);
    }

    #[tokio::test]
    async fn test_executor_gate_retry_then_exhausted() {
        let delegator = make_delegator();
        let executor = PlanExecutor::new(delegator)
            .with_quality_gate(Box::new(RetryGate));

        let mut plan = TaskPlan::new("test");
        let mut node = PlanNode::new("t1", "Task 1", "Phase 0", vec![]);
        node.max_retries = 3; // Will retry 3 times, then fail
        plan.add_node(node);

        let report = executor.execute(&mut plan).await.unwrap();
        assert_eq!(report.total_nodes, 1);
        assert_eq!(report.completed_nodes, 0);
        assert_eq!(report.failed_nodes, 1);
        assert!(!report.success);

        let n = plan.get_node("t1").unwrap();
        assert_eq!(n.status, NodeStatus::Failed);
        assert_eq!(n.retry_count, 3);
    }

    #[tokio::test]
    async fn test_executor_gate_disabled_still_passes() {
        let delegator = make_delegator();
        let mut config = PlanConfig::default();
        config.gate_enabled = false;
        let executor = PlanExecutor::with_config(delegator, config)
            .with_quality_gate(Box::new(RejectGate));

        let mut plan = TaskPlan::new("test");
        plan.add_node(PlanNode::new("t1", "Task 1", "Phase 0", vec![]));

        let report = executor.execute(&mut plan).await.unwrap();
        // Gate disabled → node completes normally despite RejectGate
        assert_eq!(report.total_nodes, 1);
        assert_eq!(report.completed_nodes, 1);
        assert!(report.success);

        let node = plan.get_node("t1").unwrap();
        assert_eq!(node.status, NodeStatus::Completed);
    }
}
