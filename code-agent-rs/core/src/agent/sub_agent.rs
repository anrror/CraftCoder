//! 子智能体类型定义 —— 句柄、状态、任务和核心注册表
//!
//! 【领域含义】`SubAgentManager` 持有活跃子智能体的注册表，提供深度
//! 跟踪和并行限制执行。定义了子智能体的数据类型和同步管理操作；
//! 异步执行委托给 `super::sub_agent_manager::SubAgentManagerImpl`。
//!
//! The [`SubAgentManager`] holds the registry of active sub-agents with
//! depth tracking and parallel limit enforcement. It defines the data types
//! and synchronous management operations; async execution is delegated to
//! [`super::sub_agent_manager::SubAgentManagerImpl`].

use std::collections::HashMap;
use std::time::Instant;

use code_agent_protocol::{PermissionMode, ResponseEvent, ThreadId};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// AgentStatus
// ---------------------------------------------------------------------------

/// 子智能体的生命周期状态
///
/// 【领域含义】描述一个子智能体当前所处的执行阶段。
/// Running → Completed / Failed / Cancelled 是终态转换。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// 子智能体正在执行任务
    Running,
    /// 子智能体成功完成任务
    Completed,
    /// 子智能体遇到错误并失败
    Failed,
    /// 子智能体被父智能体或系统取消
    Cancelled,
}

// ---------------------------------------------------------------------------
// SubAgentHandle
// ---------------------------------------------------------------------------

/// 运行中子智能体的句柄，跟踪其身份和状态
#[derive(Clone, Debug)]
pub struct SubAgentHandle {
    /// 此子智能体的唯一标识符
    pub agent_id: String,
    /// 此子智能体正在处理的任务的可读名称
    pub task_name: String,
    /// 当前生命周期状态
    pub status: AgentStatus,
    /// 此子智能体运行的线程 ID（隔离上下文）
    pub thread_id: ThreadId,
    /// 在智能体树中的当前深度（0 = 根，1 = 第一级子智能体，以此类推）
    pub depth: u32,
    /// 此子智能体的创建时间
    pub created_at: Instant,
}

// ---------------------------------------------------------------------------
// SpawnConfig
// ---------------------------------------------------------------------------

/// 子智能体配置的可选覆盖项
///
/// 【领域含义】留为 `None` 的字段从父智能体的配置继承。
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SpawnConfig {
    /// 覆盖模型名称（例如为子任务使用较小的模型）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 覆盖权限模式
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    /// 覆盖系统指令
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instructions: Option<String>,
    /// 此子智能体的最大 ReAct 循环迭代次数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<usize>,
    /// 覆盖温度参数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Token 预算上限（用于 Coder 上下文控制，Phase C）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<usize>,
}

// ---------------------------------------------------------------------------
// SpawnTask
// ---------------------------------------------------------------------------

/// 生成新子智能体的输入
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpawnTask {
    /// 此子智能体任务的可读名称
    pub task_name: String,
    /// 发送给子智能体的初始提示词
    pub message: String,
    /// 可选的配置覆盖（模型、权限模式、工具等）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_override: Option<SpawnConfig>,
}

// ---------------------------------------------------------------------------
// AgentResult
// ---------------------------------------------------------------------------

/// 子智能体完成时返回的结果
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentResult {
    /// 完成的子智能体的 ID
    pub agent_id: String,
    /// 最终状态
    pub status: AgentStatus,
    /// 子智能体发出的事件的时间序列
    pub events: Vec<ResponseEvent>,
    /// 子智能体产生的最终消息内容
    pub final_message: Option<String>,
    /// 子智能体失败时的错误消息
    pub error: Option<String>,
}

impl AgentResult {
    /// Create a successful result.
    pub fn success(agent_id: String, events: Vec<ResponseEvent>, final_message: String) -> Self {
        Self {
            agent_id,
            status: AgentStatus::Completed,
            events,
            final_message: Some(final_message),
            error: None,
        }
    }

    /// Create a failed result.
    pub fn failure(agent_id: String, error: String) -> Self {
        Self {
            agent_id,
            status: AgentStatus::Failed,
            events: Vec::new(),
            final_message: None,
            error: Some(error),
        }
    }

