//! Code Agent Core —— 多模型供应商抽象与工具系统
//!
//! 【领域总览】本 crate 是 Coding Agent 的核心引擎，采用领域驱动设计（DDD）
//! 组织为多个限界上下文（Bounded Context）。每个模块对应一个独立的领域概念，
//! 通过 trait 接口解耦，支持多供应商替换。
//!
//! 【限界上下文映射】
//! - **agent（Agent 核心上下文）**: ReAct 推理循环、会话生命周期、线程管理、多智能体协作
//! - **context（上下文管理上下文）**: 5 层渐进式上下文压缩管道、Token 预算管理
//! - **model（模型抽象上下文）**: 供应商无关的 ModelClient trait + Qwen3 实现
//! - **tools（工具体系上下文）**: Tool trait 接口、注册中心、路由与权限控制
//! - **safety（安全防护上下文）**: 提示注入防御、内容安全审查、Qwen3Guard 集成
//! - **observability（可观测性上下文）**: OpenTelemetry 追踪、结构化 JSON 日志
//! - **feedback（反馈收集上下文）**: 用户反馈采集与分析
//! - **flywheel（飞轮分析上下文）**: 失败聚类、模式识别、改进建议
//! - **persistence（持久化上下文）**: 会话状态的保存、恢复与快照
//!
//! 【架构原则】
//! 1. 面向接口编程 —— Agent 核心只依赖 trait，不依赖具体实现
//! 2. 聚合根驱动 —— Session 是核心聚合根，管理完整的会话生命周期
//! 3. 流式优先 —— 所有模型交互采用流式（SSE）协议
//! 4. 安全内建 —— 安全审查嵌入工具执行和用户输入管道
//!
//! This crate provides:
//! - **agent**: ReAct agent loop with session, thread, and turn management
//! - **context**: 5-layer context compaction pipeline (token budget management)
//! - **model**: Provider-agnostic model client traits + Qwen3 implementations
//! - **observability**: OpenTelemetry tracing and structured JSON logging
//! - **safety**: Content safety and guard client
//! - **tools**: Built-in coding tools (read_file, write_file, etc.)

pub mod agent;
pub mod context;
pub mod feedback;
pub mod flywheel;
pub mod model;
#[cfg(feature = "observability")]
pub mod observability;
pub mod safety;
pub mod tools;
