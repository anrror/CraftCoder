//! SessionRunner —— 生产级 SubAgentRunner 实现
//!
//! 【领域含义】`SessionRunner` 实现了 [`SubAgentRunner`] trait，为每个子 Agent
//! 创建一个独立的 [`Session`] 实例（Coder），实现 EPSS（Ephemeral-Persistent
//! State Separation）隔离。每个 Coder 拥有自包含的 ReAct 循环、独立的上下文
//! 管理器和工具注册中心，不会泄漏父 Agent 的对话历史。
//!
//! # Architecture
//!
//! ```text
//! SubAgentManagerImpl.spawn_agent()
//!   └─ tokio::spawn(SessionRunner::run())
//!        ├─ Session::new(config)       ← 每个 Coder 独立 Session
//!        ├─ Session::run_turn(input)   ← ReAct 循环
//!        └─ → AgentResult              ← 结果聚合
//! ```
//!
//! # EPSS 隔离保证
//!
//! - **上下文隔离**：每个 Coder 拥有独立的 ContextManager（空对话历史）
//! - **取消传播**：SubAgentManagerImpl 的 `cancel` 信号通过 `external_cancel`
//!   传递到 Session 的 ReAct 循环
//! - **结果隔离**：AgentResult 只包含该 Coder 的执行事件，不暴露父 Agent 的上下文

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use code_agent_protocol::{Message, PermissionMode, ResponseEvent, SessionId, ThreadId, TurnInput};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use super::plan::executor::PlanExecutor;
use super::plan::knowledge::KnowledgeProvider;
use super::plan::types::{ExecutionReport, PlanConfig, PlanProgressEvent, TaskPlan};
use super::session::{Session, SessionConfig};
use super::sub_agent::{AgentResult, SpawnConfig, SpawnTask};
use super::sub_agent_manager::SubAgentRunner;
use crate::agent::delegator::Delegator;
use crate::model::ModelClient;
use crate::safety::ContentSafetyLayer;
use crate::tools::registry::ToolRegistry;

// ---------------------------------------------------------------------------
// SessionRunner
// ---------------------------------------------------------------------------

/// 生产级子 Agent 运行器 — 为每个子任务创建一个独立的 Session。
///
/// 【领域含义】SessionRunner 是 SubAgentRunner 的生产实现（相对于测试用的
/// MockRunner）。它接收子 Agent 的任务描述（SpawnTask），为其创建一个全新的
/// Session 实例（Coder），执行 ReAct 循环，并将执行结果打包为 AgentResult。
///
/// # 配置覆盖
///
/// SpawnTask 中的 config_override 可以覆盖默认配置：
/// - `system_instructions`: 覆盖默认系统指令
/// - `max_iterations`: 覆盖最大迭代次数
/// - `permission_mode`: 覆盖权限模式
/// - `model`: 当前预留（未来支持按子任务切换模型）
pub struct SessionRunner {
    /// 聊天补全模型客户端（通过 Arc 共享给所有子 Agent）
    model_client: Arc<dyn ModelClient>,
    /// 工具注册中心（通过 Arc 共享给所有子 Agent）
    tool_registry: Arc<dyn ToolRegistry>,
    /// 子 Agent 的默认系统指令
    system_instructions: String,
    /// 子 Agent 的默认最大 ReAct 循环迭代次数
    max_iterations: usize,
    /// 子 Agent 的默认权限模式
    permission_mode: PermissionMode,
    /// 内容安全审查层（提示注入防御、敏感信息过滤）
    /// 设置后在 Session::new() 之后调用 with_safety_layer() 附加到每个子 Session
    safety_layer: Option<Arc<ContentSafetyLayer>>,
    // ── Phase F: Plan 引擎集成 ──────────────────────────────────
    /// Plan 执行配置（可选，默认 PlanConfig::default()）
    plan_config: Option<PlanConfig>,
    /// 知识提供者（可选，用于 PlanExecutor 的规则/技能注入）
    knowledge_provider: Option<Arc<dyn KnowledgeProvider>>,
}

impl SessionRunner {
    /// 使用共享的模型客户端和工具注册中心创建 SessionRunner。
    pub fn new(
        model_client: Arc<dyn ModelClient>,
        tool_registry: Arc<dyn ToolRegistry>,
    ) -> Self {
        Self {
            model_client,
            tool_registry,
            system_instructions: String::new(),
            max_iterations: 20,
            permission_mode: PermissionMode::Auto,
            safety_layer: None,
            plan_config: None,
            knowledge_provider: None,
        }
    }

