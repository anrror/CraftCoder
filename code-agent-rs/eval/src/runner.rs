//! EvalRunner — Rust orchestrator that delegates to the Python eval package.
//!
//! Communication model:
//! 1. Serialise [`EvalTask`]s to a temporary JSON file.
//! 2. Spawn `python -m code_agent_eval.cli run <tempfile.json>`.
//! 3. Read JSON result array from stdout.
//! 4. Deserialise into [`EvalResult`]s.
//!
//! If Python is not available, every task returns [`EvalStatus::Error`].

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;

use thiserror::Error;
use tokio::process::Command as TokioCommand;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

use crate::sandbox::check_docker;
use crate::types::{EvalResult, EvalStatus, EvalTask};

/// 评估运行器错误
///
/// 【领域含义】表示 EvalRunner 操作过程中可能发生的领域错误。
/// 【核心职责】封装 Python 子进程调用、JSON 序列化、I/O 等各类故障场景。
#[derive(Debug, Error)]
pub enum EvalError {
    /// Python 可执行文件未找到
    #[error("Python executable not found: {0}")]
    PythonNotFound(String),

    /// Python 评估脚本在预期路径未找到
    #[error("Python eval script not found at: {0}")]
    ScriptNotFound(PathBuf),

    /// Python 子进程执行失败（非零退出或 I/O 错误）
    #[error("Python subprocess failed: {0}")]
    SubprocessError(String),

    /// JSON 序列化/反序列化失败
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// I/O 错误（读写临时文件等）
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Minimal JSON shape returned by the Python CLI.
///
/// The Python CLI emits a JSON array of objects. Each object must have at
/// least `task_id` and `status`. Other fields are optional and defaulted.
#[derive(Debug, serde::Deserialize)]
struct PythonResultRow {
    task_id: String,
    status: String,
    #[serde(default)]
    score: Option<f64>,
    #[serde(default)]
    logs: Option<String>,
    #[serde(default)]
    patch: Option<String>,
}

/// 评估运行器
///
/// 【领域含义】评估流程的核心编排器，通过 Python CLI 子进程执行评估任务。
/// 【核心职责】管理 Python 子进程生命周期、控制并发度、处理 JSON 序列化/反序列化。
///
/// ## 示例
///
/// ```no_run
/// use std::path::Path;
/// use code_agent_eval::{EvalRunner, EvalTask};
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let runner = EvalRunner::new(&Path::new("code-agent-eval-py").join("code_agent_eval").join("cli.py"));
/// let task = EvalTask {
///     task_id: "hello".into(),
///     test_commands: vec!["echo hello".into()],
///     ..Default::default()
/// };
/// let result = runner.run_task(task).await?;
/// # Ok(())
/// # }
/// ```
pub struct EvalRunner {
    /// Python CLI 脚本路径
    python_script: PathBuf,

    /// Docker 可用性（构造时缓存）
    docker_enabled: bool,

    /// 最大并发 Python 子进程数
    concurrency: usize,

    /// 并发控制信号量
    semaphore: Arc<Semaphore>,
}

impl EvalRunner {
    /// 创建运行器
    ///
    /// 【领域含义】构造 EvalRunner 实例，自动探测 Docker 可用性并设置并发度。
    /// 【核心职责】初始化 Python 脚本路径、检查 Docker、根据 CPU 核数设置并发上限。
    pub fn new(python_script: &Path) -> Self {
        let docker_enabled = check_docker();
        if !docker_enabled {
            warn!("Docker daemon not detected — eval tasks will fail");
        }
        let concurrency = num_cpus::get().max(1);
        Self {
            python_script: python_script.to_path_buf(),
            docker_enabled,
            concurrency,
            semaphore: Arc::new(Semaphore::new(concurrency)),
        }
    }

    /// 创建运行器（指定并发度）
    ///
    /// 【领域含义】构造 EvalRunner 实例，允许调用方显式指定并发度。
    /// 【核心职责】与 new() 相同，但使用传入的 concurrency 值而非自动检测。
    pub fn with_concurrency(python_script: &Path, concurrency: usize) -> Self {
        let docker_enabled = check_docker();
        if !docker_enabled {
            warn!("Docker daemon not detected — eval tasks will fail");
        }
        Self {
            python_script: python_script.to_path_buf(),
            docker_enabled,
            concurrency,
            semaphore: Arc::new(Semaphore::new(concurrency)),
        }
    }

