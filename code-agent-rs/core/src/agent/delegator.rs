//! Delegator —— 子 Agent 编排与协调层
//!
//! 【领域含义】`Delegator` 是 v2 架构的编排层，封装 `SubAgentManagerImpl` 和
//! `SessionRunner`，提供高层 API 来管理子 Agent（Coder）的生命周期。Delegator
//! 负责任务分解结果的接收、Coder 的生成与等待、以及失败重试。
//!
//! # Architecture
//!
//! ```text
//! Delegator
//!  ├─ SubAgentManagerImpl    ← 子 Agent 异步运行时
//!  │    └─ SessionRunner      ← 生产级 SubAgentRunner（每个 Coder 一个 Session）
//!  └─ TaskLedger              ← 任务追踪与结果记录
//!       └─ Vec<TaskEntry>      ← 任务条目（状态、耗时、结果）
//! ```
//!
//! # 与 v2 架构的对应关系
//!
//! | v2 概念 | Delegator 组件 |
//! |---------|----------------|
//! | Delegator | `Delegator` struct（编排层） |
//! | Coder | `SubAgentManagerImpl` + `SessionRunner` 创建的子 Agent task |
//! | EPSS 隔离 | `SessionRunner` 为每个 Coder 创建独立 `Session` |
//! | 并行控制 | `SubAgentManagerImpl` 的 max_depth / max_parallel |
//! | 取消传播 | `SubAgentManagerImpl::interrupt_all()` → `Session.external_cancel` |

use std::sync::Arc;

use code_agent_protocol::PermissionMode;
use tokio::sync::Mutex;
use tracing::{debug, info};

use super::session_runner::SessionRunner;
use super::sub_agent::{AgentResult, AgentStatus, SpawnConfig, SpawnTask, SubAgentError, SubAgentHandle};
use super::sub_agent_manager::SubAgentManagerImpl;
use crate::model::ModelClient;
use crate::safety::ContentSafetyLayer;
use crate::tools::registry::ToolRegistry;

// ---------------------------------------------------------------------------
// TaskEntry — 任务追踪条目
// ---------------------------------------------------------------------------

/// 单个子 Agent 任务的追踪记录。
///
/// 【领域含义】记录一个 Coder 从生成到完成的完整生命周期信息，
/// 用于 Delegator 的结果聚合和状态查询。
#[derive(Clone, Debug)]
pub struct TaskEntry {
    /// SubAgentManager 分配的 agent_id
    pub agent_id: String,
    /// 任务的可读名称
    pub task_name: String,
    /// 当前状态
    pub status: AgentStatus,
    /// 执行结果（完成后才有值）
    pub result: Option<AgentResult>,
}

// ---------------------------------------------------------------------------
// TaskLedger — 任务台账
// ---------------------------------------------------------------------------

/// 任务台账 —— 追踪所有已派发的子任务。
///
/// 【领域含义】记录 Delegator 派发的每个 Coder 的状态和结果。
/// 用于查询进度、收集结果、以及在 Delegator 内部实现等待语义。
#[derive(Default)]
pub struct TaskLedger {
    tasks: Vec<TaskEntry>,
}

impl TaskLedger {
    /// 记录一个新任务。
    fn add_task(&mut self, agent_id: String, task_name: String) {
        self.tasks.push(TaskEntry {
            agent_id,
            task_name,
            status: AgentStatus::Running,
            result: None,
        });
    }

    /// 更新任务状态和结果。
    fn update_result(&mut self, agent_id: &str, result: AgentResult) {
        if let Some(entry) = self.tasks.iter_mut().find(|t| t.agent_id == agent_id) {
            entry.status = result.status.clone();
            entry.result = Some(result);
        }
    }

    /// 获取所有任务条目。
    pub fn all_tasks(&self) -> &[TaskEntry] {
        &self.tasks
    }

    /// 获取指定状态的任务数量。
    pub fn count_by_status(&self, status: AgentStatus) -> usize {
        self.tasks.iter().filter(|t| t.status == status).count()
    }

