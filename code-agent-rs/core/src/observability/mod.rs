//! 可观测性与结构化日志 —— OpenTelemetry 链路追踪与 JSON 日志输出
//!
//! 【领域含义】本模块为 Code Agent 提供统一的可观测性基础设施，包括
//! 会话级别链路追踪（Tracer）、结构化 JSON 日志（Logger）和事件系统
//! （Event）。三者共同构成 Agent 运行时的可观测性三支柱，支持调试、
//! 性能分析和生产环境监控。
//!
//! # 架构
//!
//! ```text
//! ┌──────────────────────────────────────────────────┐
//! │              Application Code                     │
//! │  (Session, ContextManager, ModelClient, Tools)    │
//! └────────────────────┬─────────────────────────────┘
//!                      │ uses
//!          ┌───────────▼──────────┐
//!          │       Tracer         │
//!          │  (session-scoped)    │
//!          └───────────┬──────────┘
//!                      │ emits
//!          ┌───────────▼──────────┐
//!          │   tracing Subscriber │
//!          │  (stdout + file +    │
//!          │   optional OTLP)     │
//!          └──────────────────────┘
//! ```
//!
//! # 快速开始
//!
//! ```rust,ignore
//! use code_agent_core::observability::{self, Tracer, ObservabilityConfig};
//! use code_agent_protocol::SessionId;
//!
//! // 1. 初始化日志（每个进程一次）
//! let _guard = observability::logger::setup_logging(&ObservabilityConfig::default());
//!
//! // 2. 为会话创建 Tracer
//! let tracer = Tracer::new(SessionId::from("my-session"));
//!
//! // 3. 追踪一次交互
//! let turn = tracer.start_turn("turn-1");
//! let _enter = turn.enter();
//! // ... 运行 Agent 逻辑 ...
//! tracer.turn_completed("turn-1", false, 1);
//! ```
//!
//! # 特性标志
//!
//! - observability（默认）：启用完整追踪管道。禁用后所有 Tracer 方法变为空操作。
//! - otlp（可选）：添加 OpenTelemetry OTLP Span 导出，用于生产环境可观测性平台
//!   （Jaeger、Honeycomb 等）。
//!
//! # Architecture (English)
//!
//! ```text
//! ┌──────────────────────────────────────────────────┐
//! │              Application Code                     │
//! │  (Session, ContextManager, ModelClient, Tools)    │
//! └────────────────────┬─────────────────────────────┘
//!                      │ uses
//!          ┌───────────▼──────────┐
//!          │       Tracer         │
//!          │  (session-scoped)    │
//!          └───────────┬──────────┘
//!                      │ emits
//!          ┌───────────▼──────────┐
//!          │   tracing Subscriber │
//!          │  (stdout + file +    │
//!          │   optional OTLP)     │
//!          └──────────────────────┘
//! ```
//!
//! # Feature Flags
//!
//! - observability (default): Enables the full tracing pipeline.
//!   When disabled, all Tracer methods become no-ops.
//! - otlp (optional): Adds OpenTelemetry OTLP span export for
//!   production observability platforms (Jaeger, Honeycomb, etc.).

pub mod config;
pub mod events;
pub mod logger;
pub mod tracer;

pub use config::ObservabilityConfig;
pub use events::{ObservabilityEvent, SessionMeta, event_names};
pub use tracer::{ModelRequestSpan, ToolCallSpan, Tracer};