    /// Create a cancelled result.
    pub fn cancelled(agent_id: String) -> Self {
        Self {
            agent_id,
            status: AgentStatus::Cancelled,
            events: Vec::new(),
            final_message: None,
            error: Some("Cancelled by parent".into()),
        }
    }
}

// ---------------------------------------------------------------------------
// SubAgentManager
// ---------------------------------------------------------------------------

/// 子智能体的注册表与生命周期管理器
///
/// 【领域含义】跟踪所有活跃子智能体，强制执行深度和并行限制，
/// 提供通信句柄。子智能体任务的异步执行由 `SubAgentManagerImpl` 编排。
///
/// # Concurrency
///
/// 此结构设计为在 `Arc<Mutex<>>` 后持有或在单个异步任务中使用。
/// `SubAgentManagerImpl` 包装器处理 tokio 任务生成和基于通道的通信。
pub struct SubAgentManager {
    /// 按智能体 ID 索引的活跃子智能体注册表
    agents: HashMap<String, SubAgentHandle>,
    /// 最大子智能体嵌套深度（默认 2）
    max_depth: u32,
    /// 最大并行子智能体数（默认 6）
    max_parallel: usize,
    /// 用于生成智能体 ID 的单调计数器
    counter: u64,
    /// 拥有此管理器的父智能体的深度
    parent_depth: u32,
    /// 中断标志 —— 设置后所有子智能体收到停止信号
    interrupted: bool,
}

impl SubAgentManager {
    /// 创建新的子智能体管理器
    ///
    /// # Arguments
    /// * `max_depth` — 最大子智能体嵌套深度（默认 2）
    /// * `max_parallel` — 最大并发子智能体数（默认 6）
    /// * `parent_depth` — 拥有此管理器的智能体的深度。
    ///   根智能体为 depth 0；子智能体每级增加 1。
    pub fn new(max_depth: u32, max_parallel: usize, parent_depth: u32) -> Self {
        Self {
            agents: HashMap::new(),
            max_depth,
            max_parallel,
            counter: 0,
            parent_depth,
            interrupted: false,
        }
    }

    /// 以默认值创建：max_depth=2, max_parallel=6, parent_depth=0
    pub fn default_for_root() -> Self {
        Self::new(2, 6, 0)
    }

    // ── Queries ────────────────────────────────────────────────────────

    /// 检查是否可以在给定深度下生成子智能体
    ///
    /// 如果在限制内返回 `Ok(())`，否则返回描述原因的错误。
    pub fn check_can_spawn(&self, requested_depth: u32) -> Result<(), SubAgentError> {
        if requested_depth >= self.max_depth {
            return Err(SubAgentError::MaxDepthExceeded {
                requested: requested_depth,
                max: self.max_depth,
            });
        }
        let active_count = self
            .agents
            .values()
            .filter(|h| h.status == AgentStatus::Running)
            .count();
        if active_count >= self.max_parallel {
            return Err(SubAgentError::MaxParallelExceeded {
                active: active_count,
                max: self.max_parallel,
            });
        }
        if self.interrupted {
            return Err(SubAgentError::Cancelled);
        }
        Ok(())
    }

    /// 生成唯一的智能体 ID 并预留一个槽位
    ///
    /// 检查深度 + 并行限制，然后创建一个新的处于 Running 状态的
    /// `SubAgentHandle` 并返回其 ID。
    pub fn reserve_slot(
        &mut self,
        task_name: String,
        thread_id: ThreadId,
        depth: u32,
    ) -> Result<String, SubAgentError> {
        self.check_can_spawn(depth)?;

        let agent_id = format!("subagent-{}", self.counter);
        self.counter += 1;

        let handle = SubAgentHandle {
            agent_id: agent_id.clone(),
            task_name,
            status: AgentStatus::Running,
            thread_id,
            depth,
            created_at: Instant::now(),
        };

        self.agents.insert(agent_id.clone(), handle);
        Ok(agent_id)
    }

    /// 更新子智能体的状态
    pub fn update_status(&mut self, agent_id: &str, status: AgentStatus) {
        if let Some(handle) = self.agents.get_mut(agent_id) {
            handle.status = status;
        }
    }

    /// 从注册表中移除子智能体，返回其句柄
    pub fn remove_agent(&mut self, agent_id: &str) -> Option<SubAgentHandle> {
        self.agents.remove(agent_id)
    }

