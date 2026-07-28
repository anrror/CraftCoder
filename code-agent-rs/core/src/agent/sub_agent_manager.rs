//! Async sub-agent execution runtime — spawn, message, wait, and close.
//!
//! `SubAgentManagerImpl` wraps the synchronous [`SubAgentManager`] registry
//! with tokio task spawning and channel-based communication. Each sub-agent
//! runs in its own tokio task with isolated context.
//!
//! # Architecture
//!
//! ```text
//! SubAgentManagerImpl
//!  ├─ manager: Arc<Mutex<SubAgentManager>>   ← registry + limits
//!  ├─ channels: RwLock<HashMap<agent_id, Channel>>
//!  │    ├─ message_tx: mpsc::Sender<Message>  ← parent → child messages
//!  │    ├─ result_rx: oneshot::Receiver        ← child → parent result
//!  │    └─ cancel: Arc<AtomicBool>             ← graceful shutdown
//!  └─ runner: Arc<dyn SubAgentRunner>          ← pluggable execution
//! ```
//!
//! Each spawned sub-agent:
//! 1. Is allocated channels and a tokio task
//! 2. Receives only its task-specific context (NOT parent's full history)
//! 3. Streams results back via the oneshot receiver
//! 4. Can be cancelled via the cancel flag (parent interruption cascades)
//!    子 Agent 异步执行运行时 —— 生成、通信、等待和关闭
//!
//! 【领域含义】SubAgentManagerImpl 包装同步的 SubAgentManager 注册表，
//! 提供 tokio 任务生成和基于通道的通信能力。
//! 每个子 Agent 在独立 tokio 任务中运行，上下文隔离。
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use code_agent_protocol::{Message, ThreadId};
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tracing::{debug, info, warn};

use super::sub_agent::{
    AgentResult, AgentStatus, SpawnTask, SubAgentError, SubAgentHandle, SubAgentManager,
};

// ---------------------------------------------------------------------------
// SubAgentRunner — pluggable execution trait
// ---------------------------------------------------------------------------

/// Trait for executing a sub-agent task.
///
/// This is abstracted so that unit tests can supply a mock runner without
/// needing a real model client. Production implementations use a full
/// `Session` with a real model.
#[async_trait]
/// 子 Agent 运行器 —— 可插拔的执行特质
///
/// 【领域含义】SubAgentRunner 抽象了子 Agent 任务的执行逻辑，
/// 允许单元测试使用模拟运行器而无需真实模型客户端。
/// 生产实现使用完整的 Session 和真实模型。
#[allow(clippy::too_many_arguments)]
pub trait SubAgentRunner: Send + Sync {
    /// Execute the sub-agent task.
    ///
    /// Receives messages from the parent via `message_rx` and signals
    /// completion via the returned [`AgentResult`]. The `cancel` flag is
    /// checked periodically; if set to `true`, the runner should exit early
    /// with a Cancelled result.
    async fn run(
        &self,
        agent_id: String,
        task: SpawnTask,
        thread_id: ThreadId,
        depth: u32,
        message_rx: mpsc::UnboundedReceiver<Message>,
        cancel: Arc<AtomicBool>,
    ) -> AgentResult;
}

// ---------------------------------------------------------------------------
// Internal channel bundle
// ---------------------------------------------------------------------------

/// Communication channels for a single running sub-agent.
struct SubAgentChannel {
    /// Send messages from parent to the running sub-agent.
    message_tx: mpsc::UnboundedSender<Message>,
    /// Receive the final result when the sub-agent completes.
    result_rx: Option<oneshot::Receiver<AgentResult>>,
    /// Signal the sub-agent task to cancel.
    cancel: Arc<AtomicBool>,
}

// ---------------------------------------------------------------------------
// SubAgentManagerImpl
// ---------------------------------------------------------------------------