    /// 查询 Docker 可用性
    ///
    /// 【领域含义】返回构造时缓存的 Docker 可用性检查结果。
    /// 【核心职责】供外部调用方判断是否可以使用 Docker 沙箱。
    pub fn docker_available(&self) -> bool {
        self.docker_enabled
    }

    /// 检查 Python 评估脚本是否存在
    ///
    /// 【领域含义】验证 Python CLI 脚本在磁盘上是否存在。
    /// 【核心职责】供前置检查使用，避免启动子进程时发现脚本缺失。
    pub fn python_script_exists(&self) -> bool {
        self.python_script.exists()
    }

    // ------------------------------------------------------------------
    // Single task
    // ------------------------------------------------------------------

    /// 运行单个评估任务
    ///
    /// 【领域含义】执行一个评估任务并返回其结果。
    /// 【核心职责】将单个任务包装为批量调用，委托给 execute_batch 执行。
    pub async fn run_task(&self, task: EvalTask) -> Result<EvalResult, EvalError> {
        let tasks = vec![task];
        let mut results = self.execute_batch(tasks).await?;
        Ok(results.pop().expect("batch always returns at least one result"))
    }

    // ------------------------------------------------------------------
    // Batch execution
    // ------------------------------------------------------------------

    /// 批量运行评估任务
    ///
    /// 【领域含义】并发执行多个评估任务，受 concurrency 限制。
    /// 【核心职责】将任务按并发度分块，每块通过一次 Python 子进程调用执行，结果按输入顺序返回。
    pub async fn run_batch(&self, tasks: Vec<EvalTask>) -> Result<Vec<EvalResult>, EvalError> {
        if tasks.is_empty() {
            return Ok(Vec::new());
        }

        let total = tasks.len();
        info!(count = total, "Starting batch eval");

        // Split into chunks of `concurrency` to avoid saturating the system.
        // Each chunk is run as a single Python invocation.
        let chunk_size = self.concurrency.max(1);
        let mut results: Vec<EvalResult> = Vec::with_capacity(total);

        for chunk in tasks.chunks(chunk_size) {
            let chunk_tasks = chunk.to_vec();
            let chunk_results = self.execute_batch(chunk_tasks).await?;
            results.extend(chunk_results);
        }

        info!(count = results.len(), "Batch eval complete");
        Ok(results)
    }

    // ------------------------------------------------------------------
    // Internal
    // ------------------------------------------------------------------