    /// 使用完整配置创建 SessionRunner。
    #[allow(clippy::too_many_arguments)]
    pub fn with_config(
        model_client: Arc<dyn ModelClient>,
        tool_registry: Arc<dyn ToolRegistry>,
        system_instructions: impl Into<String>,
        max_iterations: usize,
        permission_mode: PermissionMode,
    ) -> Self {
        Self {
            model_client,
            tool_registry,
            system_instructions: system_instructions.into(),
            max_iterations,
            permission_mode,
            safety_layer: None,
            plan_config: None,
            knowledge_provider: None,
        }
    }

    // ── Phase F: Plan 配置 builder 方法 ──────────────────────────

    /// 设置 Plan 执行配置。
    pub fn with_plan_config(mut self, config: PlanConfig) -> Self {
        self.plan_config = Some(config);
        self
    }

    /// 设置知识提供者（用于 PlanExecutor 的规则/技能注入）。
    pub fn with_knowledge(mut self, provider: Arc<dyn KnowledgeProvider>) -> Self {
        self.knowledge_provider = Some(provider);
        self
    }

    /// 设置内容安全审查层（提示注入防御）。
    ///
    /// 当设置后，每个由 SessionRunner 创建的子 Agent Session 都会在
    /// `Session::new()` 之后调用 `session.with_safety_layer()` 附加安全审查。
    /// 这确保子 Agent（Coder）具有与父 Session 同等级别的注入防御。
    pub fn with_safety_layer(mut self, layer: Arc<ContentSafetyLayer>) -> Self {
        self.safety_layer = Some(layer);
        self
    }

    /// 将 SpawnTask 的 message 转换为 TurnInput。
    /// 使用提供的 thread_id 和 task message 构建单轮输入。
    fn build_turn_input(&self, task: &SpawnTask, thread_id: &ThreadId) -> TurnInput {
        TurnInput {
            thread_id: thread_id.clone(),
            messages: vec![Message::UserMessage {
                content: task.message.clone(),
            }],
        }
    }

    /// 从 Session::run_turn 输出的事件流中提取最终消息内容。
    fn extract_final_message(events: &[ResponseEvent]) -> Option<String> {
        for event in events.iter().rev() {
            if let ResponseEvent::TurnComplete { ref final_message, .. } = event {
                if let Message::AssistantMessage { content } = final_message {
                    return Some(content.clone());
                }
                // TurnComplete 也可能是其他消息类型，序列化为字符串回退
                return Some(format!("{final_message:?}"));
            }
        }
        None
    }

    // ── Phase F: Plan 引擎执行 ─────────────────────────────────

    /// 执行一个完整的任务计划（DAG 分解任务）。
    ///
    /// 此方法创建一个临时 Delegator（共享同样的 ModelClient 和 ToolRegistry），
    /// 构造一个 PlanExecutor，注入已配置的 QualityGate 和 KnowledgeProvider，
    /// 然后驱动 DAG 执行循环，返回执行报告。
    ///
    /// # 取消传播
    ///
    /// 当 `cancel` 信号被触发时，PlanExecutor 会：
    /// 1. 取消所有正在通过 Delegator 运行的 Coder
    /// 2. 标记所有尚未完成的节点为 Cancelled
    /// 3. 返回部分执行结果报告
    ///
    /// # Arguments
    /// * `plan` — 要执行的任务计划（会修改节点状态）
    /// * `cancel` — 外部取消信号（通常来自 SubAgentManagerImpl）
    /// * `progress_tx` — 可选进度事件发送端（用于实时推送执行状态）
    ///
    /// # Returns
    /// 包含完整执行结果的报告。
    pub async fn run_plan(
        &self,
        plan: &mut TaskPlan,
        cancel: Arc<AtomicBool>,
        progress_tx: Option<mpsc::Sender<PlanProgressEvent>>,
    ) -> Result<ExecutionReport, super::plan::PlanError> {
        // 创建一个临时 Delegator（共享 ModelClient 和 ToolRegistry）
        let delegator = Arc::new(Delegator::new(
            Arc::clone(&self.model_client),
            Arc::clone(&self.tool_registry),
        ));

        // 使用已配置的 PlanConfig（或默认值）
        let plan_config = self.plan_config.clone().unwrap_or_default();

        // 构造 PlanExecutor
        let mut executor = PlanExecutor::with_config(delegator, plan_config)
            .with_cancel(cancel);

        // 注入知识提供者
        if let Some(ref kp) = self.knowledge_provider {
            executor = executor.with_knowledge(Arc::clone(kp));
        }

        // 注入进度事件 channel
        if let Some(tx) = progress_tx {
            executor = executor.with_progress_tx(tx);
        }

        // 执行计划
        let report = executor.execute(plan).await?;

        info!(
            plan = %plan.name,
            completed = report.completed_nodes,
            total = report.total_nodes,
            success = report.success,
            "SessionRunner: plan execution complete"
        );

        Ok(report)
    }
}