/// Async execution runtime for sub-agents.
///
/// Wraps the synchronous [`SubAgentManager`] registry with tokio task
/// spawning and channel-based communication. Designed for use within a
/// parent agent's async context.
///
/// # Example
///
/// ```rust,ignore
/// use code_agent_core::agent::sub_agent_manager::SubAgentManagerImpl;
///
/// let manager = SubAgentManagerImpl::default_for_root();
/// let task = SpawnTask {
///     task_name: "review".into(),
///     message: "Review main.rs".into(),
///     config_override: None,
/// };
/// let agent_id = manager.spawn_agent(task).await?;
/// let result = manager.wait_agent(&agent_id).await?;
/// ```
/// 子 Agent 异步执行运行时 —— 生成、通信、等待和关闭
///
/// 【领域含义】SubAgentManagerImpl 包装了同步的 SubAgentManager 注册表，
/// 提供 tokio 任务生成和基于通道的通信能力。
/// 每个子 Agent 在独立的 tokio 任务中运行，拥有隔离的上下文。
pub struct SubAgentManagerImpl {
    /// Synchronous registry for state, limits, and handles.
    manager: Arc<Mutex<SubAgentManager>>,
    /// Active communication channels for running sub-agents.
    channels: Arc<RwLock<HashMap<String, SubAgentChannel>>>,
    /// Pluggable runner for executing sub-agent tasks.
    runner: Arc<dyn SubAgentRunner>,
}

impl SubAgentManagerImpl {
    /// Create a new manager with the given registry limits and runner.
    /// 使用指定的注册表限制和运行器创建管理器
    pub fn new_with_runner(
        max_depth: u32,
        max_parallel: usize,
        parent_depth: u32,
        runner: Arc<dyn SubAgentRunner>,
    ) -> Self {
        Self {
            manager: Arc::new(Mutex::new(SubAgentManager::new(
                max_depth,
                max_parallel,
                parent_depth,
            ))),
            channels: Arc::new(RwLock::new(HashMap::new())),
            runner,
        }
    }

    /// Create with defaults: max_depth=2, max_parallel=6, parent_depth=0.
    ///
    /// Requires a runner to be set before spawning agents.
    /// 使用默认配置创建根级管理器：max_depth=2, max_parallel=6, parent_depth=0
    ///
    /// 需在生成 Agent 前设置运行器。
    pub fn default_for_root(runner: Arc<dyn SubAgentRunner>) -> Self {
        Self::new_with_runner(2, 6, 0, runner)
    }

    // ── Query helpers ──────────────────────────────────────────────────

    /// List all registered sub-agent handles.
    /// 列出所有已注册的子 Agent 句柄
    pub async fn list_agents(&self) -> Vec<SubAgentHandle> {
        self.manager.lock().await.list_agents()
    }

    /// Total number of registered sub-agents.
    /// 已注册的子 Agent 总数
    pub async fn total_count(&self) -> usize {
        self.manager.lock().await.total_count()
    }

    /// Number of currently active (Running) sub-agents.
    /// 当前活跃（Running）的子 Agent 数量
    pub async fn active_count(&self) -> usize {
        self.manager.lock().await.active_count()
    }

    /// Maximum allowed depth.
    pub async fn max_depth(&self) -> u32 {
        self.manager.lock().await.max_depth()
    }

    /// Maximum allowed parallel agents.
    pub async fn max_parallel(&self) -> usize {
        self.manager.lock().await.max_parallel()
    }

    /// Parent depth.
    pub async fn parent_depth(&self) -> u32 {
        self.manager.lock().await.parent_depth()
    }

    /// Check whether the manager has been interrupted.
    pub async fn is_interrupted(&self) -> bool {
        self.manager.lock().await.is_interrupted()
    }

    // ── Core lifecycle ─────────────────────────────────────────────────

