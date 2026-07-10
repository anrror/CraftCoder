//! PyO3 bridge — Rust → Python calls into `code_agent_eval`.
//!
//! ## Public API
//!
//! | Function        | Input               | Output                  |
//! |-----------------|---------------------|-------------------------|
//! | `run_eval`      | EvalTask JSON        | EvalResult JSON         |
//! | `run_benchmark` | [EvalTask, …] JSON   | [EvalResult, …] JSON    |
//!
//! Both functions are **blocking** and acquire the Python GIL internally.
//!
//! ## Usage (from code-agent-rs)
//!
//! ```ignore
//! use code_agent_eval_bridge::{run_eval, run_benchmark};
//!
//! let result = run_eval(r#"{"task_id":"t","test_commands":["echo ok"]}"#)?;
//! println!("{result}");
//! ```

use pyo3::prelude::*;
use pyo3::types::PyModule;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------
mod error;
pub use error::BridgeError;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run a single eval task.
///
/// `task_json` must be a valid **EvalTask** JSON string (see
/// `code_agent_eval.models.EvalTask`).
///
/// Returns an **EvalResult** JSON string or a :class:`BridgeError`.
pub fn run_eval(task_json: &str) -> Result<String, BridgeError> {
    let module = import_eval_module()?;
    let result: String = Python::with_gil(|py| {
        let run_single = module.getattr(py, "run_single_py")?;
        run_single.call1(py, (task_json,))?.extract(py)
    })?;
    Ok(result)
}

/// Run a batch of eval tasks in parallel (via Python's ThreadPoolExecutor).
///
/// `tasks_json` must be a JSON **array** of EvalTask objects.
///
/// Returns a JSON array of EvalResult objects or a :class:`BridgeError`.
pub fn run_benchmark(tasks_json: &str) -> Result<String, BridgeError> {
    let module = import_eval_module()?;
    let result: String = Python::with_gil(|py| {
        let run_batch = module.getattr(py, "run_batch_py")?;
        run_batch.call1(py, (tasks_json,))?.extract(py)
    })?;
    Ok(result)
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn import_eval_module() -> Result<Py<PyModule>, BridgeError> {
    Python::with_gil(|py| {
        let module = PyModule::import(py, "code_agent_eval.bridge")
            .map_err(|e| BridgeError::Import(format!("{e}")))?;
        Ok(module.into_py(py))
    })
}