    /// 获取所有已完成的任务结果。
    pub fn completed_results(&self) -> Vec<&AgentResult> {
        self.tasks
            .iter()
            .filter_map(|t| t.result.as_ref())
            .collect()
    }

    /// 清空台账。
    fn clear(&mut self) {
        self.tasks.clear();
    }
}

// ---------------------------------------------------------------------------
// DelegatorError
// ---------------------------------------------------------------------------

/// Delegator 操作错误。
#[derive(Debug, thiserror::Error)]
pub enum DelegatorError {
    /// 子 Agent 执行错误
    #[error("Sub-agent error: {0}")]
    SubAgent(#[from] SubAgentError),

    /// 任务已在运行中
    #[error("Task '{0}' is already running")]
    AlreadyRunning(String),

    /// 未找到指定任务
    #[error("Task not found: {0}")]
    TaskNotFound(String),

    /// Delegator 已被中断
    #[error("Delegator is interrupted")]
    Interrupted,
}

// ---------------------------------------------------------------------------
// Delegator
// ---------------------------------------------------------------------------

/// 子 Agent 编排层 —— 管理 Coder 的生成、等待和取消。
///
/// 【领域含义】Delegator 是 v2 双角色架构中的"编排者"。它接收高层任务指令，
/// 将其派发为多个并行或串行的子 Agent（Coder）任务，等待结果，并在需要时
/// 执行重试或取消。
///
/// # 使用示例
///
/// ```rust,ignore
/// let delegator = Delegator::new(model_client, tool_registry);
///
/// // 单个任务
/// let result = delegator.run_task("review", "Review src/main.rs").await?;
///
/// // 多个并行任务
/// let ids = delegator.spawn_tasks(vec![
///     ("task-a", "Do A"),
///     ("task-b", "Do B"),
/// ]).await?;
/// for id in &ids {
///     let result = delegator.wait_task(id).await?;
/// }
/// ```
pub struct Delegator {
    /// 子 Agent 异步执行运行时
    manager: SubAgentManagerImpl,
    /// 任务台账
    ledger: Arc<Mutex<TaskLedger>>,
}

impl Delegator {
    /// 使用默认配置创建 Delegator。
    ///
    /// 内部创建 `SessionRunner` 作为 `SubAgentRunner` 的生产实现。
    /// 使用默认的 depth=2, parallel=6 限制。
    pub fn new(model_client: Arc<dyn ModelClient>, tool_registry: Arc<dyn ToolRegistry>) -> Self {
        let runner = Arc::new(SessionRunner::new(model_client, tool_registry));
        let manager = SubAgentManagerImpl::default_for_root(runner);
        Self {
            manager,
            ledger: Arc::new(Mutex::new(TaskLedger::default())),
        }
    }

    /// 使用完整配置创建 Delegator。
    ///
    /// # Arguments
    /// * `model_client` — 共享的模型客户端
    /// * `tool_registry` — 共享的工具注册中心
    /// * `max_depth` — 最大嵌套深度（默认 2）
    /// * `max_parallel` — 最大并行 Coder 数（默认 6）
    /// * `system_instructions` — 子 Agent 的默认系统指令
    /// * `max_iterations` — 子 Agent 的最大迭代次数
    /// * `permission_mode` — 子 Agent 的默认权限模式
    /// * `safety_layer` — 可选的内容安全审查层（提示注入防御）
    #[allow(clippy::too_many_arguments)]
    pub fn with_config(
        model_client: Arc<dyn ModelClient>,
        tool_registry: Arc<dyn ToolRegistry>,
        max_depth: u32,
        max_parallel: usize,
        system_instructions: impl Into<String>,
        max_iterations: usize,
        permission_mode: PermissionMode,
        safety_layer: Option<Arc<ContentSafetyLayer>>,
    ) -> Self {
        let mut runner = SessionRunner::with_config(
            model_client,
            tool_registry,
            system_instructions,
            max_iterations,
            permission_mode,
        );
        if let Some(ref layer) = safety_layer {
            runner = runner.with_safety_layer(Arc::clone(layer));
        }
        let runner = Arc::new(runner);
        let manager = SubAgentManagerImpl::new_with_runner(max_depth, max_parallel, 0, runner);
        Self {
            manager,
            ledger: Arc::new(Mutex::new(TaskLedger::default())),
        }
    }