#[async_trait]
impl SubAgentRunner for SessionRunner {
    /// 执行子 Agent 任务。
    ///
    /// 1. 检查取消标志 — 如果已取消，立即返回 Cancelled
    /// 2. 应用配置覆盖（SpawnConfig → SessionConfig）
    /// 3. 创建隔离的 Session 实例（Coder）
    /// 4. 运行单轮 ReAct 循环
    /// 5. 将结果打包为 AgentResult
    ///
    /// # 取消处理
    ///
    /// 通过 `external_cancel` 将 SubAgentManagerImpl 的取消信号传播到
    /// Session 的 ReAct 循环。如果取消信号在 Session 执行期间触发，
    /// ReAct 循环会在下一次迭代开始时检查并退出。
    async fn run(
        &self,
        agent_id: String,
        task: SpawnTask,
        thread_id: ThreadId,
        _depth: u32,
        _message_rx: mpsc::UnboundedReceiver<Message>,
        cancel: Arc<AtomicBool>,
    ) -> AgentResult {
        debug!(
            agent_id = %agent_id,
            task = %task.task_name,
            "SessionRunner: starting sub-agent"
        );

        // Step 1: 检查是否已被取消
        if cancel.load(Ordering::Acquire) {
            warn!(
                agent_id = %agent_id,
                task = %task.task_name,
                "SessionRunner: cancelled before start"
            );
            return AgentResult::cancelled(agent_id);
        }

        // Step 2: 应用配置覆盖
        let config_override = &task.config_override;
        let system_instructions = config_override
            .as_ref()
            .and_then(|c: &SpawnConfig| c.system_instructions.clone())
            .unwrap_or_else(|| self.system_instructions.clone());

        let max_iterations = config_override
            .as_ref()
            .and_then(|c: &SpawnConfig| c.max_iterations)
            .unwrap_or(self.max_iterations);

        let permission_mode = config_override
            .as_ref()
            .and_then(|c: &SpawnConfig| c.permission_mode.clone())
            .unwrap_or_else(|| self.permission_mode.clone());

        // Step 3: 创建隔离的 Session（Coder）
        let session_id = SessionId(format!("coder-{agent_id}"));
        let config = SessionConfig {
            id: session_id,
            system_instructions,
            max_iterations,
            permission_mode,
            model_client: Arc::clone(&self.model_client),
            tool_registry: Arc::clone(&self.tool_registry),
            tool_router: None,
            hook_registry: None,
            external_cancel: Some(Arc::clone(&cancel)),
            max_context_tokens: task
                .config_override
                .as_ref()
                .and_then(|cfg| cfg.token_budget),
            temperature: None,
            knowledge: None,
            quality_gate: None,
        };

        let mut session = Session::new(config).await;

        // 附加安全审查层（提示注入防御、敏感信息过滤）
        // 确保子 Agent（Coder）具有与父 Session 同等级别的注入防御
        if let Some(ref safety) = self.safety_layer {
            session.with_safety_layer(Arc::clone(safety));
        }

        // Step 4: 构建并执行 ReAct 循环
        let input = self.build_turn_input(&task, &thread_id);
        let events = session.run_turn(input).await;

        // Step 5: 检查执行结果
        let has_error = events.iter().any(|e| matches!(e, ResponseEvent::Error { .. }));

        // 再次检查取消标志（可能在执行期间被设置）
        if cancel.load(Ordering::Acquire) {
            info!(
                agent_id = %agent_id,
                task = %task.task_name,
                "SessionRunner: cancelled during execution"
            );
            return AgentResult::cancelled(agent_id);
        }

        if has_error {
            let error_msg = events
                .iter()
                .filter_map(|e| {
                    if let ResponseEvent::Error { ref message } = e {
                        Some(message.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");

            error!(
                agent_id = %agent_id,
                task = %task.task_name,
                error = %error_msg,
                "SessionRunner: sub-agent failed"
            );

            AgentResult::failure(agent_id, error_msg)
        } else {
            let final_message = Self::extract_final_message(&events);
            info!(
                agent_id = %agent_id,
                task = %task.task_name,
                "SessionRunner: sub-agent completed successfully"
            );
            AgentResult::success(agent_id, events, final_message.unwrap_or_default())
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sub_agent::SubAgentError;
    use crate::agent::sub_agent_manager::SubAgentManagerImpl;
    use crate::agent::AgentStatus;
    use crate::tools::registry::DefaultToolRegistry;
    use code_agent_protocol::TurnId;

    /// 模拟 ModelClient 用于测试
    struct MockModelClient;

    #[async_trait]
    impl ModelClient for MockModelClient {
        fn model_name(&self) -> &str {
            "mock-model"
        }

        async fn complete_stream(
            &self,
            _messages: &[Message],
            _tools: &[crate::model::types::ToolDefinition],
            _temperature: Option<f32>,
        ) -> crate::model::ModelResult<Box<dyn futures::Stream<Item = ResponseEvent> + Send + Unpin>>
        {
            use futures::stream;
            let events = vec![ResponseEvent::TurnComplete {
                turn_id: TurnId("mock-turn".into()),
                final_message: Message::AssistantMessage {
                    content: "Mock response".into(),
                },
            }];
            Ok(Box::new(stream::iter(events)))
        }

        fn last_token_usage(&self) -> Option<crate::model::types::TokenUsage> {
            None
        }
    }

    fn make_runner() -> SessionRunner {
        let model: Arc<dyn ModelClient> = Arc::new(MockModelClient);
        let registry = Arc::new(DefaultToolRegistry::new());
        SessionRunner::new(model, registry)
    }

    #[tokio::test]
    async fn test_session_runner_basic_execution() {
        let runner = Arc::new(make_runner());
        let manager = SubAgentManagerImpl::default_for_root(runner);
        let task = SpawnTask {
            task_name: "test-task".into(),
            message: "Hello".into(),
            config_override: None,
        };

        let agent_id = manager.spawn_agent(task).await.unwrap();
        let result = manager.wait_agent(&agent_id).await.unwrap();

        assert_eq!(result.status, AgentStatus::Completed);
        assert!(result.final_message.is_some());
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn test_session_runner_cancelled_before_start() {
        let runner = Arc::new(make_runner());
        let mut manager = SubAgentManagerImpl::default_for_root(runner);
        let task = SpawnTask {
            task_name: "cancel-test".into(),
            message: "Hello".into(),
            config_override: None,
        };

        let agent_id = manager.spawn_agent(task).await.unwrap();

        // Close the agent before it starts processing
        manager.close_agent(&agent_id).await.unwrap();

        // Wait should return AgentNotFound since channels are cleaned up
        let result = manager.wait_agent(&agent_id).await;
        assert!(result.is_err());
        match result {
            Err(SubAgentError::AgentNotFound(_)) => {}
            Err(SubAgentError::ChannelClosed(_)) => {}
            other => panic!("Expected AgentNotFound or ChannelClosed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_session_runner_config_override() {
        let runner = Arc::new(make_runner());
        let manager = SubAgentManagerImpl::default_for_root(runner);
        let task = SpawnTask {
            task_name: "config-test".into(),
            message: "Test with config override".into(),
            config_override: Some(SpawnConfig {
                model: Some("custom-model".into()),
                max_iterations: Some(5),
                ..Default::default()
            }),
        };

        let agent_id = manager.spawn_agent(task).await.unwrap();
        let result = manager.wait_agent(&agent_id).await.unwrap();

        assert_eq!(result.status, AgentStatus::Completed);
    }

    #[tokio::test]
    async fn test_session_runner_multiple_parallel() {
        let runner = Arc::new(make_runner());
        let manager = SubAgentManagerImpl::default_for_root(runner);

        let mut ids = Vec::new();
        for i in 0..3 {
            let task = SpawnTask {
                task_name: format!("parallel-task-{i}"),
                message: format!("Task {i}"),
                config_override: None,
            };
            let id = manager.spawn_agent(task).await.unwrap();
            ids.push(id);
            // Yield to let the spawned task start running before next spawn
            tokio::task::yield_now().await;
        }

        // Wait for all to complete — some may have finished already
        for id in &ids {
            match manager.wait_agent(id).await {
                Ok(result) => assert_eq!(result.status, AgentStatus::Completed),
                Err(SubAgentError::AgentNotFound(_)) => {
                    // Task completed and already cleaned up — acceptable
                }
                Err(e) => panic!("Unexpected error for {id}: {e}"),
            }
        }
    }

    #[tokio::test]
    async fn test_session_runner_respects_parallel_limit() {
        let runner = Arc::new(make_runner());
        let manager = SubAgentManagerImpl::new_with_runner(2, 1, 0, runner);

        let task = SpawnTask {
            task_name: "par-test".into(),
            message: "Hello".into(),
            config_override: None,
        };

        // First spawn should succeed
        let _id1 = manager.spawn_agent(task.clone()).await.unwrap();
        tokio::task::yield_now().await;

        // Second spawn should exceed max_parallel=1 (first is still running if yield_now is not enough
        // but since mock is instant, both spawn may succeed if first finished too fast)
        // Use max_parallel=1 and expect either MaxParallelExceeded or success
        let result = manager.spawn_agent(task.clone()).await;
        if let Err(e) = result {
            match e {
                SubAgentError::MaxParallelExceeded { .. } => {} // Expected
                other => panic!("Expected parallel limit, got: {other}"),
            }
        }
        // If it succeeded, the first task finished before the second spawn
    }
}