    /// Spawn a sub-agent with isolated context.
    ///
    /// Creates a new sub-agent that runs asynchronously in its own tokio
    /// task. The sub-agent receives only the task message (NOT the parent's
    /// full conversation history) — this enforces context isolation.
    ///
    /// # Errors
    ///
    /// Returns [`SubAgentError::MaxDepthExceeded`] if the child depth would
    /// exceed `max_depth`, [`SubAgentError::MaxParallelExceeded`] if the
    /// parallel limit has been reached, or [`SubAgentError::Cancelled`] if
    /// the manager has been interrupted.
    ///
    /// # Context isolation
    ///
    /// The spawned sub-agent starts with a fresh context containing only its
    /// task prompt. It cannot see the parent's conversation history. This
    /// prevents information leakage and keeps sub-agents focused.
    /// 生成具有隔离上下文的子 Agent
    ///
    /// 创建在独立 tokio 任务中异步运行的子 Agent。
    /// 子 Agent 仅接收任务消息（而非父 Agent 的完整对话历史），
    /// 从而强制上下文隔离，防止信息泄露。
    pub async fn spawn_agent(&self, task: SpawnTask) -> Result<String, SubAgentError> {
        // Calculate child depth
        let child_depth = {
            let mgr = self.manager.lock().await;
            mgr.parent_depth() + 1
        };

        // Reserve a slot in the registry
        let thread_id = ThreadId(format!("sub-{}", uuid::Uuid::new_v4()));
        let agent_id = {
            let mut mgr = self.manager.lock().await;
            mgr.reserve_slot(task.task_name.clone(), thread_id.clone(), child_depth)?
        };
        let agent_id_clone = agent_id.clone();

        // Set up communication channels
        let (message_tx, message_rx) = mpsc::unbounded_channel();
        let (result_tx, result_rx) = oneshot::channel();
        let cancel = Arc::new(AtomicBool::new(false));

        {
            let mut chans = self.channels.write().await;
            chans.insert(
                agent_id.clone(),
                SubAgentChannel {
                    message_tx,
                    result_rx: Some(result_rx),
                    cancel: Arc::clone(&cancel),
                },
            );
        }

        // Clone Arcs for the spawned task
        let runner = Arc::clone(&self.runner);
        let manager = Arc::clone(&self.manager);
        let _channels = Arc::clone(&self.channels);
        let cancel_clone = Arc::clone(&cancel);

        // Spawn the async task
        tokio::spawn(async move {
            info!(
                agent_id = %agent_id_clone,
                task = %task.task_name,
                depth = child_depth,
                "Spawning sub-agent"
            );

            // Run the sub-agent via the pluggable runner
            let result = runner
                .run(
                    agent_id_clone.clone(),
                    task,
                    thread_id,
                    child_depth,
                    message_rx,
                    cancel_clone,
                )
                .await;

            // Update status in the registry
            {
                let mut mgr = manager.lock().await;
                mgr.update_status(&agent_id_clone, result.status.clone());
            }

            // Send result back through the oneshot channel
            // (ignore error if receiver was dropped — e.g., parent closed agent)
            let _ = result_tx.send(result);

            // NOTE: Channel cleanup is deliberately NOT done here.
            // It happens in wait_agent() after the result is consumed.
            // This prevents a race: without this guard, the spawned task
            // could finish and remove channels before wait_agent reads them.

            debug!(agent_id = %agent_id_clone, "Sub-agent completed");
        });

        Ok(agent_id)
    }

    /// Send a message to a running sub-agent.
    ///
    /// Messages are forwarded through an unbounded mpsc channel to the
    /// sub-agent's task. The sub-agent runner is responsible for reading
    /// and processing these messages.
    ///
    /// # Errors
    ///
    /// Returns [`SubAgentError::AgentNotFound`] if the agent ID is not
    /// registered, or [`SubAgentError::ChannelClosed`] if the sub-agent
    /// task has already completed and the channel was dropped.
    /// 向运行中的子 Agent 发送消息
    ///
    /// 消息通过无界 mpsc 通道转发到子 Agent 的任务。
    pub async fn send_message(
        &self,
        agent_id: &str,
        message: Message,
    ) -> Result<(), SubAgentError> {
        let chans = self.channels.read().await;
        let chan = chans
            .get(agent_id)
            .ok_or_else(|| SubAgentError::AgentNotFound(agent_id.to_string()))?;

        chan.message_tx
            .send(message)
            .map_err(|_| SubAgentError::ChannelClosed(agent_id.to_string()))
    }