    /// Execute a batch of tasks in a single Python subprocess invocation.
    async fn execute_batch(&self, tasks: Vec<EvalTask>) -> Result<Vec<EvalResult>, EvalError> {
        let _permit = self.semaphore.acquire().await.expect("semaphore never closed");

        // Write tasks to a temporary JSON file.
        let mut temp_file = tempfile::NamedTempFile::new()?;
        let tasks_json = serde_json::to_vec(&tasks)?;
        temp_file.write_all(&tasks_json)?;
        temp_file.flush()?;

        let temp_path = temp_file.path().to_path_buf();
        debug!(path = %temp_path.display(), count = tasks.len(), "Wrote temp tasks file");

        let python_exe = find_python()?;

        let started = Instant::now();

        let work_dir = self
            .python_script
            .parent()
            .filter(|p| p.exists())
            .unwrap_or_else(|| Path::new("."));

        let output = TokioCommand::new(&python_exe)
            .arg("-m")
            .arg("code_agent_eval.cli")
            .arg("run")
            .arg(&temp_path)
            .current_dir(work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| EvalError::SubprocessError(format!("failed to spawn Python: {e}")))?;

        let elapsed = started.elapsed();

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!(stderr = %stderr, "Python subprocess failed");
            return Ok(tasks
                .iter()
                .map(|t| {
                    EvalResult::error(
                        &t.task_id,
                        vec![format!("Python interop error: {stderr}")],
                    )
                })
                .collect());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        debug!(stdout_len = stdout.len(), elapsed_ms = elapsed.as_millis() as u64, "Python subprocess complete");

        // Parse results — the Python CLI outputs a JSON array of result objects.
        let python_rows: Vec<PythonResultRow> =
            serde_json::from_str(&stdout).map_err(|e| {
                error!(raw_output = %stdout, json_error = %e, "Failed to parse Python output");
                EvalError::SubprocessError(format!(
                    "Failed to parse Python output as JSON: {e}"
                ))
            })?;

        // Build an index by task_id so we can return results in input order.
        let row_map: HashMap<&str, &PythonResultRow> =
            python_rows.iter().map(|r| (r.task_id.as_str(), r)).collect();

        let results: Vec<EvalResult> = tasks
            .iter()
            .map(|task| match row_map.get(task.task_id.as_str()) {
                Some(row) => row_to_eval_result(row),
                None => EvalResult::error(
                    &task.task_id,
                    vec!["Task not found in Python output".into()],
                ),
            })
            .collect();

        Ok(results)
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Find a viable Python interpreter.
///
/// Tries `python3` first (Unix convention), then `python`.
fn find_python() -> Result<String, EvalError> {
    for candidate in &["python3", "python"] {
        let output = std::process::Command::new(candidate)
            .arg("--version")
            .output();
        if let Ok(o) = output {
            if o.status.success() {
                return Ok(candidate.to_string());
            }
        }
    }
    Err(EvalError::PythonNotFound(
        "Neither 'python3' nor 'python' found on PATH".into(),
    ))
}

/// Convert a single row from Python's JSON output into an [`EvalResult`].
fn row_to_eval_result(row: &PythonResultRow) -> EvalResult {
    let status = match row.status.as_str() {
        "pass" => EvalStatus::Pass,
        "fail" => EvalStatus::Fail,
        _ => EvalStatus::Error,
    };

    let logs = row
        .logs
        .as_ref()
        .map(|s| s.lines().map(|l| l.to_string()).collect())
        .unwrap_or_default();

    EvalResult {
        task_id: row.task_id.clone(),
        status,
        score: row.score,
        logs,
        patch: row.patch.clone(),
        metrics: Default::default(),
    }
}

// ---------------------------------------------------------------------------
// num_cpus stub (avoid pulling whole crate for one fn)
// ---------------------------------------------------------------------------
mod num_cpus {
    use std::sync::OnceLock;

    pub fn get() -> usize {
        static COUNT: OnceLock<usize> = OnceLock::new();
        *COUNT.get_or_init(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(2)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // row_to_eval_result unit tests
    // ------------------------------------------------------------------

    #[test]
    fn row_pass_converts() {
        let row = PythonResultRow {
            task_id: "t1".into(),
            status: "pass".into(),
            score: Some(1.0),
            logs: Some("hello\nworld".into()),
            patch: None,
        };
        let r = row_to_eval_result(&row);
        assert_eq!(r.status, EvalStatus::Pass);
        assert_eq!(r.logs, vec!["hello", "world"]);
        assert_eq!(r.score, Some(1.0));
    }

    #[test]
    fn row_fail_converts() {
        let row = PythonResultRow {
            task_id: "t2".into(),
            status: "fail".into(),
            score: Some(0.0),
            logs: None,
            patch: None,
        };
        let r = row_to_eval_result(&row);
        assert_eq!(r.status, EvalStatus::Fail);
        assert!(r.logs.is_empty());
    }

    #[test]
    fn row_unknown_status_is_error() {
        let row = PythonResultRow {
            task_id: "t3".into(),
            status: "unknown".into(),
            score: None,
            logs: None,
            patch: None,
        };
        let r = row_to_eval_result(&row);
        assert_eq!(r.status, EvalStatus::Error);
    }

    // ------------------------------------------------------------------
    // EvalRunner::new / docker detection (unit)
    // ------------------------------------------------------------------

    #[test]
    fn runner_constructs_without_panic() {
        let runner = EvalRunner::new(Path::new("/nonexistent/cli.py"));
        // Docker may or may not be available — that's fine.
        let _ = runner.docker_available();
    }

    #[test]
    fn runner_with_concurrency() {
        let runner = EvalRunner::with_concurrency(Path::new("/nonexistent/cli.py"), 2);
        let _ = runner.docker_available();
    }

    // ------------------------------------------------------------------
    // Empty batch
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn empty_batch_returns_empty() {
        let runner = EvalRunner::new(Path::new("/nonexistent/cli.py"));
        let results = runner.run_batch(Vec::new()).await.expect("empty batch");
        assert!(results.is_empty());
    }
}