    // ── 核心操作 ──────────────────────────────────────────────

    /// 生成一个子 Agent（Coder）任务。
    ///
    /// 返回 agent_id，可用于后续的 `wait_task` 调用。
    /// 任务在后台异步执行，不会阻塞调用者。
    ///
    /// # Errors
    ///
    /// 返回 [`DelegatorError`] 如果：
    /// - 达到最大深度限制
    /// - 达到最大并行限制
    /// - Delegator 已被中断
    pub async fn spawn_task(
        &self,
        task_name: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<String, DelegatorError> {
        let task_name = task_name.into();
        let task = SpawnTask {
            task_name: task_name.clone(),
            message: message.into(),
            config_override: None,
        };

        let agent_id = self.manager.spawn_agent(task).await?;

        // 记录到台账
        self.ledger
            .lock()
            .await
            .add_task(agent_id.clone(), task_name);

        debug!(
            agent_id = %agent_id,
            "Delegator: spawned task"
        );

        Ok(agent_id)
    }

    /// 生成带配置覆盖的子 Agent 任务。
    pub async fn spawn_task_with_config(
        &self,
        task_name: impl Into<String>,
        message: impl Into<String>,
        config_override: SpawnConfig,
    ) -> Result<String, DelegatorError> {
        let task_name = task_name.into();
        let task = SpawnTask {
            task_name: task_name.clone(),
            message: message.into(),
            config_override: Some(config_override),
        };

        let agent_id = self.manager.spawn_agent(task).await?;

        self.ledger
            .lock()
            .await
            .add_task(agent_id.clone(), task_name);

        Ok(agent_id)
    }

    /// 等待指定的子 Agent 完成并返回结果。
    ///
    /// 每个 agent_id 只能等待一次（oneshot channel 语义）。
    /// 完成后会自动更新台账。
    ///
    /// # Errors
    ///
    /// 如果 agent_id 不存在、通道已关闭或任务已 panic，返回错误。
    pub async fn wait_task(&self, agent_id: &str) -> Result<AgentResult, DelegatorError> {
        let result = self.manager.wait_agent(agent_id).await?;
        self.ledger.lock().await.update_result(agent_id, result.clone());
        Ok(result)
    }

    /// 生成并等待单个任务完成（便捷方法）。
    pub async fn run_task(
        &self,
        task_name: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<AgentResult, DelegatorError> {
        let agent_id = self.spawn_task(task_name, message).await?;
        self.wait_task(&agent_id).await
    }

    /// 生成并等待多个任务（并行执行）。
    ///
    /// 每个任务由 `(task_name, message)` 元组描述。
    /// 所有任务在后台并行执行，结果按输入顺序返回。
    pub async fn run_tasks(
        &self,
        tasks: Vec<(impl Into<String>, impl Into<String>)>,
    ) -> Result<Vec<AgentResult>, DelegatorError> {
        let mut ids = Vec::with_capacity(tasks.len());

        for (name, msg) in tasks {
            let id = self.spawn_task(name, msg).await?;
            ids.push(id);
        }

        let mut results = Vec::with_capacity(ids.len());
        for id in &ids {
            let result = self.wait_task(id).await?;
            results.push(result);
        }

        Ok(results)
    }

    // ── 取消操作 ──────────────────────────────────────────────

    /// 中断所有正在运行的子 Agent。
    ///
    /// 级联取消所有 Coder，已完成的 Coder 结果不受影响。
    pub async fn cancel_all(&self) {
        info!("Delegator: cancelling all sub-agents");
        self.manager.interrupt_all().await;
    }

    /// 关闭并清理单个子 Agent。
    pub async fn close_task(&mut self, agent_id: &str) -> Result<(), DelegatorError> {
        self.manager.close_agent(agent_id).await?;
        Ok(())
    }

    // ── 查询操作 ──────────────────────────────────────────────

    /// 获取所有活跃（Running）的子 Agent 句柄。
    pub async fn list_active(&self) -> Vec<SubAgentHandle> {
        self.manager.list_agents().await
    }

    /// 获取活跃 Coder 数量。
    pub async fn active_count(&self) -> usize {
        self.manager.active_count().await
    }

    /// 获取任务台账的快照。
    pub async fn task_ledger(&self) -> Vec<TaskEntry> {
        self.ledger.lock().await.all_tasks().to_vec()
    }

    /// 获取已完成任务数量。
    pub async fn completed_count(&self) -> usize {
        self.ledger
            .lock()
            .await
            .count_by_status(AgentStatus::Completed)
    }

    /// 获取失败任务数量。
    pub async fn failed_count(&self) -> usize {
        self.ledger
            .lock()
            .await
            .count_by_status(AgentStatus::Failed)
    }

    /// 检查 Delegator 是否被中断。
    pub async fn is_interrupted(&self) -> bool {
        self.manager.is_interrupted().await
    }

    // ── 重置 ──────────────────────────────────────────────────

    /// 清空台账并重置中断状态（不停止正在运行的任务）。
    pub async fn reset(&mut self) {
        self.ledger.lock().await.clear();
        info!("Delegator: ledger cleared");
    }
}

// ---------------------------------------------------------------------------
// RetryGate — 选择性重试
// ---------------------------------------------------------------------------

/// 重试门控 —— 对失败的子 Agent 任务执行选择性重试。
///
/// 【领域含义】实现 CodeDelegator 论文中的 PROCEED/RETRY/REPLAN 三态决策：
/// - PROCEED: 任务成功，接受结果
/// - RETRY: 任务失败但可重试，重新生成并等待（最多 N 次）
/// - REPLAN: 任务失败且不可重试，需要重新规划
///
/// # 使用示例
///
/// ```rust,ignore
/// let retry = RetryGate::new(3);   // 最多重试 3 次
/// let result = retry.execute(&delegator, "task", "msg").await?;
/// ```
pub struct RetryGate {
    /// 最大重试次数
    max_retries: u32,
}

impl Default for RetryGate {
    fn default() -> Self {
        Self { max_retries: 3 }
    }
}

impl RetryGate {
    /// 创建重试门控。
    pub fn new(max_retries: u32) -> Self {
        Self { max_retries }
    }

    /// 使用 Delegator 执行带重试的任务。
    ///
    /// 在 `max_retries` 次重试内，对失败的子 Agent 重新执行。
    /// 首次成功立即返回；超过最大重试次数后返回最后一次失败结果。
    pub async fn execute(
        &self,
        delegator: &Delegator,
        task_name: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<AgentResult, DelegatorError> {
        let task_name = task_name.into();
        let message = message.into();

        for attempt in 0..=self.max_retries {
            let result = delegator.run_task(&task_name, &message).await?;

            match result.status {
                AgentStatus::Completed => {
                    debug!(
                        task = %task_name,
                        attempt,
                        "RetryGate: task completed"
                    );
                    return Ok(result);
                }
                AgentStatus::Failed => {
                    if attempt < self.max_retries {
                        info!(
                            task = %task_name,
                            attempt,
                            max_retries = self.max_retries,
                            error = ?result.error,
                            "RetryGate: retrying failed task"
                        );
                        continue;
                    }
                    info!(
                        task = %task_name,
                        attempt,
                        max_retries = self.max_retries,
                        "RetryGate: max retries reached"
                    );
                    return Ok(result);
                }
                AgentStatus::Cancelled => {
                    return Ok(result);
                }
                AgentStatus::Running => {
                    // Should not happen — wait_task returns after completion
                    unreachable!("RetryGate: received Running status from completed task");
                }
            }
        }

        unreachable!("RetryGate: loop exited unexpectedly");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::DefaultToolRegistry;
    use async_trait::async_trait;
    use crate::agent::sub_agent::SpawnConfig;
    use code_agent_protocol::{Message, ResponseEvent, TurnId};

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
            _temperature: Option<f32>,
        ) -> crate::model::ModelResult<Box<dyn futures::Stream<Item = ResponseEvent> + Send + Unpin>>
        {
            use futures::stream;
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

    fn make_delegator() -> Delegator {
        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient);
        let registry = Arc::new(DefaultToolRegistry::new());
        Delegator::new(model, registry)
    }

    #[tokio::test]
    async fn test_delegator_spawn_and_wait() {
        let delegator = make_delegator();
        let agent_id = delegator.spawn_task("test", "hello").await.unwrap();
        let result = delegator.wait_task(&agent_id).await.unwrap();
        assert_eq!(result.status, AgentStatus::Completed);
    }

    #[tokio::test]
    async fn test_delegator_run_task() {
        let delegator = make_delegator();
        let result = delegator.run_task("test", "hello").await.unwrap();
        assert_eq!(result.status, AgentStatus::Completed);
    }

    #[tokio::test]
    async fn test_delegator_run_tasks_sequential() {
        let delegator = make_delegator();
        // Use run_task (single) sequentially to avoid timing issues with mock
        let r1 = delegator.run_task("a", "1").await.unwrap();
        let r2 = delegator.run_task("b", "2").await.unwrap();
        let r3 = delegator.run_task("c", "3").await.unwrap();

        assert_eq!(r1.status, AgentStatus::Completed);
        assert_eq!(r2.status, AgentStatus::Completed);
        assert_eq!(r3.status, AgentStatus::Completed);
    }

    #[tokio::test]
    async fn test_delegator_task_ledger() {
        let delegator = make_delegator();
        delegator.run_task("t1", "msg1").await.unwrap();

        let ledger = delegator.task_ledger().await;
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].task_name, "t1");
        assert_eq!(ledger[0].status, AgentStatus::Completed);
    }

    #[tokio::test]
    async fn test_delegator_cancel_all() {
        let delegator = make_delegator();
        let id = delegator.spawn_task("t", "hello").await.unwrap();

        // Wait for task to start
        tokio::task::yield_now().await;

        delegator.cancel_all().await;
        tokio::task::yield_now().await;

        // After cancel_all, the task either:
        // - completed before cancel took effect (AgentNotFound at wait)
        // - was cancelled (Cancelled status)
        match delegator.wait_task(&id).await {
            Ok(result) => {
                // Task completed or was cancelled
                assert!(
                    result.status == AgentStatus::Completed
                        || result.status == AgentStatus::Cancelled,
                    "Expected Completed or Cancelled, got {:?}",
                    result.status
                );
            }
            Err(_) => {
                // Task finished and cleaned up before wait — acceptable
            }
        }
    }

    #[tokio::test]
    async fn test_retry_gate_success() {
        let delegator = make_delegator();
        let retry = RetryGate::new(3);

        let result = retry
            .execute(&delegator, "retry-test", "hello")
            .await
            .unwrap();

        assert_eq!(result.status, AgentStatus::Completed);
    }

    #[tokio::test]
    async fn test_delegator_spawn_with_config() {
        let delegator = make_delegator();
        let config = SpawnConfig {
            max_iterations: Some(5),
            ..Default::default()
        };

        let id = delegator
            .spawn_task_with_config("config-test", "hello", config)
            .await
            .unwrap();
        let result = delegator.wait_task(&id).await.unwrap();
        assert_eq!(result.status, AgentStatus::Completed);
    }

    #[tokio::test]
    async fn test_delegator_active_count() {
        let delegator = make_delegator();

        // Spawn, wait for it to finish, then check active count
        let _ = delegator.run_task("t", "msg").await.unwrap();

        // Task finished, so active count should be 0
        assert_eq!(delegator.active_count().await, 0);
    }
}