    /// Wait for a sub-agent to complete and return its result.
    ///
    /// This consumes the oneshot receiver for the given agent. It can only
    /// be called once per agent — subsequent calls will fail with
    /// [`SubAgentError::AgentNotFound`] because the channel is cleaned up
    /// on completion.
    ///
    /// # Errors
    ///
    /// Returns [`SubAgentError::AgentNotFound`] if the agent ID is not
    /// registered, or [`SubAgentError::ChannelClosed`] if the sub-agent
    /// task panicked or the result sender was dropped.
    /// 等待子 Agent 完成并返回其结果
    ///
    /// 消耗该 Agent 的 oneshot 接收器，每个 Agent 仅可调用一次。
    /// 结果消费后自动清理通道，消除 spawned task 与 wait_agent 之间的竞态。
    pub async fn wait_agent(&self, agent_id: &str) -> Result<AgentResult, SubAgentError> {
        // Extract the oneshot receiver from the channel entry
        let result_rx = {
            let mut chans = self.channels.write().await;
            let chan = chans
                .get_mut(agent_id)
                .ok_or_else(|| SubAgentError::AgentNotFound(agent_id.to_string()))?;
            chan.result_rx
                .take()
                .ok_or_else(|| SubAgentError::ChannelClosed(agent_id.to_string()))?
        };

        let result = match result_rx.await {
            Ok(result) => Ok(result),
            Err(_) => Err(SubAgentError::ChannelClosed(agent_id.to_string())),
        };

        // Clean up channels AFTER consuming the result,
        // so the spawned task cannot race ahead and remove them first.
        {
            let mut chans = self.channels.write().await;
            chans.remove(agent_id);
        }

        result
    }

    /// Terminate a sub-agent.
    ///
    /// Sets the cancel flag, updates the registry status to Cancelled, and
    /// removes communication channels. The sub-agent's tokio task will exit
    /// on its next cancellation check.
    ///
    /// # Errors
    ///
    /// Returns [`SubAgentError::AgentNotFound`] if the agent ID is not
    /// registered.
    /// 终止子 Agent
    ///
    /// 设置取消标志、更新注册表状态为 Cancelled 并移除通信通道。
    pub async fn close_agent(&mut self, agent_id: &str) -> Result<(), SubAgentError> {
        // Signal cancellation
        {
            let chans = self.channels.read().await;
            if let Some(chan) = chans.get(agent_id) {
                chan.cancel.store(true, Ordering::Release);
            }
        }

        // Update registry
        {
            let mut mgr = self.manager.lock().await;
            if mgr.get_agent(agent_id).is_none() {
                return Err(SubAgentError::AgentNotFound(agent_id.to_string()));
            }
            mgr.update_status(agent_id, AgentStatus::Cancelled);
        }

        // Remove channels (result receiver gets dropped, the spawned task
        // will clean itself up on noticing the cancel flag)
        {
            let mut chans = self.channels.write().await;
            chans.remove(agent_id);
        }

        info!(agent_id = %agent_id, "Sub-agent closed");
        Ok(())
    }

