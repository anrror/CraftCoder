//! Git 操作安全策略执行
//!
//! 【领域含义】为 Git 操作提供安全策略执行机制，每个操作被分类为 **safe**、**warn** 或 **block**。
//! `OperationCategory` 枚举驱动决策，`GitClient` 在每个变更操作前调用 `check_safety`。
//!
//! 【核心职责】根据操作分类和安全策略配置，决定是否允许操作执行。

use tracing::warn;

use super::{GitError, GitResult, SafetyPolicy};

/// Git 操作安全分类
///
/// 【领域含义】对 Git 操作进行安全等级分类，决定操作是否需要特殊权限。
/// 【核心职责】区分只读安全操作、潜在危险操作和需要显式授权的危险操作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationCategory {
    /// 只读/非破坏性操作 — 始终允许
    Safe,

    /// 潜在危险操作 — 允许但记录警告日志
    Warn,

    /// 需要显式授权 — 默认禁止，需在 `SafetyPolicy` 中启用
    BlockUnlessAllowed {
        /// 对应的安全策略标志名（如 `"allow_force_push"`）
        policy_flag: &'static str,
    },
}

/// 检查操作是否被安全策略允许
///
/// 【领域含义】根据安全策略评估一个 Git 操作是否允许执行。
///
/// # 决策矩阵
///
/// | 分类                                | 结果                                    |
/// |-------------------------------------|-----------------------------------------|
/// | `Safe`                              | `Ok(())`                                |
/// | `Warn`                              | `warn!` 日志 + `Ok(())`                 |
/// | `BlockUnlessAllowed { "force" }`    | 未授权时返回 `Err(PermissionDenied)`    |
///
/// # 参数
///
/// * `safety` — 要检查的安全策略
/// * `operation` — 操作名称（用于错误消息）
/// * `category` — 操作的分类
///
/// 【核心职责】根据分类执行决策：Safe 直接通过，Warn 记录日志后通过，Block 检查策略标志。
pub fn check_safety(safety: &SafetyPolicy, operation: &str, category: OperationCategory) -> GitResult<()> {
    match category {
        OperationCategory::Safe => Ok(()),

        OperationCategory::Warn => {
            warn!(
                operation = operation,
                "Potentially dangerous git operation executed by agent"
            );
            Ok(())
        }

        OperationCategory::BlockUnlessAllowed { policy_flag } => {
            let allowed = match policy_flag {
                "allow_force_push" => safety.allow_force_push,
                "allow_reset_hard" => safety.allow_reset_hard,
                other => {
                    return Err(GitError::PermissionDenied(format!(
                        "Unknown safety policy flag: '{}' for operation '{}'",
                        other, operation
                    )));
                }
            };
            if allowed {
                warn!(
                    operation = operation,
                    policy_flag = policy_flag,
                    "Agent performed a restricted git operation (explicitly allowed)"
                );
                Ok(())
            } else {
                Err(GitError::PermissionDenied(format!(
                    "Operation '{}' requires '{}' to be enabled in the safety policy",
                    operation, policy_flag
                )))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::SafetyPolicy;

    // ── Safe operations always pass ────────────────────────────────

    #[test]
    fn safe_operations_always_pass() {
        let safety = SafetyPolicy::default();
        assert!(check_safety(&safety, "status", OperationCategory::Safe).is_ok());
        assert!(check_safety(&safety, "diff", OperationCategory::Safe).is_ok());
        assert!(check_safety(&safety, "log", OperationCategory::Safe).is_ok());
        assert!(check_safety(&safety, "show", OperationCategory::Safe).is_ok());
        assert!(check_safety(&safety, "commit", OperationCategory::Safe).is_ok());
    }

    // ── Warn operations ────────────────────────────────────────────

    #[test]
    fn warn_operations_pass_but_log() {
        let safety = SafetyPolicy::default();
        assert!(check_safety(&safety, "reset --soft", OperationCategory::Warn).is_ok());
        assert!(check_safety(&safety, "push", OperationCategory::Warn).is_ok());
    }

    // ── Block operations denied by default ─────────────────────────

    #[test]
    fn force_push_denied_by_default() {
        let safety = SafetyPolicy::default();
        let result = check_safety(
            &safety,
            "push --force",
            OperationCategory::BlockUnlessAllowed {
                policy_flag: "allow_force_push",
            },
        );
        assert!(result.is_err());
        match result {
            Err(GitError::PermissionDenied(msg)) => {
                assert!(msg.contains("push --force"));
                assert!(msg.contains("allow_force_push"));
            }
            other => panic!("expected PermissionDenied, got {:?}", other),
        }
    }

    #[test]
    fn reset_hard_denied_by_default() {
        let safety = SafetyPolicy::default();
        let result = check_safety(
            &safety,
            "reset --hard",
            OperationCategory::BlockUnlessAllowed {
                policy_flag: "allow_reset_hard",
            },
        );
        assert!(result.is_err());
        match result {
            Err(GitError::PermissionDenied(msg)) => {
                assert!(msg.contains("reset --hard"));
                assert!(msg.contains("allow_reset_hard"));
            }
            other => panic!("expected PermissionDenied, got {:?}", other),
        }
    }

    // ── Block operations allowed when explicitly enabled ──────────

    #[test]
    fn force_push_allowed_when_enabled() {
        let safety = SafetyPolicy {
            allow_force_push: true,
            ..Default::default()
        };
        let result = check_safety(
            &safety,
            "push --force",
            OperationCategory::BlockUnlessAllowed {
                policy_flag: "allow_force_push",
            },
        );
        assert!(result.is_ok());
    }

    #[test]
    fn reset_hard_allowed_when_enabled() {
        let safety = SafetyPolicy {
            allow_reset_hard: true,
            ..Default::default()
        };
        let result = check_safety(
            &safety,
            "reset --hard",
            OperationCategory::BlockUnlessAllowed {
                policy_flag: "allow_reset_hard",
            },
        );
        assert!(result.is_ok());
    }

    // ── Unknown policy flags ──────────────────────────────────────

    #[test]
    fn unknown_policy_flag_returns_error() {
        let safety = SafetyPolicy::default();
        let result = check_safety(
            &safety,
            "unknown-op",
            OperationCategory::BlockUnlessAllowed {
                policy_flag: "nonexistent_flag",
            },
        );
        assert!(result.is_err());
        match result {
            Err(GitError::PermissionDenied(msg)) => {
                assert!(msg.contains("nonexistent_flag"));
            }
            other => panic!("expected PermissionDenied, got {:?}", other),
        }
    }

    // ── OperationCategory equality ─────────────────────────────────

    #[test]
    fn operation_category_debug() {
        let safe = OperationCategory::Safe;
        assert_eq!(safe, OperationCategory::Safe);
        assert_ne!(safe, OperationCategory::Warn);
    }
}