    /// 获取子智能体句柄的引用
    pub fn get_agent(&self, agent_id: &str) -> Option<&SubAgentHandle> {
        self.agents.get(agent_id)
    }

    /// 列出所有已注册的子智能体句柄
    pub fn list_agents(&self) -> Vec<SubAgentHandle> {
        self.agents.values().cloned().collect()
    }

    /// 活跃（Running）子智能体的数量
    pub fn active_count(&self) -> usize {
        self.agents
            .values()
            .filter(|h| h.status == AgentStatus::Running)
            .count()
    }

    /// 已注册子智能体的总数（包括已完成/失败）
    pub fn total_count(&self) -> usize {
        self.agents.len()
    }

    /// 最大允许的嵌套深度
    pub fn max_depth(&self) -> u32 {
        self.max_depth
    }

    /// 最大允许的并行子智能体数
    pub fn max_parallel(&self) -> usize {
        self.max_parallel
    }

    /// 父智能体的深度
    pub fn parent_depth(&self) -> u32 {
        self.parent_depth
    }

    /// 向所有子智能体发送停止信号
    ///
    /// 设置中断标志并将所有 Running 状态的智能体标记为 Cancelled。
    pub fn interrupt(&mut self) {
        self.interrupted = true;
        for handle in self.agents.values_mut() {
            if handle.status == AgentStatus::Running {
                handle.status = AgentStatus::Cancelled;
            }
        }
    }

    /// 检查管理器是否已被中断
    pub fn is_interrupted(&self) -> bool {
        self.interrupted
    }
}

// ---------------------------------------------------------------------------
// SubAgentError
// ---------------------------------------------------------------------------

/// 子智能体生命周期操作期间可能发生的错误
#[derive(Debug, thiserror::Error)]
pub enum SubAgentError {
    /// 请求的嵌套深度超过配置的最大值
    #[error("Max sub-agent depth exceeded: requested {requested}, max {max}")]
    MaxDepthExceeded {
        /// 请求的深度
        requested: u32,
        /// 配置的最大深度
        max: u32,
    },

    /// 已达到最大并行子智能体数
    #[error("Max parallel sub-agents exceeded: {active} active, max {max}")]
    MaxParallelExceeded {
        /// 当前活跃子智能体数
        active: usize,
        /// 配置的最大值
        max: usize,
    },

    /// 指定的子智能体 ID 未找到
    #[error("Sub-agent not found: {0}")]
    AgentNotFound(String),

    /// 到子智能体任务的通道已关闭（任务完成或崩溃）
    #[error("Sub-agent channel closed: {0}")]
    ChannelClosed(String),

    /// 操作因父智能体中断而被取消
    #[error("Operation cancelled")]
    Cancelled,

