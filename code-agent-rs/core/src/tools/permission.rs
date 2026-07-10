//! Permission enforcement for tool calls.
//!
//! `PermissionEnforcer` sits in front of the tool router and gates every
//! tool call based on the current [`PermissionMode`] and the tool's
//! [`CapabilityLevel`].
//!
//! # 领域描述
//!
//! 权限守卫是工具调用链路中的安全边界，确保每个工具调用在
//! 执行前都经过权限模式（Auto/Permit/Block）与能力等级（Read/Edit/Exec）的匹配检查。

use code_agent_protocol::{CapabilityLevel, PermissionMode};

use super::ToolError;

/// 工具调用权限守卫 —— 在工具执行前进行访问控制检查
///
/// 【领域含义】PermissionEnforcer 是工具调用管道的守卫层，
/// 根据当前 PermissionMode 与工具声明 CapabilityLevel 的对比结果
/// 决定放行或拒绝。它是 AI Agent 安全策略的核心执行点。
///
/// Enforces access control on tool calls.
///
/// The enforcer compares the tool's required [`CapabilityLevel`] against the
/// current [`PermissionMode`]:
///
/// | Mode     | Read | Edit | Exec |
/// |----------|------|------|------|
/// | `Auto`   | ✓    | ✓    | ✓    |
/// | `Permit` | ✓    | ✗    | ✗    |
/// | `Block`  | ✓    | ✗    | ✗    |
///
/// In `Permit` mode Edit and Exec tools require explicit user confirmation;
/// the confirmation flow is handled by the caller. `Block` mode restricts to
/// read-only access.
#[derive(Debug, Clone)]
pub struct PermissionEnforcer {
    mode: PermissionMode,
}

impl PermissionEnforcer {
    /// 创建权限守卫实例
    ///
    /// 【领域含义】使用指定的权限模式初始化守卫。
    /// 模式一经创建可通过 set_mode 动态调整。
    ///
    /// Create a new enforcer with the given permission mode.
    pub fn new(mode: PermissionMode) -> Self {
        Self { mode }
    }

    /// 获取当前权限模式
    ///
    /// 【领域含义】返回守卫当前的 PermissionMode 值，
    /// 用于调用方判断当前安全策略状态。
    ///
    /// Returns the current permission mode.
    pub fn mode(&self) -> &PermissionMode {
        &self.mode
    }

    /// 动态更新权限模式
    ///
    /// 【领域含义】运行时调整守卫的安全策略级别，
    /// 无需重新创建实例即可在 Auto/Permit/Block 间切换。
    ///
    /// Update the permission mode in-place.
    pub fn set_mode(&mut self, mode: PermissionMode) {
        self.mode = mode;
    }

    /// 检查工具调用是否被允许
    ///
    /// 【领域含义】核心授权决策方法。根据当前模式与工具所需能力等级
    /// 做矩阵匹配：Auto 全部放行；Permit 仅 Read 自动放行；
    /// Block 仅 Read 放行。拒绝时返回 PermissionDenied 错误。
    ///
    /// # Arguments
    /// * `tool_name` – name of the tool (used in error messages).
    /// * `capability` – the capability level required by the tool.
    ///
    /// # Errors
    /// Returns [`ToolError::PermissionDenied`] if the tool is not allowed.
    pub fn check(&self, tool_name: &str, capability: CapabilityLevel) -> Result<(), ToolError> {
        match self.mode {
            PermissionMode::Auto => {
                // Auto mode allows everything.
            }
            PermissionMode::Permit => {
                // Permit mode: only Read is auto-allowed.
                // Edit/Exec require external confirmation (handled by caller).
                if capability > CapabilityLevel::Read {
                    return Err(ToolError::permission_denied(format!(
                        "tool '{tool_name}' requires {capability:?} capability, \
                         but current mode is Permit (auto-allow: Read only)"
                    )));
                }
            }
            PermissionMode::Block => {
                // Block mode: restrict to read-only access.
                if capability > CapabilityLevel::Read {
                    return Err(ToolError::permission_denied(format!(
                        "tool '{tool_name}' requires {capability:?} capability, \
                         but current mode is Block (max: Read)"
                    )));
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_allows_all_capabilities() {
        let enforcer = PermissionEnforcer::new(PermissionMode::Auto);
        assert!(enforcer.check("read", CapabilityLevel::Read).is_ok());
        assert!(enforcer.check("edit", CapabilityLevel::Edit).is_ok());
        assert!(enforcer.check("exec", CapabilityLevel::Exec).is_ok());
    }

    #[test]
    fn block_allows_read_denies_edit_and_exec() {
        let enforcer = PermissionEnforcer::new(PermissionMode::Block);
        assert!(enforcer.check("read", CapabilityLevel::Read).is_ok());
        assert!(enforcer.check("write_file", CapabilityLevel::Edit).is_err());
        assert!(enforcer.check("bash", CapabilityLevel::Exec).is_err());
    }

    #[test]
    fn permit_allows_read_denies_edit_and_exec() {
        let enforcer = PermissionEnforcer::new(PermissionMode::Permit);
        assert!(enforcer.check("read_file", CapabilityLevel::Read).is_ok());
        assert!(enforcer.check("write_file", CapabilityLevel::Edit).is_err());
        assert!(enforcer.check("bash", CapabilityLevel::Exec).is_err());
    }

    #[test]
    fn set_mode_updates_behavior() {
        let mut enforcer = PermissionEnforcer::new(PermissionMode::Auto);
        assert!(enforcer.check("exec", CapabilityLevel::Exec).is_ok());

        enforcer.set_mode(PermissionMode::Block);
        assert!(enforcer.check("read_file", CapabilityLevel::Read).is_ok());
        assert!(enforcer.check("exec", CapabilityLevel::Exec).is_err());
    }

    #[test]
    fn block_error_message_contains_tool_name_and_mode() {
        let enforcer = PermissionEnforcer::new(PermissionMode::Block);
        let err = enforcer
            .check("dangerous_tool", CapabilityLevel::Exec)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("dangerous_tool"));
        assert!(msg.contains("Block"));
    }
}
