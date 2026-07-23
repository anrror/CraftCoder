//! Token 预算分配 —— 角色级 Token 配额管理与分配
//!
//! 【领域含义】TokenBudgetAllocator 是 Phase C 上下文增强的核心组件之一。
//! 它为 Plan 引擎中的不同角色（Delegator / Coder / Planner）分配独立的
//! Token 预算，防止单个角色消耗过多上下文窗口。
//!
//! # 设计
//!
//! ```text
//!                   ┌──────────────────────┐
//!                   │   Total Budget 64K   │
//!                   │  (model max tokens)  │
//!                   └──────┬───────────────┘
//!                           │
//!            ┌──────────────┼──────────────┐
//!            ▼              ▼              ▼
//!     ┌──────────┐   ┌──────────┐   ┌──────────┐
//!     │ Planner  │   │ Coder #1 │   │ Coder #2 │  ...
//!     │ 8K       │   │ 16K      │   │ 16K      │
//!     └──────────┘   └──────────┘   └──────────┘
//!                           │
//!                      ┌────┴────┐
//!                      │ reserve │→ allocation record
//!                      │ release │→ free token return
//!                      └─────────┘
//! ```

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// TokenRole
// ---------------------------------------------------------------------------

/// Token 预算分配的角色类型。
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenRole {
    /// Planner（计划者）：任务分解、规划
    Planner,
    /// Delegator（调度者）：任务派发与协调
    Delegator,
    /// Coder（执行者）：具体编码实现
    Coder,
    /// 系统上下文（system prompt, 工具定义等）
    System,
    /// 用户定义的角色
    Custom(String),
}

impl std::fmt::Display for TokenRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenRole::Planner => write!(f, "planner"),
            TokenRole::Delegator => write!(f, "delegator"),
            TokenRole::Coder => write!(f, "coder"),
            TokenRole::System => write!(f, "system"),
            TokenRole::Custom(name) => write!(f, "custom:{name}"),
        }
    }
}

// ---------------------------------------------------------------------------
// BudgetAllocation
// ---------------------------------------------------------------------------

/// 单次预算分配记录。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BudgetAllocation {
    /// 分配的唯一标识
    pub id: String,
    /// 分配的角色
    pub role: TokenRole,
    /// 分配的 Token 数量
    pub granted: usize,
    /// 本次分配后该角色累积已用 Token
    pub role_used: usize,
    /// 本次分配后该角色剩余 Token
    pub role_remaining: usize,
    /// 本次分配后总剩余 Token
    pub total_remaining: usize,
}

// ---------------------------------------------------------------------------
// TokenBudgetConfig
// ---------------------------------------------------------------------------

/// Token 预算配置。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenBudgetConfig {
    /// 总可用 Token 数量（模型上下文窗口上限）
    pub total_budget: usize,
    /// 各角色的默认预算比例（相对于 total_budget 的百分比，0.0–1.0）
    pub role_ratios: HashMap<TokenRole, f64>,
    /// Planner 预算比例（默认 0.125 = 12.5%）
    pub planner_ratio: f64,
    /// Delegator 预算比例（默认 0.125 = 12.5%）
    pub delegator_ratio: f64,
    /// 单个 Coder 预算比例（默认 0.25 = 25%）
    pub coder_ratio: f64,
    /// 系统上下文预算比例（默认 0.125 = 12.5%）
    pub system_ratio: f64,
    /// 缓冲区比例（未分配，用于弹性，默认 0.25 = 25%）
    pub buffer_ratio: f64,
}

impl Default for TokenBudgetConfig {
    fn default() -> Self {
        Self {
            total_budget: 64_000,
            role_ratios: HashMap::new(),
            planner_ratio: 0.125,
            delegator_ratio: 0.125,
            coder_ratio: 0.375,
            system_ratio: 0.125,
            buffer_ratio: 0.25,
        }
    }
}