    /// 通用 I/O 或运行时错误
    #[error("Sub-agent runtime error: {0}")]
    Runtime(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use code_agent_protocol::ThreadId;

    // ── Creation and defaults ──────────────────────────────────────────

    #[test]
    fn default_for_root_has_correct_defaults() {
        let mgr = SubAgentManager::default_for_root();
        assert_eq!(mgr.max_depth(), 2);
        assert_eq!(mgr.max_parallel(), 6);
        assert_eq!(mgr.parent_depth(), 0);
        assert_eq!(mgr.total_count(), 0);
        assert_eq!(mgr.active_count(), 0);
        assert!(!mgr.is_interrupted());
    }

    #[test]
    fn custom_construction() {
        let mgr = SubAgentManager::new(4, 8, 1);
        assert_eq!(mgr.max_depth(), 4);
        assert_eq!(mgr.max_parallel(), 8);
        assert_eq!(mgr.parent_depth(), 1);
    }

    // ── Reserve slot ───────────────────────────────────────────────────

    #[test]
    fn reserve_slot_creates_running_agent() {
        let mut mgr = SubAgentManager::new(2, 6, 0);
        let agent_id = mgr
            .reserve_slot("test-task".into(), ThreadId::from("thread-1"), 1)
            .expect("should succeed");

        assert!(agent_id.starts_with("subagent-"));

        let handle = mgr.get_agent(&agent_id).expect("should exist");
        assert_eq!(handle.task_name, "test-task");
        assert_eq!(handle.status, AgentStatus::Running);
        assert_eq!(handle.depth, 1);
        assert_eq!(handle.thread_id.0, "thread-1");
    }

    #[test]
    fn reserve_slot_increments_counter() {
        let mut mgr = SubAgentManager::new(2, 6, 0);
        let id0 = mgr
            .reserve_slot("a".into(), ThreadId::from("th"), 1)
            .unwrap();
        let id1 = mgr
            .reserve_slot("b".into(), ThreadId::from("th"), 1)
            .unwrap();
        assert_eq!(id0, "subagent-0");
        assert_eq!(id1, "subagent-1");
    }

    // ── Depth enforcement ──────────────────────────────────────────────

    #[test]
    fn depth_within_limit_succeeds() {
        let mut mgr = SubAgentManager::new(3, 6, 0);
        assert!(mgr.reserve_slot("t".into(), ThreadId::from("th"), 0).is_ok());
        assert!(mgr.reserve_slot("t".into(), ThreadId::from("th"), 1).is_ok());
        assert!(mgr.reserve_slot("t".into(), ThreadId::from("th"), 2).is_ok());
    }

    #[test]
    fn max_depth_enforcement_blocks_deep_nesting() {
        let mut mgr = SubAgentManager::new(2, 6, 0);
        assert!(mgr.reserve_slot("t1".into(), ThreadId::from("th1"), 1).is_ok());
        let err = mgr
            .reserve_slot("t2".into(), ThreadId::from("th2"), 2)
            .unwrap_err();
        assert!(matches!(err, SubAgentError::MaxDepthExceeded { .. }));
        if let SubAgentError::MaxDepthExceeded { requested, max } = err {
            assert_eq!(requested, 2);
            assert_eq!(max, 2);
        }
    }

    #[test]
    fn check_can_spawn_rejects_at_max() {
        let mgr = SubAgentManager::new(2, 6, 0);
        assert!(mgr.check_can_spawn(0).is_ok());
        assert!(mgr.check_can_spawn(1).is_ok());
        assert!(mgr.check_can_spawn(2).is_err());
    }

    // ── Parallel limit enforcement ─────────────────────────────────────

    #[test]
    fn max_parallel_enforcement_blocks_excess() {
        let mut mgr = SubAgentManager::new(5, 3, 0);
        mgr.reserve_slot("t1".into(), ThreadId::from("th1"), 1).unwrap();
        mgr.reserve_slot("t2".into(), ThreadId::from("th2"), 1).unwrap();
        mgr.reserve_slot("t3".into(), ThreadId::from("th3"), 1).unwrap();

        let err = mgr.reserve_slot("t4".into(), ThreadId::from("th4"), 1).unwrap_err();
        assert!(matches!(err, SubAgentError::MaxParallelExceeded { .. }));
        if let SubAgentError::MaxParallelExceeded { active, max } = err {
            assert_eq!(active, 3);
            assert_eq!(max, 3);
        }
    }

    #[test]
    fn completed_agents_free_parallel_slots() {
        let mut mgr = SubAgentManager::new(5, 2, 0);
        let id1 = mgr.reserve_slot("t1".into(), ThreadId::from("th1"), 1).unwrap();
        mgr.reserve_slot("t2".into(), ThreadId::from("th2"), 1).unwrap();

        mgr.update_status(&id1, AgentStatus::Completed);
        assert!(mgr.reserve_slot("t3".into(), ThreadId::from("th3"), 1).is_ok());
    }

    #[test]
    fn active_count_only_counts_running() {
        let mut mgr = SubAgentManager::new(2, 6, 0);
        let id1 = mgr.reserve_slot("t1".into(), ThreadId::from("th1"), 1).unwrap();
        let id2 = mgr.reserve_slot("t2".into(), ThreadId::from("th2"), 1).unwrap();

        assert_eq!(mgr.active_count(), 2);

        mgr.update_status(&id1, AgentStatus::Completed);
        assert_eq!(mgr.active_count(), 1);

        mgr.update_status(&id2, AgentStatus::Failed);
        assert_eq!(mgr.active_count(), 0);
    }

    // ── Interruption / cancellation ────────────────────────────────────

    #[test]
    fn interrupt_cancels_all_running() {
        let mut mgr = SubAgentManager::new(2, 6, 0);
        mgr.reserve_slot("t1".into(), ThreadId::from("th1"), 1).unwrap();
        mgr.reserve_slot("t2".into(), ThreadId::from("th2"), 1).unwrap();

        mgr.interrupt();
        assert!(mgr.is_interrupted());

        for handle in mgr.list_agents() {
            assert_eq!(handle.status, AgentStatus::Cancelled);
        }
    }

    #[test]
    fn cancelled_interrupt_blocks_spawn() {
        let mut mgr = SubAgentManager::new(2, 6, 0);
        mgr.interrupt();
        let err = mgr.reserve_slot("task".into(), ThreadId::from("th1"), 1).unwrap_err();
        assert!(matches!(err, SubAgentError::Cancelled));
    }

    // ── Status updates and removal ─────────────────────────────────────

    #[test]
    fn update_status_changes_agent() {
        let mut mgr = SubAgentManager::new(2, 6, 0);
        let id = mgr.reserve_slot("task".into(), ThreadId::from("th1"), 1).unwrap();

        mgr.update_status(&id, AgentStatus::Completed);
        assert_eq!(mgr.get_agent(&id).unwrap().status, AgentStatus::Completed);

        mgr.update_status(&id, AgentStatus::Failed);
        assert_eq!(mgr.get_agent(&id).unwrap().status, AgentStatus::Failed);
    }

    #[test]
    fn remove_agent_cleans_up() {
        let mut mgr = SubAgentManager::new(2, 6, 0);
        let id = mgr.reserve_slot("task".into(), ThreadId::from("th1"), 1).unwrap();

        let removed = mgr.remove_agent(&id).unwrap();
        assert_eq!(removed.agent_id, id);
        assert!(mgr.get_agent(&id).is_none());
        assert_eq!(mgr.total_count(), 0);
    }

    #[test]
    fn remove_nonexistent_returns_none() {
        let mut mgr = SubAgentManager::new(2, 6, 0);
        assert!(mgr.remove_agent("no-such-agent").is_none());
    }

    // ── List agents ────────────────────────────────────────────────────

    #[test]
    fn list_agents_returns_all() {
        let mut mgr = SubAgentManager::new(2, 6, 0);
        let id1 = mgr.reserve_slot("a".into(), ThreadId::from("th1"), 1).unwrap();
        let id2 = mgr.reserve_slot("b".into(), ThreadId::from("th2"), 1).unwrap();

        let handles = mgr.list_agents();
        assert_eq!(handles.len(), 2);

        let ids: Vec<String> = handles.iter().map(|h| h.agent_id.clone()).collect();
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
    }

    #[test]
    fn list_agents_empty_initially() {
        let mgr = SubAgentManager::new(2, 6, 0);
        assert!(mgr.list_agents().is_empty());
    }

    // ── AgentResult constructors ───────────────────────────────────────

    #[test]
    fn agent_result_success() {
        let events = vec![ResponseEvent::AgentMessageDelta {
            content: "done".into(),
        }];
        let result = AgentResult::success("agent-1".into(), events.clone(), "all done".into());
        assert_eq!(result.agent_id, "agent-1");
        assert_eq!(result.status, AgentStatus::Completed);
        assert_eq!(result.events, events);
        assert_eq!(result.final_message, Some("all done".into()));
        assert!(result.error.is_none());
    }

    #[test]
    fn agent_result_failure() {
        let result = AgentResult::failure("agent-1".into(), "boom".into());
        assert_eq!(result.status, AgentStatus::Failed);
        assert_eq!(result.error, Some("boom".into()));
        assert!(result.events.is_empty());
        assert!(result.final_message.is_none());
    }

    #[test]
    fn agent_result_cancelled() {
        let result = AgentResult::cancelled("agent-1".into());
        assert_eq!(result.status, AgentStatus::Cancelled);
        assert!(result.error.unwrap().contains("Cancelled"));
    }

    // ── SubAgentError display ──────────────────────────────────────────

    #[test]
    fn sub_agent_error_max_depth_display() {
        let err = SubAgentError::MaxDepthExceeded {
            requested: 4,
            max: 2,
        };
        let msg = err.to_string();
        assert!(msg.contains("4"));
        assert!(msg.contains("2"));
    }

    #[test]
    fn sub_agent_error_agent_not_found_display() {
        let err = SubAgentError::AgentNotFound("ghost-agent".into());
        assert!(err.to_string().contains("ghost-agent"));
    }

    #[test]
    fn sub_agent_error_max_parallel_display() {
        let err = SubAgentError::MaxParallelExceeded {
            active: 6,
            max: 6,
        };
        let msg = err.to_string();
        assert!(msg.contains("6"));
    }

    #[test]
    fn sub_agent_error_cancelled_display() {
        assert!(SubAgentError::Cancelled.to_string().contains("cancelled"));
    }

    // ── SpawnConfig serialization ──────────────────────────────────────

    #[test]
    fn spawn_config_default_is_empty() {
        let cfg = SpawnConfig::default();
        let json = serde_json::to_string(&cfg).expect("serialize");
        assert_eq!(json, "{}");
    }

    #[test]
    fn spawn_config_with_overrides() {
        let cfg = SpawnConfig {
            model: Some("fast-model".into()),
            permission_mode: Some(PermissionMode::Auto),
            system_instructions: Some("Be concise.".into()),
            max_iterations: Some(10),
            temperature: Some(0.5),
            token_budget: None,
        };
        let json = serde_json::to_string(&cfg).expect("serialize");
        assert!(json.contains("fast-model"));
        assert!(json.contains("auto"));
        assert!(json.contains("Be concise."));
        assert!(json.contains("10"));
        assert!(json.contains("0.5"));
    }

    #[test]
    fn spawn_config_round_trip() {
        let cfg = SpawnConfig {
            model: Some("test-model".into()),
            permission_mode: Some(PermissionMode::Permit),
            system_instructions: None,
            max_iterations: Some(5),
            temperature: None,
            token_budget: None,
        };
        let json = serde_json::to_string(&cfg).expect("serialize");
        let parsed: SpawnConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.model, Some("test-model".into()));
        assert_eq!(parsed.permission_mode, Some(PermissionMode::Permit));
        assert!(parsed.system_instructions.is_none());
        assert_eq!(parsed.max_iterations, Some(5));
        assert!(parsed.temperature.is_none());
    }

