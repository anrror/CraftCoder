//! `code-agent-eval` — Rust-side eval orchestrator.
//!
//! Delegates to `code-agent-eval-py` via JSON-over-subprocess (the Python
//! CLI accepts a JSON task file and outputs a JSON result array).
//!
//! ## Modules
//!
//! | Module     | Purpose                                              |
//! |------------|------------------------------------------------------|
//! | `types`    | [`EvalTask`], [`EvalResult`], [`EvalStatus`], [`EvalMetrics`] |
//! | `sandbox`  | Docker availability checks via [`DockerSandbox`]     |
//! | `runner`   | [`EvalRunner`] orchestrator (Python subprocess)      |
//! | `adapters` | Benchmark adapters (HumanEval, SWE-bench-Live)       |
//! | `metrics`  | [`MetricsCalculator`], [`ReportGenerator`], regression detection |

pub mod adapters;
pub mod metrics;
pub mod runner;
pub mod sandbox;
pub mod types;

pub use adapters::{AdapterError, AgentOutput, BenchmarkAdapter};
pub use metrics::{BenchmarkReport, MetricsCalculator, TaskResult};
pub use metrics::regression::{RegressionDetector, RegressionSeverity, RegressionSignal};
pub use metrics::report::ReportGenerator;
pub use runner::{EvalError, EvalRunner};
pub use sandbox::{check_docker, DockerSandbox};
pub use types::{EvalMetrics, EvalResult, EvalStatus, EvalTask};