impl TokenBudgetConfig {
    /// 取整后各角色预算之和 = total_budget (含 buffer)
    pub fn validate(&self) -> Result<(), String> {
        let sum = self.planner_ratio
            + self.delegator_ratio
            + self.coder_ratio
            + self.system_ratio
            + self.buffer_ratio;
        if (sum - 1.0).abs() > 0.001 {
            return Err(format!(
                "Token budget ratios sum to {:.3}, expected 1.0", sum
            ));
        }
        Ok(())
    }

    /// 获取指定角色的预算比例。
    pub fn role_ratio(&self, role: &TokenRole) -> f64 {
        match role {
            TokenRole::Planner => self.planner_ratio,
            TokenRole::Delegator => self.delegator_ratio,
            TokenRole::Coder => self.coder_ratio,
            TokenRole::System => self.system_ratio,
            TokenRole::Custom(_) => {
                self.role_ratios.get(role).copied().unwrap_or(self.coder_ratio)
            }
        }
    }

    /// 获取指定角色的预算上限（取整）。
    pub fn role_budget(&self, role: &TokenRole) -> usize {
        let raw = self.total_budget as f64 * self.role_ratio(role);
        (raw.round() as usize).max(1)
    }
}

// ---------------------------------------------------------------------------
// TokenBudgetAllocator
// ---------------------------------------------------------------------------

/// Token 预算分配器 —— 在角色间分配和管理 Token 配额。
///
/// 【领域含义】TokenBudgetAllocator 跟踪已分配和已使用的 Token，
/// 防止单个角色超出其配额。它为 Delegator 的 spawn_task 和 Coder
/// 的 ReAct 循环提供 Token 预算上限。
///
/// # 使用示例
///
/// ```rust,ignore
/// let allocator = TokenBudgetAllocator::new(64000);
/// let coder_budget = allocator.request(TokenRole::Coder, "auth-module").unwrap();
/// // coder_budget == 16000 (25% of 64000)
/// allocator.release(TokenRole::Coder, "auth-module");
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenBudgetAllocator {
    /// 预算配置
    pub config: TokenBudgetConfig,
    /// 角色 → 已分配的 Token 总额
    allocated: HashMap<TokenRole, usize>,
    /// 角色 → 活跃分配记录数
    active_allocations: HashMap<TokenRole, usize>,
    /// 分配记录 ID → 分配详情
    allocations: HashMap<String, BudgetAllocation>,
    /// 计数器（用于生成唯一 ID）
    counter: u64,
}

impl TokenBudgetAllocator {
    /// 使用默认配置创建分配器。
    pub fn new(total_budget: usize) -> Self {
        let mut config = TokenBudgetConfig::default();
        config.total_budget = total_budget;
        Self {
            config,
            allocated: HashMap::new(),
            active_allocations: HashMap::new(),
            allocations: HashMap::new(),
            counter: 0,
        }
    }

