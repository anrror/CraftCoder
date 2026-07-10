//! 标识层 (Identity Layer) — 领域标识值对象
//!
//! 本模块定义了编码会话系统中最底层的三个不可变值对象：
//! [`SessionId`]、[`ThreadId`]、[`TurnId`]。
//!
//! 【DDD 分层】标识层属于「通用语言 (Ubiquitous Language)」的核心词汇，
//! 所有上层模块（消息层、执行层、配置层）均依赖本层的值对象。
//!
//! 【值对象特征】
//! - 不可变性：创建后内部值不可修改
//! - 相等性：基于值语义比较，而非引用比较
//! - 自校验：提供工厂方法封装构造逻辑

use derive_more::Display;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// SessionId — 会话标识
// ---------------------------------------------------------------------------

/// 会话唯一标识 (Session Identity Value Object)
///
/// 【领域含义】每个编码会话从开始到结束的唯一标识。
/// SessionId 是不可变值对象，创建后不可修改。
/// 在整个系统中，SessionId 是追踪用户与 Agent 交互的根标识。
///
/// 【使用场景】
/// - 在 ThreadManager 中查找会话
/// - 在 SessionStore 中持久化会话状态
/// - 在日志 / 链路追踪中定位特定会话
/// - 作为 API 端点参数标识目标会话
///
/// 【约束】
/// - SessionId 应为全局唯一字符串
/// - 建议格式: `"sess-{uuid}"` 或 `"sess-{timestamp_hex}"`
/// - 空字符串不被禁止，但建议调用方保证非空
///
/// 【与其他类型的关系】
/// - 一对多关联 [`ThreadId`]：一个会话可包含多个线程
/// - 被 [`crate::config::SessionStatus`] 描述生命周期状态
#[derive(
    Clone, Debug, Display, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[display("{_0}")]
pub struct SessionId(pub String);

impl SessionId {
    /// 从任意可转换为 String 的值创建 SessionId
    ///
    /// 【使用场景】当调用方已有明确的标识字符串时使用。
    ///
    /// ```rust
    /// use code_agent_protocol::SessionId;
    /// let id = SessionId::new("sess-abc-123");
    /// ```
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// 生成一个新的全局唯一 SessionId
    ///
    /// 【生成策略】基于当前系统时间纳秒数的十六进制表示，
    /// 格式为 `"sess-{timestamp_nanos_hex}"`。
    /// 在单机非并发场景下保证唯一性；并发场景建议使用 UUID。
    ///
    /// ```rust
    /// use code_agent_protocol::SessionId;
    /// let id = SessionId::generate();
    /// assert!(id.0.starts_with("sess-"));
    /// ```
    pub fn generate() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self(format!("sess-{nanos:x}"))
    }
}

impl From<String> for SessionId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for SessionId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

// ---------------------------------------------------------------------------
// ThreadId — 线程标识
// ---------------------------------------------------------------------------

/// 线程唯一标识 (Thread Identity Value Object)
///
/// 【领域含义】会话内部逻辑对话分支的唯一标识。
/// 一个 Thread 代表一次完整的任务执行上下文（如：修复一个 bug、
/// 实现一个 feature），包含多轮 Turn。
///
/// 【使用场景】
/// - 在 Agent 循环中追踪当前线程
/// - 在持久化层按线程粒度存储对话历史
/// - 在 UI 层切换不同线程的对话界面
/// - SubAgent 模式下作为子任务线程标识
///
/// 【约束】
/// - ThreadId 在单个 Session 内必须唯一
/// - 建议格式: `"thread-{uuid}"` 或 `"thread-{timestamp_hex}"`
///
/// 【与其他类型的关系】
/// - 属于某个 [`SessionId`]：线程归属于会话
/// - 一对多关联 [`crate::identity::TurnId`]：一个线程包含多轮对话
/// - 被 [`crate::execution::TurnInput`] 引用作为执行上下文
#[derive(
    Clone, Debug, Display, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[display("{_0}")]
pub struct ThreadId(pub String);

impl ThreadId {
    /// 从任意可转换为 String 的值创建 ThreadId
    ///
    /// ```rust
    /// use code_agent_protocol::ThreadId;
    /// let id = ThreadId::new("thread-xyz-456");
    /// ```
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// 生成一个新的全局唯一 ThreadId
    ///
    /// 【生成策略】与 [`SessionId::generate()`] 相同，使用系统时间纳秒数。
    /// 格式为 `"thread-{timestamp_nanos_hex}"`。
    ///
    /// ```rust
    /// use code_agent_protocol::ThreadId;
    /// let id = ThreadId::generate();
    /// assert!(id.0.starts_with("thread-"));
    /// ```
    pub fn generate() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self(format!("thread-{nanos:x}"))
    }
}

impl From<String> for ThreadId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ThreadId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

// ---------------------------------------------------------------------------
// TurnId — 轮次标识
// ---------------------------------------------------------------------------

/// 轮次唯一标识 (Turn Identity Value Object)
///
/// 【领域含义】线程中单轮交互的唯一标识。一轮完整的 Turn 包含：
/// 用户输入 → Agent 推理 → 工具调用（可选）→ 响应输出。
///
/// 【使用场景】
/// - 在事件流中用 TurnId 关联事件到特定轮次
/// - 在压缩/摘要功能中按轮次索引历史
/// - 在日志中追踪单次 Agent 调用的完整链路
/// - 在计费系统中按轮次统计 Token 消耗
///
/// 【约束】
/// - TurnId 在单个 Thread 内必须唯一
/// - Turn 在 Thread 内是有序序列（按时间先后排列）
/// - 建议格式: `"turn-{sequence_number}"` 或 `"turn-{timestamp_hex}"`
///
/// 【与其他类型的关系】
/// - 属于某个 [`ThreadId`]：轮次归属于线程
/// - 被 [`crate::execution::ResponseEvent`] 的事件变体引用
///   (`TurnStarted`, `TurnComplete` 携带 TurnId)
#[derive(
    Clone, Debug, Display, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[display("{_0}")]
pub struct TurnId(pub String);

impl TurnId {
    /// 从任意可转换为 String 的值创建 TurnId
    ///
    /// ```rust
    /// use code_agent_protocol::TurnId;
    /// let id = TurnId::new("turn-001");
    /// ```
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// 生成一个新的全局唯一 TurnId
    ///
    /// 【生成策略】与 [`SessionId::generate()`] 相同，使用系统时间纳秒数。
    /// 格式为 `"turn-{timestamp_nanos_hex}"`。
    ///
    /// ```rust
    /// use code_agent_protocol::TurnId;
    /// let id = TurnId::generate();
    /// assert!(id.0.starts_with("turn-"));
    /// ```
    pub fn generate() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self(format!("turn-{nanos:x}"))
    }
}

impl From<String> for TurnId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for TurnId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_new_and_generate() {
        let id = SessionId::new("sess-test");
        assert_eq!(id.0, "sess-test");
        assert_eq!(id.to_string(), "sess-test");

        let gen = SessionId::generate();
        assert!(gen.0.starts_with("sess-"));
        assert!(!gen.0.ends_with("sess-"));
    }

    #[test]
    fn thread_id_new_and_generate() {
        let id = ThreadId::new("thread-test");
        assert_eq!(id.to_string(), "thread-test");

        let gen = ThreadId::generate();
        assert!(gen.0.starts_with("thread-"));
    }

    #[test]
    fn turn_id_new_and_generate() {
        let id = TurnId::new("turn-test");
        assert_eq!(id.to_string(), "turn-test");

        let gen = TurnId::generate();
        assert!(gen.0.starts_with("turn-"));
    }
}