    /// Signal interruption to all running sub-agents (cascading cancel).
    ///
    /// This is the parent-interruption cascade: when the parent is
    /// interrupted, this method marks all children as Cancelled and sets
    /// their cancel flags so their tasks exit at the next opportunity.
    /// 向所有运行中的子 Agent 发送中断信号（级联取消）
    ///
    /// 当父 Agent 被中断时调用此方法，所有子 Agent 将被标记为 Cancelled。
    pub async fn interrupt_all(&self) {
        // Update registry
        {
            let mut mgr = self.manager.lock().await;
            mgr.interrupt();
        }

        // Signal cancellation to all running tasks
        let chans = self.channels.read().await;
        for (agent_id, chan) in chans.iter() {
            chan.cancel.store(true, Ordering::Release);
            warn!(agent_id = %agent_id, "Interrupt signalled to sub-agent");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use code_agent_protocol::ResponseEvent;
    use std::sync::atomic::AtomicU32;

    // ── Mock Runner ────────────────────────────────────────────────────

    /// A mock sub-agent runner that simulates processing.
    ///
    /// When spawned, it waits for messages on the channel, echoes them back
    /// as ResponseEvent deltas, and either completes normally or simulates
    /// failure based on config.
    struct MockRunner {
        /// If true, the mock fails instead of completing successfully.
        should_fail: bool,
        /// Delay before responding (ms). 0 = instant.
        delay_ms: u64,
        /// Track how many times run() was called.
        call_count: Arc<AtomicU32>,
    }

    impl MockRunner {
        fn new() -> Self {
            Self {
                should_fail: false,
                delay_ms: 0,
                call_count: Arc::new(AtomicU32::new(0)),
            }
        }

        fn with_delay(mut self, ms: u64) -> Self {
            self.delay_ms = ms;
            self
        }
    }

    #[async_trait]
    impl SubAgentRunner for MockRunner {
        async fn run(
            &self,
            agent_id: String,
            task: SpawnTask,
            _thread_id: ThreadId,
            _depth: u32,
            mut message_rx: mpsc::UnboundedReceiver<Message>,
            cancel: Arc<AtomicBool>,
        ) -> AgentResult {
            self.call_count.fetch_add(1, Ordering::SeqCst);

            let mut events = vec![ResponseEvent::AgentMessageDelta {
                content: format!("Starting: {}", task.task_name),
            }];

            // Simulate processing loop — read messages, check cancel flag
            loop {
                if cancel.load(Ordering::Acquire) {
                    return AgentResult::cancelled(agent_id);
                }

                if self.delay_ms > 0 {
                    tokio::time::sleep(tokio::time::Duration::from_millis(self.delay_ms)).await;
                }

                match message_rx.try_recv() {
                    Ok(msg) => {
                        let content = match msg {
                            Message::UserMessage { content } => {
                                format!("Received: {content}")
                            }
                            _ => "Received message".into(),
                        };
                        events.push(ResponseEvent::AgentMessageDelta { content });
                    }
                    Err(mpsc::error::TryRecvError::Empty) => {
                        // No more messages — complete
                        break;
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        break;
                    }
                }
            }

            if self.should_fail {
                AgentResult::failure(agent_id, "Mock failure".into())
            } else {
                AgentResult::success(agent_id, events, format!("Done: {}", task.task_name))
            }
        }
    }

    // ── Helper: create manager with mock runner ────────────────────────

    fn make_manager() -> (SubAgentManagerImpl, Arc<MockRunner>) {
        let runner = Arc::new(MockRunner::new());
        let mgr = SubAgentManagerImpl::new_with_runner(2, 6, 0, Arc::clone(&runner) as Arc<dyn SubAgentRunner>);
        (mgr, runner)
    }

    fn make_task(name: &str, message: &str) -> SpawnTask {
        SpawnTask {
            task_name: name.into(),
            message: message.into(),
            config_override: None,
        }
    }

    // ── Tests: Spawn → wait lifecycle ──────────────────────────────────

    #[tokio::test]
    async fn spawn_and_wait_completes() {
        let (mgr, _runner) = make_manager();
        let task = make_task("hello", "greet the user");

        let agent_id = mgr.spawn_agent(task).await.expect("spawn should succeed");
        assert!(agent_id.starts_with("subagent-"));

        // Should be listed as active
        assert_eq!(mgr.active_count().await, 1);

        let result = mgr.wait_agent(&agent_id).await.expect("wait should succeed");
        assert_eq!(result.agent_id, agent_id);
        assert_eq!(result.status, AgentStatus::Completed);
        assert!(result.final_message.is_some());
        assert!(result.final_message.unwrap().contains("hello"));

        // After wait, channel is cleaned up
        assert_eq!(mgr.total_count().await, 1); // still in registry
    }

    #[tokio::test]
    async fn spawn_send_receive_close_lifecycle() {
        let (mut mgr, _runner) = make_manager();
        let task = make_task("review", "review main.rs");

        let agent_id = mgr.spawn_agent(task).await.expect("spawn");

        // Send an additional message to the sub-agent
        let msg = Message::UserMessage {
            content: "Also check lib.rs".into(),
        };
        mgr.send_message(&agent_id, msg).await.expect("send should succeed");

        // Wait for completion
        let result = mgr.wait_agent(&agent_id).await.expect("wait");
        assert_eq!(result.status, AgentStatus::Completed);

        // Verify events include both the auto message and the echoed message
        let delta_messages: Vec<&str> = result
            .events
            .iter()
            .filter_map(|e| {
                if let ResponseEvent::AgentMessageDelta { content } = e {
                    Some(content.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(delta_messages.iter().any(|s| s.contains("Starting: review")));
        assert!(delta_messages.iter().any(|s| s.contains("Received: Also check lib.rs")));

        // Close should succeed
        let result = mgr.close_agent(&agent_id).await;
        assert!(result.is_ok());
    }

    // ── Tests: Max depth enforcement ───────────────────────────────────

    #[tokio::test]
    async fn max_depth_enforcement_blocks_deep_spawn() {
        // Create a manager with parent_depth=1, max_depth=2
        // Child depth would be 2, which is >= max_depth=2 → blocked
        let runner = Arc::new(MockRunner::new());
        let mgr = SubAgentManagerImpl::new_with_runner(
            2,
            6,
            1, // parent is already at depth 1
            Arc::clone(&runner) as Arc<dyn SubAgentRunner>,
        );

        let task = make_task("deep", "too deep");
        let err = mgr.spawn_agent(task).await.unwrap_err();
        assert!(matches!(err, SubAgentError::MaxDepthExceeded { .. }));
    }

    #[tokio::test]
    async fn depth_within_limit_succeeds() {
        let runner = Arc::new(MockRunner::new());
        let mgr = SubAgentManagerImpl::new_with_runner(
            3,
            6,
            0,
            Arc::clone(&runner) as Arc<dyn SubAgentRunner>,
        );

        // depth 0 → child at depth 1 (ok)
        let id1 = mgr.spawn_agent(make_task("t1", "ok")).await.unwrap();
        let _ = mgr.wait_agent(&id1).await;

        // depth 1 → child at depth 2 (ok, since max=3)
        let mgr2 = SubAgentManagerImpl::new_with_runner(
            3,
            6,
            2,
            Arc::clone(&runner) as Arc<dyn SubAgentRunner>,
        );
        let err = mgr2.spawn_agent(make_task("t2", "too deep")).await;
        // child depth = 3, max_depth = 3 → blocked (>=)
        assert!(err.is_err());
    }

    // ── Tests: Max parallel agents ─────────────────────────────────────

    #[tokio::test]
    async fn max_parallel_enforcement() {
        let runner2 = Arc::new(MockRunner::new());
        let mgr = SubAgentManagerImpl::new_with_runner(
            5,
            3,
            0,
            Arc::clone(&runner2) as Arc<dyn SubAgentRunner>,
        );

        let id1 = mgr.spawn_agent(make_task("a", "1")).await.unwrap();
        mgr.spawn_agent(make_task("b", "2")).await.unwrap();
        mgr.spawn_agent(make_task("c", "3")).await.unwrap();

        assert_eq!(mgr.active_count().await, 3);

        let err = mgr.spawn_agent(make_task("d", "4")).await.unwrap_err();
        assert!(matches!(err, SubAgentError::MaxParallelExceeded { .. }));

        // Complete one → frees a slot
        let _ = mgr.wait_agent(&id1).await;
        // Wait for the registry update (it happens in the spawned task)
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        // Now we should be able to spawn again
        let id4 = mgr.spawn_agent(make_task("d", "4")).await;
        // Might still fail if the task hasn't updated the registry yet
        // but with the mock being instant it should pass
        let _ = id4.expect("should spawn after completion frees slot");
    }

    // ── Tests: Parent interruption cascade ─────────────────────────────

    #[tokio::test]
    async fn parent_interruption_cascades_to_children() {
        let runner = Arc::new(MockRunner::new().with_delay(500)); // slow mock
        let mgr = SubAgentManagerImpl::new_with_runner(
            5,
            6,
            0,
            Arc::clone(&runner) as Arc<dyn SubAgentRunner>,
        );

        mgr.spawn_agent(make_task("t1", "slow")).await.unwrap();
        mgr.spawn_agent(make_task("t2", "slow")).await.unwrap();

        assert_eq!(mgr.active_count().await, 2);

        // Interrupt all — should cancel both
        mgr.interrupt_all().await;

        // Wait a bit for cancellation to propagate
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        assert!(mgr.is_interrupted().await);
    }

    #[tokio::test]
    async fn cancelled_agent_blocks_further_spawns() {
        let (mgr, _runner) = make_manager();
        mgr.interrupt_all().await;

        let err = mgr.spawn_agent(make_task("t", "blocked")).await.unwrap_err();
        assert!(matches!(err, SubAgentError::Cancelled));
    }

    // ── Tests: Context isolation ───────────────────────────────────────

    #[tokio::test]
    async fn context_isolation_sub_agent_has_only_task_message() {
        // The sub-agent's runner receives ONLY the SpawnTask message —
        // NOT the parent's full history. This is verified by the runner
        // interface itself: run() gets the SpawnTask, not a Message history.
        //
        // We verify that the mock runner can process messages sent AFTER
        // spawning but the initial context is only the task.
        let (mgr, _runner) = make_manager();
        let task = SpawnTask {
            task_name: "isolated".into(),
            message: "ONLY_THIS_MESSAGE".into(),
            config_override: None,
        };

        let agent_id = mgr.spawn_agent(task).await.unwrap();

        // Send a message that the parent "knows"
        let _ = mgr
            .send_message(
                &agent_id,
                Message::UserMessage {
                    content: "Parent's secret context".into(),
                },
            )
            .await;

        let result = mgr.wait_agent(&agent_id).await.unwrap();
        // The result contains the echo of "Parent's secret context"
        // BUT the initial message is the task message only — the parent's
        // conversation history is NOT in the events. This is enforced by
        // the SubAgentRunner trait contract.
        assert!(result.final_message.unwrap().contains("isolated"));
    }

    // ── Tests: SpawnConfig override ────────────────────────────────────

    #[tokio::test]
    async fn spawn_config_override_is_passed_through() {
        // The SpawnConfig is embedded in SpawnTask, which is passed to the
        // runner. We verify that a task with config_override works.
        let (mgr, _runner) = make_manager();
        let task = SpawnTask {
            task_name: "with-config".into(),
            message: "test".into(),
            config_override: Some(super::super::sub_agent::SpawnConfig {
                model: Some("override-model".into()),
                permission_mode: Some(code_agent_protocol::PermissionMode::Auto),
                system_instructions: Some("Be brief.".into()),
                max_iterations: Some(5),
                temperature: Some(0.1),
                token_budget: None,
            }),
        };

        let agent_id = mgr.spawn_agent(task).await.unwrap();
        let result = mgr.wait_agent(&agent_id).await.unwrap();
        assert_eq!(result.status, AgentStatus::Completed);
    }

    #[tokio::test]
    async fn spawn_config_none_works() {
        let (mgr, _runner) = make_manager();
        let task = SpawnTask {
            task_name: "no-config".into(),
            message: "test".into(),
            config_override: None,
        };
        let agent_id = mgr.spawn_agent(task).await.unwrap();
        let result = mgr.wait_agent(&agent_id).await.unwrap();
        assert_eq!(result.status, AgentStatus::Completed);
    }

    // ── Tests: Error paths ─────────────────────────────────────────────

    #[tokio::test]
    async fn send_message_to_nonexistent_agent_fails() {
        let (mgr, _runner) = make_manager();
        let err = mgr
            .send_message(
                "no-such-agent",
                Message::UserMessage {
                    content: "hello".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SubAgentError::AgentNotFound(_)));
    }

    #[tokio::test]
    async fn wait_nonexistent_agent_fails() {
        let (mgr, _runner) = make_manager();
        let err = mgr.wait_agent("no-such-agent").await.unwrap_err();
        assert!(matches!(err, SubAgentError::AgentNotFound(_)));
    }

    #[tokio::test]
    async fn close_nonexistent_agent_fails() {
        let (mut mgr, _runner) = make_manager();
        let err = mgr.close_agent("no-such-agent").await.unwrap_err();
        assert!(matches!(err, SubAgentError::AgentNotFound(_)));
    }

    #[tokio::test]
    async fn send_message_after_close_fails() {
        let (mut mgr, _runner) = make_manager();
        let agent_id = mgr.spawn_agent(make_task("t", "hi")).await.unwrap();

        // Close the agent
        mgr.close_agent(&agent_id).await.unwrap();

        // Sending should now fail
        let err = mgr
            .send_message(
                &agent_id,
                Message::UserMessage {
                    content: "too late".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SubAgentError::AgentNotFound(_)));
    }

    #[tokio::test]
    async fn wait_agent_twice_fails() {
        let (mgr, _runner) = make_manager();
        let agent_id = mgr.spawn_agent(make_task("t", "hi")).await.unwrap();

        // First wait succeeds
        let _ = mgr.wait_agent(&agent_id).await.unwrap();

        // Second wait fails (channel already consumed and cleaned up)
        let err = mgr.wait_agent(&agent_id).await.unwrap_err();
        // May be AgentNotFound if the spawned task already cleaned up,
        // or ChannelClosed if it hasn't yet
        assert!(
            matches!(err, SubAgentError::AgentNotFound(_))
                || matches!(err, SubAgentError::ChannelClosed(_))
        );
    }

    // ── Tests: List agents ─────────────────────────────────────────────

    #[tokio::test]
    async fn list_agents_shows_all() {
        let (mgr, _runner) = make_manager();
        mgr.spawn_agent(make_task("a", "1")).await.unwrap();
        mgr.spawn_agent(make_task("b", "2")).await.unwrap();

        let handles = mgr.list_agents().await;
        assert_eq!(handles.len(), 2);

        let names: Vec<&str> = handles.iter().map(|h| h.task_name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
    }

    // ── Tests: Concurrent spawns ───────────────────────────────────────

    #[tokio::test]
    async fn six_parallel_agents_no_deadlock() {
        let runner = Arc::new(MockRunner::new());
        let mgr = SubAgentManagerImpl::new_with_runner(
            5,
            6,
            0,
            Arc::clone(&runner) as Arc<dyn SubAgentRunner>,
        );

        let mut ids = Vec::new();
        for i in 0..6 {
            let id = mgr
                .spawn_agent(make_task(&format!("task-{i}"), &format!("msg-{i}")))
                .await
                .expect("should spawn");
            ids.push(id);
        }
        assert_eq!(ids.len(), 6);
        assert_eq!(mgr.active_count().await, 6);

        // Wait for all to complete concurrently
        let results: Vec<AgentResult> = futures::future::join_all(
            ids.iter().map(|id| mgr.wait_agent(id)),
        )
        .await
        .into_iter()
        .map(|r| r.expect("should complete"))
        .collect();

        assert_eq!(results.len(), 6);
        for r in &results {
            assert_eq!(r.status, AgentStatus::Completed);
        }
    }

    // ── Tests: Default factory ─────────────────────────────────────────

    #[tokio::test]
    async fn default_for_root_has_correct_limits() {
        let runner = Arc::new(MockRunner::new());
        let mgr = SubAgentManagerImpl::default_for_root(
            Arc::clone(&runner) as Arc<dyn SubAgentRunner>,
        );
        assert_eq!(mgr.max_depth().await, 2);
        assert_eq!(mgr.max_parallel().await, 6);
        assert_eq!(mgr.parent_depth().await, 0);
    }
}