    /// 使用自定义配置创建分配器。
    pub fn with_config(config: TokenBudgetConfig) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            config,
            allocated: HashMap::new(),
            active_allocations: HashMap::new(),
            allocations: HashMap::new(),
            counter: 0,
        })
    }

    /// 请求分配指定角色的 Token 预算。
    ///
    /// 返回该角色的预算上限。如果已超出总预算则返回 None。
    /// 多次请求同一角色会累积 allocated 计数（用于追踪活跃 Coder 数量）。
    pub fn request(&mut self, role: TokenRole, _label: &str) -> Option<BudgetAllocation> {
        let budget = self.config.role_budget(&role);
        let current_allocated = self.allocated.get(&role).copied().unwrap_or(0);

        // Check total budget: ensure we don't exceed
        let total_allocated: usize = self.allocated.values().sum();
        if total_allocated + budget > self.config.total_budget {
            return None;
        }

        // Update counts
        *self.allocated.entry(role.clone()).or_insert(0) += budget;
        *self.active_allocations.entry(role.clone()).or_insert(0) += 1;

        self.counter += 1;
        let alloc_id = format!("alloc-{}", self.counter);

        let role_used = current_allocated + budget;
        let role_remaining = budget.saturating_sub(role_used);
        let total_remaining = self
            .config
            .total_budget
            .saturating_sub(total_allocated + budget);

        let allocation = BudgetAllocation {
            id: alloc_id.clone(),
            role: role.clone(),
            granted: budget,
            role_used,
            role_remaining,
            total_remaining,
        };

        self.allocations.insert(alloc_id, allocation.clone());

        Some(allocation)
    }

    /// 释放指定角色的 Token 分配（Coder 完成时调用）。
    pub fn release(&mut self, role: &TokenRole, _label: &str) {
        let budget = self.config.role_budget(role);
        if let Some(allocated) = self.allocated.get_mut(role) {
            *allocated = allocated.saturating_sub(budget);
        }
        if let Some(active) = self.active_allocations.get_mut(role) {
            *active = active.saturating_sub(1);
        }
    }

    /// 获取指定角色当前已分配的 Token 总额。
    pub fn allocated(&self, role: &TokenRole) -> usize {
        self.allocated.get(role).copied().unwrap_or(0)
    }

    /// 获取指定角色的预算上限。
    pub fn budget(&self, role: &TokenRole) -> usize {
        self.config.role_budget(role)
    }

    /// 获取总已分配 Token 数量。
    pub fn total_allocated(&self) -> usize {
        self.allocated.values().sum()
    }

    /// 获取总剩余可用 Token 数量。
    pub fn total_remaining(&self) -> usize {
        self.config.total_budget.saturating_sub(self.total_allocated())
    }

    /// 获取活跃分配数量（当前活跃的 Coder 数等）。
    pub fn active_count(&self, role: &TokenRole) -> usize {
        self.active_allocations.get(role).copied().unwrap_or(0)
    }

    /// 获取指定角色的所有分配记录。
    pub fn allocations_for(&self, role: &TokenRole) -> Vec<&BudgetAllocation> {
        self.allocations
            .values()
            .filter(|a| &a.role == role)
            .collect()
    }

    /// 获取当前总预算。
    pub fn total_budget(&self) -> usize {
        self.config.total_budget
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_sums_to_one() {
        let config = TokenBudgetConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_invalid_config_errors() {
        let config = TokenBudgetConfig {
            planner_ratio: 0.5,
            delegator_ratio: 0.5,
            coder_ratio: 0.5,
            buffer_ratio: 0.0,
            system_ratio: 0.0,
            ..TokenBudgetConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_allocator_creation() {
        let alloc = TokenBudgetAllocator::new(64000);
        assert_eq!(alloc.total_budget(), 64000);
        assert_eq!(alloc.total_allocated(), 0);
    }

    #[test]
    fn test_request_coder_budget() {
        let mut alloc = TokenBudgetAllocator::new(64000);
        let result = alloc.request(TokenRole::Coder, "user-auth");
        assert!(result.is_some());
        let allocation = result.unwrap();
        // Coder ratio = 0.375 → 64000 * 0.375 = 24000
        assert_eq!(allocation.granted, 24000);
        assert_eq!(alloc.total_allocated(), 24000);
    }

    #[test]
    fn test_request_planner_budget() {
        let mut alloc = TokenBudgetAllocator::new(64000);
        let result = alloc.request(TokenRole::Planner, "plan-phase-1");
        assert!(result.is_some());
        assert_eq!(result.unwrap().granted, 8000); // 64000 * 0.125
    }

    #[test]
    fn test_multiple_coders_fit_in_budget() {
        let mut alloc = TokenBudgetAllocator::new(64000);
        // Coder ratio = 0.375 → 24000 each (but 2 coders = 48000 > 24000? No — each
        // gets the full role budget until the budget floor is hit)
        // Just check that requesting doesn't exceed total
        let c1 = alloc.request(TokenRole::Coder, "c1").unwrap();
        let c2 = alloc.request(TokenRole::Coder, "c2").unwrap();
        assert_eq!(c1.granted, 24000);
        assert_eq!(c2.granted, 24000);
        assert_eq!(alloc.total_allocated(), 48000);
        assert!(alloc.total_remaining() > 0);
    }

    #[test]
    fn test_release_frees_budget() {
        let mut alloc = TokenBudgetAllocator::new(64000);
        alloc.request(TokenRole::Coder, "t1").unwrap();
        assert_eq!(alloc.total_allocated(), 24000);

        alloc.release(&TokenRole::Coder, "t1");
        assert_eq!(alloc.total_allocated(), 0);
    }

    #[test]
    fn test_budget_limit_respected() {
        let mut alloc = TokenBudgetAllocator::new(16000);
        let result = alloc.request(TokenRole::Coder, "big-task");
        // 16000 * 0.25 = 4000 — should succeed
        assert!(result.is_some());
        let result2 = alloc.request(TokenRole::Coder, "another-task");
        // 4000 + 4000 = 8000 <= 16000 — should succeed
        assert!(result2.is_some());
    }

    #[test]
    fn test_role_budget_calculation() {
        let config = TokenBudgetConfig::default();
        assert_eq!(config.role_budget(&TokenRole::Planner), 8000); // 64000 * 0.125
        assert_eq!(config.role_budget(&TokenRole::Coder), 24000); // 64000 * 0.375
        assert_eq!(config.role_budget(&TokenRole::System), 8000); // 64000 * 0.125
    }

    #[test]
    fn test_active_count_tracking() {
        let mut alloc = TokenBudgetAllocator::new(64000);
        assert_eq!(alloc.active_count(&TokenRole::Coder), 0);

        alloc.request(TokenRole::Coder, "c1").unwrap();
        assert_eq!(alloc.active_count(&TokenRole::Coder), 1);

        alloc.request(TokenRole::Coder, "c2").unwrap();
        assert_eq!(alloc.active_count(&TokenRole::Coder), 2);

        alloc.release(&TokenRole::Coder, "c1");
        assert_eq!(alloc.active_count(&TokenRole::Coder), 1);
    }

    #[test]
    fn test_request_none_when_over_budget() {
        let mut alloc = TokenBudgetAllocator::new(1000);
        // Set total budget very low
        alloc.config.total_budget = 1000;
        alloc.config.coder_ratio = 0.6; // 600 per coder

        let r1 = alloc.request(TokenRole::Coder, "c1");
        assert!(r1.is_some()); // 600 <= 1000

        let r2 = alloc.request(TokenRole::Coder, "c2");
        // 600 + 600 = 1200 > 1000
        assert!(r2.is_none());
    }

    #[test]
    fn test_allocations_for_role() {
        let mut alloc = TokenBudgetAllocator::new(64000);
        alloc.request(TokenRole::Coder, "task-1").unwrap();
        alloc.request(TokenRole::Planner, "plan-1").unwrap();
        alloc.request(TokenRole::Coder, "task-2").unwrap();

        let coder_allocs = alloc.allocations_for(&TokenRole::Coder);
        assert_eq!(coder_allocs.len(), 2);

        let planner_allocs = alloc.allocations_for(&TokenRole::Planner);
        assert_eq!(planner_allocs.len(), 1);
    }

    #[test]
    fn test_custom_role_budget() {
        let mut alloc = TokenBudgetAllocator::new(64000);
        // Custom role falls back to coder_ratio (0.375)
        let result = alloc.request(TokenRole::Custom("reviewer".into()), "review");
        assert!(result.is_some());
        assert_eq!(result.unwrap().granted, 24000);
    }

    #[test]
    fn test_token_role_display() {
        assert_eq!(TokenRole::Planner.to_string(), "planner");
        assert_eq!(TokenRole::Coder.to_string(), "coder");
        assert_eq!(
            TokenRole::Custom("reviewer".into()).to_string(),
            "custom:reviewer"
        );
    }

    #[test]
    fn test_token_role_serde() {
        let role = TokenRole::Coder;
        let json = serde_json::to_string(&role).unwrap();
        assert_eq!(json, "\"coder\"");
        let deserialized: TokenRole = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, TokenRole::Coder);
    }

    #[test]
    fn test_budget_allocation_serde() {
        let mut alloc = TokenBudgetAllocator::new(64000);
        let result = alloc.request(TokenRole::Coder, "test").unwrap();
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: BudgetAllocation = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, result.id);
        assert_eq!(deserialized.granted, result.granted);
    }
}