    // ── SpawnTask serialization ────────────────────────────────────────

    #[test]
    fn spawn_task_round_trip() {
        let task = SpawnTask {
            task_name: "review-code".into(),
            message: "Review src/main.rs for bugs".into(),
            config_override: Some(SpawnConfig {
                model: Some("small-model".into()),
                ..Default::default()
            }),
        };
        let json = serde_json::to_string(&task).expect("serialize");
        let parsed: SpawnTask = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.task_name, "review-code");
        assert_eq!(parsed.message, "Review src/main.rs for bugs");
        assert!(parsed.config_override.is_some());
        assert_eq!(parsed.config_override.unwrap().model, Some("small-model".into()));
    }

    #[test]
    fn spawn_task_without_override() {
        let task = SpawnTask {
            task_name: "simple".into(),
            message: "do it".into(),
            config_override: None,
        };
        let json = serde_json::to_string(&task).expect("serialize");
        let parsed: SpawnTask = serde_json::from_str(&json).expect("deserialize");
        assert!(parsed.config_override.is_none());
    }

    // ── AgentStatus serialization ──────────────────────────────────────

    #[test]
    fn agent_status_serde_round_trip() {
        for status in [
            AgentStatus::Running,
            AgentStatus::Completed,
            AgentStatus::Failed,
            AgentStatus::Cancelled,
        ] {
            let json = serde_json::to_string(&status).expect("serialize");
            let parsed: AgentStatus = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(status, parsed);
        }
    }

    #[test]
    fn agent_status_json_values() {
        assert_eq!(
            serde_json::to_string(&AgentStatus::Running).unwrap(),
            "\"running\""
        );
        assert_eq!(
            serde_json::to_string(&AgentStatus::Completed).unwrap(),
            "\"completed\""
        );
        assert_eq!(
            serde_json::to_string(&AgentStatus::Failed).unwrap(),
            "\"failed\""
        );
        assert_eq!(
            serde_json::to_string(&AgentStatus::Cancelled).unwrap(),
            "\"cancelled\""
        );
    }
}
