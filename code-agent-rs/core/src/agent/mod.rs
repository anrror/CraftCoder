//! ReAct 智能体循环 —— Agent 核心执行引擎
//!
//! 【领域含义】本模块是 Coding Agent 的核心领域层，实现了 ReAct (Reasoning + Acting)
//! 智能循环。Agent 通过「推理 → 行动 → 观察 → 再推理」的循环模式，自主完成编码任务。
//!
//! 【核心领域概念】
//! - `Session`: 一次交互式编码会话的完整生命周期，维护会话状态和对话历史
//! - `ThreadManager`: 线程管理器，按 ThreadId 管理多个并发会话
//! - `TurnContext`: 单轮交互的执行上下文，包含该轮的用户输入和线程归属
//! - `SubAgentManager`: 子智能体管理器，支持层次化多智能体协作
//!
//! 【领域职责】
//! 1. 接收用户输入，驱动 ReAct 循环
//! 2. 调用 ModelClient 获取模型推理结果
//! 3. 解析工具调用或最终答案
//! 4. 通过 ToolRegistry 执行工具
//! 5. 管理会话生命周期（创建、中断、恢复）
//!
//! # Architecture
//!
//! ```text
//! User Input → Session.run_turn(TurnInput) → Vec<ResponseEvent>
//!                   │
//!     ┌─────────────┼─────────────┐
//!     ▼             ▼             ▼
//! ModelClient   ToolRegistry   ContextManager
//! (streaming)   (execution)    (history)
//! ```
//!
//! The ReAct loop:
//! 1. Build prompt (system + history + tool definitions)
//! 2. Call model (streaming) → collect text deltas + tool calls
//! 3. If tool calls: execute each tool → append results → loop to step 1
//! 4. If final text: emit TurnComplete, return events

pub mod session;
pub mod sub_agent;
pub mod sub_agent_manager;
pub mod thread_manager;
pub mod turn;

pub use session::{Session, SessionConfig, SessionState};
// ContextManager is now defined in the `context` module with the 5-layer
// compaction pipeline. Re-export for backward compatibility.
pub use crate::context::ContextManager;
pub use sub_agent::{
    AgentResult, AgentStatus, SpawnConfig, SpawnTask, SubAgentError, SubAgentHandle,
    SubAgentManager,
};
pub use sub_agent_manager::SubAgentManagerImpl;
pub use thread_manager::ThreadManager;
pub use turn::TurnContext;
