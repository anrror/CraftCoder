//! 配置层 (Configuration Layer) — 会话状态与安全策略值对象
//!
//! 本模块定义了 Agent 系统的三个关键配置型值对象：
//! [`SessionStatus`] 描述会话生命周期阶段，
//! [`PermissionMode`] 控制工具调用的权限策略，
//! [`CapabilityLevel`] 定义 Agent 的操作能力边界。
//!
//! 【DDD 分层】配置层属于领域层的值对象。
//! 这些枚举定义了系统的业务规则和状态机，
//! 被核心 Agent 循环、权限管理器、会话管理器共同依赖。
//!
//! 【不变式】
//! - SessionStatus 遵循严格的状态转换规则（见各变体说明）
//! - CapabilityLevel 是全序关系：Read < Edit < Exec
//! - PermissionMode 在单个会话中可以动态切换

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// SessionStatus — 会话生命周期状态
// ---------------------------------------------------------------------------

/// 会话生命周期状态 (Session Lifecycle State Value Object)
///
/// 【领域含义】描述一个编码会话从创建到归档的完整生命周期阶段。
/// SessionStatus 是一个有限状态机，状态之间的转换受到业务规则约束。
///
/// 【状态转换图】
/// ```text
/// Active ──→ Paused ──→ Active    (暂停/恢复)
/// Active ──→ Completed             (任务完成)
/// Completed ──→ Archived           (归档)
/// Paused ──→ Completed             (暂停中也可标记完成)
/// Archived 是终态，不可进一步转换
/// ```
///
/// 【使用场景】
/// - SessionStore 按状态筛选会话（如查询所有 Active 会话）
/// - UI 层根据状态显示不同的操作按钮（恢复、归档、删除）
/// - 后台任务扫描长时间 Paused 的会话并提示用户
///
/// 【序列化格式】`#[serde(rename_all = "snake_case")]`
/// JSON 输出: `"active"`, `"paused"`, `"completed"`, `"archived"`
///
/// 【与其他类型的关系】
/// - 描述 [`crate::identity::SessionId`] 所标识会话的当前状态
/// - 通常与 [`PermissionMode`] 一起存储在会话配置中
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// 会话活跃中 — Agent 正在处理用户的输入
    ///
    /// 【进入条件】会话创建时的初始状态
    /// 【可转换至】Paused, Completed
    Active,
    /// 会话已暂停 — 用户暂时离开或主动暂停
    ///
    /// 【进入条件】用户触发暂停操作，或系统检测到超时
    /// 【可转换至】Active, Completed
    Paused,
    /// 会话已完成 — 任务目标已达成
    ///
    /// 【进入条件】Agent 完成最终响应，用户确认完成
    /// 【可转换至】Archived
    Completed,
    /// 会话已归档 — 长期存储，不可再操作
    ///
    /// 【进入条件】用户或系统触发归档操作
    /// 【可转换至】无（终态）
    Archived,
}

// ---------------------------------------------------------------------------
// PermissionMode — 工具调用权限策略
// ---------------------------------------------------------------------------

/// 工具调用权限模式 (Tool Permission Policy Value Object)
///
/// 【领域含义】定义 Agent 在执行工具调用时的用户交互策略。
/// 不同的权限模式对应不同的安全等级和自动化程度。
///
/// 【三种策略】
/// - `Auto`: 全自动 — 完全信任 Agent，不询问用户
/// - `Permit`: 需授权 — 每次工具调用前询问用户
/// - `Block`: 全禁止 — Agent 只能使用内置知识，不可调用任何外部工具
///
/// 【使用场景】
/// - PermissionManager 根据当前 PermissionMode 决定工具调用流程
/// - 用户可在 TUI 中实时切换权限模式（如从 Permit 切换到 Auto）
/// - Web UI 提供权限模式选择器
///
/// 【序列化格式】JSON 输出: `"auto"`, `"permit"`, `"block"`
///
/// 【与其他类型的关系】
/// - 与 [`CapabilityLevel`] 配合使用：PermissionMode 控制交互流程，
///   CapabilityLevel 控制能力边界
/// - 通常存储在会话级别的配置中
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// 自动执行 — 工具调用无需用户确认，直接执行
    ///
    /// 【适用场景】用户完全信任 Agent 在安全环境下工作
    Auto,
    /// 需要授权 — 每次工具调用前暂停并询问用户
    ///
    /// 【适用场景】默认模式，平衡效率与安全
    Permit,
    /// 禁止执行 — 阻塞所有工具调用
    ///
    /// 【适用场景】用户仅想查看 Agent 的建议但不想执行任何操作
    Block,
}

// ---------------------------------------------------------------------------
// CapabilityLevel — Agent 操作能力边界
// ---------------------------------------------------------------------------

/// Agent 操作能力级别 (Agent Capability Boundary Value Object)
///
/// 【领域含义】定义 Agent 可执行操作的范围和安全边界。
/// 这是一个递增的权限体系：更高级别包含所有低级别的能力。
///
/// 【能力层级】
/// - `Read`: 只读 — 可查看但不能修改任何文件或系统状态
/// - `Edit`: 读写 — 在 Read 基础上增加文件编辑和 Git 操作
/// - `Exec`: 完全 — 在 Edit 基础上增加命令执行能力
///
/// 【全序关系】Read < Edit < Exec（实现 `PartialOrd` + `Ord`）
/// 这允许代码中直接用 `>` / `<` 比较能力级别：
/// ```rust
/// use code_agent_protocol::CapabilityLevel;
/// assert!(CapabilityLevel::Exec > CapabilityLevel::Read);
/// ```
///
/// 【使用场景】
/// - 权限检查：`if agent.capability >= CapabilityLevel::Edit { ... }`
/// - SubAgent 沙箱：子 Agent 的能力级别不能超过主 Agent
/// - 工具注册：每个工具声明所需的最低能力级别
///
/// 【序列化格式】JSON 输出: `"read"`, `"edit"`, `"exec"`
///
/// 【与其他类型的关系】
/// - 与 [`PermissionMode`] 互补：PermissionMode 管交互，CapabilityLevel 管边界
/// - 在 SubAgent 创建时作为约束参数传递
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityLevel {
    /// 只读能力 — 可读取文件、列出目录、搜索代码，不可修改
    ///
    /// 【适用场景】代码审查、代码理解、安全最低权限原则
    Read,
    /// 读写能力 — 在 Read 基础上增加文件编辑和 Git 操作
    ///
    /// 【适用场景】代码重构、功能开发、文档编写
    Edit,
    /// 完全能力 — 在 Edit 基础上增加 Shell 命令执行
    ///
    /// 【适用场景】完整开发流程：构建、测试、部署
    Exec,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_status_all_variants_exist() {
        let _ = SessionStatus::Active;
        let _ = SessionStatus::Paused;
        let _ = SessionStatus::Completed;
        let _ = SessionStatus::Archived;
    }

    #[test]
    fn permission_mode_all_variants_exist() {
        let _ = PermissionMode::Auto;
        let _ = PermissionMode::Permit;
        let _ = PermissionMode::Block;
    }

    #[test]
    fn capability_level_all_variants_exist() {
        let _ = CapabilityLevel::Read;
        let _ = CapabilityLevel::Edit;
        let _ = CapabilityLevel::Exec;
    }

    #[test]
    fn capability_level_ordering() {
        assert!(CapabilityLevel::Read < CapabilityLevel::Edit);
        assert!(CapabilityLevel::Edit < CapabilityLevel::Exec);
        assert!(CapabilityLevel::Read < CapabilityLevel::Exec);
    }
}
