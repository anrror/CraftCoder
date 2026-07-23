//! Plan 引擎 —— 结构化任务分解与 DAG 执行
//!
//! 【领域含义】Plan 引擎是 v2 架构的规划层核心，实现三级任务分解
//!（业务→功能→文件）和 DAG 依赖图驱动的并行执行。它使 Agent 从
//! 简单的 ReAct 循环升级为结构化、可治理的多步执行引擎。
//!
//! # Architecture
//!
//! ```text
//! User Request
//!      ↓
//! ┌─ DecompositionEngine ─────────────────┐
//! │  Level 1: 业务模块分解                 │
//! │  Level 2: 技术功能分解                 │
//! │  Level 3: 代码文件映射 + 依赖分析       │
//! │  ┌────────────────────────────────┐   │
//! │  │ KnowledgeProvider injects      │   │
//! │  │ rules/skills into prompt       │   │  ← Phase E
//! │  └────────────────────────────────┘   │
//! └──────────────┬────────────────────────┘
//!                ↓ TaskPlan (nodes[] + edges[])
//! ┌─ PlanExecutor ─────────────────────────┐
//! │  Kahn 拓扑排序 → parallel batches      │
//! │  for each batch:                      │
//! │    Delegator.spawn_task() × N         │
//! │    Delegator.wait_task() × N          │
//! │    RETRY / REPLAN 决策                 │
//! │    QualityGate (incl. KnowledgeGate)  │
//! └──────────────┬────────────────────────┘
//!                ↓ ExecutionReport
//! ```
//!
//! # Component Overview
//!
//! | Component | File | Description |
//! |-----------|------|-------------|
//! | `TaskPlan` / `PlanNode` | types.rs | DAG 数据模型 |
//! | `DynamicScheduler` | kahn.rs | 拓扑排序 + 运行时调度 |
//! | `Decomposer` trait | decomposition.rs | 分解策略接口 |
//! | `DecompositionEngine` | decomposition.rs | LLM 驱动分解 |
//! | `ManualDecomposer` | decomposition.rs | 手动构造（测试用） |
//! | `PlanExecutor` | executor.rs | DAG 感知的任务编排器 |
//! | `QualityGate` / `SpecGate` | quality.rs | Phase D 质量门禁系统 |
//! | `KnowledgeProvider` / `KnowledgeStore` | knowledge.rs | Phase E 知识体系 |
//! | `PlanStore` / `PlanExecutionRecord` | store.rs | Phase G 数据飞轮持久化 |

pub mod decomposition;
pub mod executor;
pub mod kahn;
pub mod knowledge;
pub mod quality;
pub mod store;
pub mod types;

pub use decomposition::{
    Decomposer, DecompositionEngine, DecompositionError, ManualDecomposer,
    make_web_project_plan,
};
pub use executor::{PlanError, PlanExecutor};
pub use kahn::{DynamicScheduler, InDegreeTracker, parallel_batches};
pub use knowledge::{
    EmptyKnowledgeProvider, KnowledgeConfig, KnowledgeError, KnowledgeGate,
    KnowledgeGateConfig, KnowledgeProvider, KnowledgeStore, Rule, RuleSeverity,
};
pub use quality::{
    GateVerdict, QualityGate, QualityGatePipeline, SpecConfig, SpecGate, NoopGate,
};
pub use store::{execution_report_to_record, PlanExecutionRecord, PlanNodeRecord, PlanStore};
pub use types::{
    ExecutionReport, NodeStatus, NodeSummary, PlanConfig, PlanNode, PlanProgressEvent, TaskPlan,
};
