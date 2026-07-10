//! HumanEval benchmark adapter.
//!
//! HumanEval is a dataset of 164 hand-written Python programming problems.
//! Each problem consists of a function signature, docstring, and a set of
//! `assert`-based test cases. The agent must generate the function body,
//! and evaluation runs the test cases to determine correctness.
//!
//! ## Dataset format
//!
//! The dataset is a JSONL file where each line is a JSON object:
//!
//! ```json
//! {
//!   "task_id": "HumanEval/0",
//!   "prompt": "from typing import List\n\ndef has_close_elements(...",
//!   "canonical_solution": "    ...\n",
//!   "test": "def check(candidate):\n    assert ...\n",
//!   "entry_point": "has_close_elements"
//! }
//! ```
//!
//! ## pass@k metric
//!
//! For each problem we generate *k* candidate solutions and count how many
//! pass all tests. The unbiased estimator is:
//!
//! ```text
//! pass@k = 1 - C(n-c, k) / C(n, k)
//! ```
//!
//! where *n* = total samples and *c* = correct samples.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{AdapterError, AgentOutput, BenchmarkAdapter};
use crate::types::{EvalResult, EvalStatus, EvalTask};

// ---------------------------------------------------------------------------
// HumanEval dataset types
// ---------------------------------------------------------------------------

/// HumanEval 问题实例
///
/// 【领域含义】HumanEval 数据集中单个编程问题的值对象，描述一个手写 Python 编程问题。
/// 【核心职责】封装函数签名、测试用例和入口函数名，供适配器转换为标准 EvalTask。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanEvalProblem {
    /// 任务标识 — 如 "HumanEval/0"。
    pub task_id: String,
    /// 提示 — 函数签名 + 文档字符串（给 Agent 的提示）。
    pub prompt: String,
    /// 参考解答 — 标准答案（用于调试，不用于评估）。
    #[serde(default)]
    pub canonical_solution: String,
    /// 测试用例 — Python 字符串形式的测试代码，通常为 `def check(candidate):` 函数。
    pub test: String,
    /// 入口函数名 — Agent 需要实现的函数名称。
    pub entry_point: String,
}

/// Scaffolded test harness wrapping a problem's test cases.
///
/// The harness:
/// 1. Reads the agent's generated code from a file.
/// 2. Imports the `candidate` function from that file.
/// 3. Runs the problem's `check(candidate)` test function.
#[derive(Debug, Clone)]
struct TestHarness {
    /// Full Python script that runs the test.
    script: String,
    /// Path to a temp file where the agent's code is written.
    code_path: PathBuf,
}

impl TestHarness {
    /// Generate a test harness for a given problem.
    ///
    /// The agent's generated code is written to a separate file (module).
    /// The harness imports that module and runs the test.
    fn build(problem: &HumanEvalProblem, work_dir: &Path) -> Self {
        let code_path = work_dir.join(format!("solution_{}.py", sanitize_filename(&problem.entry_point)));

        let escaped_code_path = escape_for_python_string(&code_path.display().to_string());
        let script = format!(
            r#"import sys
import importlib.util
import traceback

# Load the candidate solution from the generated file.
spec = importlib.util.spec_from_file_location(
    "candidate_module",
    "{escaped_code_path}"
)
module = importlib.util.module_from_spec(spec)

try:
    spec.loader.exec_module(module)
except Exception as e:
    print(f"IMPORT_ERROR: {{e}}", file=sys.stderr)
    traceback.print_exc(file=sys.stderr)
    sys.exit(2)

{entry_point} = getattr(module, "{entry_point}", None)
if {entry_point} is None:
    print(f"MISSING_ENTRY_POINT: {{repr(\"{entry_point}\")}}", file=sys.stderr)
    sys.exit(3)

# ---- test cases ----
{test_code}

# Run the checker.
try:
    check({entry_point})
    print("ALL_TESTS_PASSED")
    sys.exit(0)
except AssertionError as e:
    print(f"TEST_FAILED: {{e}}", file=sys.stderr)
    traceback.print_exc(file=sys.stderr)
    sys.exit(1)
except Exception as e:
    print(f"RUNTIME_ERROR: {{e}}", file=sys.stderr)
    traceback.print_exc(file=sys.stderr)
    sys.exit(4)
"#,
            entry_point = problem.entry_point,
            test_code = problem.test,
        );

        Self { script, code_path }
    }
}

// ---------------------------------------------------------------------------
// HumanEvalAdapter
// ---------------------------------------------------------------------------

/// HumanEval 适配器
///
/// 【领域含义】HumanEval 基准测试的适配器实现，将 HumanEval 原生格式转换为标准评估流程。
/// 【核心职责】加载数据集、生成 EvalTask、评估 Agent 生成的代码。
/// 内部缓存已解析的问题，供 process_result 按 task_id 查找原始测试用例。
///
/// ## 使用示例
///
/// ```no_run
/// use std::path::Path;
/// use code_agent_eval::adapters::human_eval::HumanEvalAdapter;
/// use code_agent_eval::BenchmarkAdapter;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let adapter = HumanEvalAdapter::new(Path::new("HumanEval.jsonl"));
/// let tasks = adapter.load_tasks().await?;
/// // Feed tasks to EvalRunner...
/// # Ok(())
/// # }
/// ```
pub struct HumanEvalAdapter {
    /// 数据集文件路径 — HumanEval JSONL 文件路径。
    data_path: PathBuf,
    /// 已解析的问题缓存，以 task_id 为键。
    /// 由 load_tasks 填充，供 process_result 使用。
    problems: RwLock<BTreeMap<String, HumanEvalProblem>>,
}

impl HumanEvalAdapter {
    /// 创建适配器
    ///
    /// 【领域含义】构造 HumanEval 适配器实例，指向给定的 JSONL 数据集文件。
    /// 【核心职责】指定数据集文件路径，初始化问题缓存。
    pub fn new(data_path: &Path) -> Self {
        Self {
            data_path: data_path.to_path_buf(),
            problems: RwLock::new(BTreeMap::new()),
        }
    }

    /// 解析所有问题
    ///
    /// 【领域含义】从 JSONL 文件中解析所有 HumanEval 问题（不填充内部缓存）。
    /// 【核心职责】逐行读取 JSONL 文件，反序列化为 HumanEvalProblem。
    pub fn parse_problems(&self) -> Result<Vec<HumanEvalProblem>, AdapterError> {
        let content = std::fs::read_to_string(&self.data_path)
            .map_err(|e| AdapterError::DatasetLoad(format!("Cannot read {}: {e}", self.data_path.display())))?;

        let mut problems = Vec::new();
        for (i, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let problem: HumanEvalProblem = serde_json::from_str(trimmed)
                .map_err(|e| AdapterError::DatasetLoad(format!("Line {}: {e}", i + 1)))?;
            problems.push(problem);
        }
        Ok(problems)
    }

    /// 将 HumanEvalProblem 转换为 EvalTask
    ///
    /// 【领域含义】将 HumanEval 原生问题转换为标准评估任务。
    /// 【核心职责】生成包含测试代码的 Python 脚本命令，供 EvalRunner 在沙箱中执行。
    pub fn problem_to_task(problem: &HumanEvalProblem) -> EvalTask {
        // The test command will execute a Python script that:
        // 1. Reads the agent's generated code from a predetermined path.
        // 2. Runs the `check(candidate)` function.
        let test_script = format!(
            r#"import sys, traceback, importlib.util
spec = importlib.util.spec_from_file_location("candidate_module", "/tmp/candidate.py")
module = importlib.util.module_from_spec(spec)
try:
    spec.loader.exec_module(module)
except Exception as e:
    print(f"IMPORT_ERROR: {{e}}", file=sys.stderr)
    sys.exit(2)
candidate = getattr(module, "{entry_point}", None)
if candidate is None:
    sys.exit(3)
{test_code}
try:
    check(candidate)
    print("PASS")
    sys.exit(0)
except AssertionError:
    sys.exit(1)
"#,
            entry_point = problem.entry_point,
            test_code = problem.test,
        );

        EvalTask {
            task_id: problem.task_id.clone(),
            setup_commands: vec![format!(
                "echo '{}' > /tmp/candidate.py",
                "# Agent-generated code will be placed here by the runner"
            )],
            test_commands: vec![format!("python3 -c '{escaped}'", escaped = escape_single_quoted(&test_script))],
            expected_output: None,
            timeout_secs: 30,
            repository: None,
            base_commit: None,
        }
    }

    /// 计算无偏 pass@k 估计值
    ///
    /// 【领域含义】HumanEval 论文中定义的无偏 pass@k 估计器，衡量 Agent 在 k 次采样中的通过概率。
    /// 【核心职责】根据 Chen et al. (2021) 的公式计算 pass@k。
    ///
    /// # 公式
    /// ```text
    /// pass@k = 1 - C(n-c, k) / C(n, k)
    /// ```
    /// 其中 c = 通过样本数。
    /// 如果 n - c < k（失败样本不足以填满 k），则 pass@k = 1.0。
    pub fn compute_pass_at_k(results: &[EvalResult], k: usize) -> f64 {
        let n = results.len();
        if n == 0 || k == 0 || k > n {
            return 0.0;
        }

        let c = results.iter().filter(|r| r.status.is_pass()).count();
        let failures = n.saturating_sub(c);

        if failures < k {
            return 1.0;
        }

        // pass@k = 1 - C(n-c, k) / C(n, k)
        1.0 - (combinations(failures, k) / combinations(n, k))
    }
}

// ---------------------------------------------------------------------------
// BenchmarkAdapter impl
// ---------------------------------------------------------------------------

#[async_trait]
impl BenchmarkAdapter for HumanEvalAdapter {
    fn name(&self) -> &str {
        "human_eval"
    }

    async fn load_tasks(&self) -> Result<Vec<EvalTask>, AdapterError> {
        let problems = self.parse_problems()?;

        // Populate the cache for later use in process_result.
        {
            let mut cache = self.problems.write().expect("problems lock poisoned");
            cache.clear();
            for p in &problems {
                cache.insert(p.task_id.clone(), p.clone());
            }
        }

        let tasks: Vec<EvalTask> = problems.iter().map(Self::problem_to_task).collect();
        Ok(tasks)
    }

    async fn process_result(
        &self,
        task: &EvalTask,
        output: &AgentOutput,
    ) -> Result<EvalResult, AdapterError> {
        // Look up the original problem to get test code and entry point.
        let problems = self.problems.read().expect("problems lock poisoned");
        let problem = problems
            .get(&task.task_id)
            .ok_or_else(|| AdapterError::TaskNotFound(task.task_id.clone()))?;

        // Extract generated code from the agent output.
        let code = output
            .code
            .as_ref()
            .or(if !output.content.is_empty() {
                Some(&output.content)
            } else {
                None
            })
            .ok_or_else(|| AdapterError::Parse("No code found in agent output".into()))?;

        // Write code and test harness to a temp directory, then run.
        let temp_dir = tempfile::tempdir()?;
        let harness = TestHarness::build(problem, temp_dir.path());

        std::fs::write(&harness.code_path, code)?;
        let script_path = temp_dir.path().join("test_harness.py");
        std::fs::write(&script_path, &harness.script)?;

        let result = std::process::Command::new("python3")
            .arg(&script_path)
            .output();

        let (status, logs) = match result {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let mut log_lines: Vec<String> = Vec::new();

                if !stdout.is_empty() {
                    log_lines.extend(stdout.lines().map(|l| l.to_string()));
                }
                if !stderr.is_empty() {
                    log_lines.extend(stderr.lines().map(|l| format!("[stderr] {l}")));
                }

                let eval_status = if out.status.success() && stdout.contains("ALL_TESTS_PASSED") {
                    EvalStatus::Pass
                } else if out.status.code() == Some(1) {
                    EvalStatus::Fail
                } else {
                    EvalStatus::Error
                };

                (eval_status, log_lines)
            }
            Err(e) => (EvalStatus::Error, vec![format!("Failed to run python3: {e}")]),
        };

        Ok(EvalResult {
            task_id: task.task_id.clone(),
            status,
            score: Some(if status.is_pass() { 1.0 } else { 0.0 }),
            logs,
            patch: None,
            metrics: Default::default(),
        })
    }
}

// ---------------------------------------------------------------------------
// pass@k computation helpers
// ---------------------------------------------------------------------------

/// Compute the binomial coefficient C(n, k) using the multiplicative formula
/// to avoid intermediate overflow.
fn combinations(n: usize, k: usize) -> f64 {
    if k > n {
        return 0.0;
    }
    if k == 0 || k == n {
        return 1.0;
    }
    let k = k.min(n - k); // use symmetry for smaller k
    let mut result = 1.0f64;
    for i in 0..k {
        result *= (n - i) as f64;
        result /= (i + 1) as f64;
    }
    result
}

/// Escape a string for use inside single-quoted Python strings.
///
/// Python single-quoted strings do not support escape sequences except
/// for `\'`, so we replace any `'` with `'\''`.
fn escape_single_quoted(s: &str) -> String {
    s.replace('\'', r#"'\''"#)
}

/// Sanitize a string for use in a filename.
fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

/// Escape a string for embedding in a Python double-quoted string literal.
fn escape_for_python_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EvalStatus, EvalTask};

    /// A minimal HumanEval fixture — one line of JSONL.
    const FIXTURE_JSONL: &str = r#"{"task_id":"HumanEval/0","prompt":"def has_close_elements(numbers: List[float], threshold: float) -> bool:\n","canonical_solution":"    for idx, elem in enumerate(numbers):\n        for idx2, elem2 in enumerate(numbers):\n            if idx != idx2:\n                distance = abs(elem - elem2)\n                if distance < threshold:\n                    return True\n    return False\n","test":"def check(candidate):\n    assert candidate([1.0, 2.0, 3.0], 0.5) == False\n    assert candidate([1.0, 2.8, 3.0, 4.0, 5.0, 2.0], 0.3) == True\n","entry_point":"has_close_elements"}
"#;

    #[test]
    fn parse_single_problem_from_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl_path = tmp.path().join("test.jsonl");
        std::fs::write(&jsonl_path, FIXTURE_JSONL).unwrap();

        let adapter = HumanEvalAdapter::new(&jsonl_path);
        let problems = adapter.parse_problems().unwrap();

        assert_eq!(problems.len(), 1);
        let p = &problems[0];
        assert_eq!(p.task_id, "HumanEval/0");
        assert_eq!(p.entry_point, "has_close_elements");
        assert!(p.prompt.contains("def has_close_elements"));
        assert!(p.test.contains("def check(candidate)"));
        assert!(p.test.contains("assert candidate([1.0, 2.0, 3.0], 0.5) == False"));
    }

    #[test]
    fn parse_empty_lines_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl_path = tmp.path().join("test.jsonl");
        std::fs::write(
            &jsonl_path,
            format!("{FIXTURE_JSONL}\n\n{FIXTURE_JSONL}"),
        )
        .unwrap();

        let adapter = HumanEvalAdapter::new(&jsonl_path);
        let problems = adapter.parse_problems().unwrap();
        assert_eq!(problems.len(), 2, "empty lines should be skipped");
    }

    #[test]
    fn parse_invalid_jsonl_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl_path = tmp.path().join("bad.jsonl");
        std::fs::write(&jsonl_path, "not valid json\n").unwrap();

        let adapter = HumanEvalAdapter::new(&jsonl_path);
        let result = adapter.parse_problems();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Line 1"));
    }

    #[test]
    fn parse_missing_file_returns_error() {
        let adapter = HumanEvalAdapter::new(Path::new("/nonexistent/humaneval.jsonl"));
        let result = adapter.parse_problems();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Cannot read"));
    }

    #[test]
    fn problem_to_task_generates_valid_eval_task() {
        let problem = HumanEvalProblem {
            task_id: "HumanEval/0".into(),
            prompt: "def foo(x):\n".into(),
            canonical_solution: "    return x + 1\n".into(),
            test: "def check(candidate):\n    assert candidate(1) == 2\n".into(),
            entry_point: "foo".into(),
        };

        let task = HumanEvalAdapter::problem_to_task(&problem);

        assert_eq!(task.task_id, "HumanEval/0");
        assert!(!task.test_commands.is_empty(), "should have test commands");
        assert!(task.test_commands[0].contains("python3"), "should run python3");
        assert!(task.test_commands[0].contains("foo"), "should reference entry point");
        assert_eq!(task.timeout_secs, 30);
        assert!(task.repository.is_none());
        assert!(task.base_commit.is_none());
    }

    #[test]
    fn problem_to_task_embeds_test_code() {
        let problem = HumanEvalProblem {
            task_id: "HumanEval/42".into(),
            prompt: "def bar():\n".into(),
            canonical_solution: String::new(),
            test: "def check(candidate):\n    assert candidate() == 42\n".into(),
            entry_point: "bar".into(),
        };

        let task = HumanEvalAdapter::problem_to_task(&problem);
        let cmd = &task.test_commands[0];
        // The command should contain the entry point reference somewhere.
        assert!(cmd.contains("bar"), "test command should reference entry_point");
    }

    // ------------------------------------------------------------------
    // pass@k tests
    // ------------------------------------------------------------------

    fn make_result(task_id: &str, status: EvalStatus) -> EvalResult {
        EvalResult {
            task_id: task_id.into(),
            status,
            score: Some(if status.is_pass() { 1.0 } else { 0.0 }),
            logs: vec![],
            patch: None,
            metrics: Default::default(),
        }
    }

    #[test]
    fn pass_at_k_all_pass() {
        // n=10, c=10, k=1 → pass@1 = 1.0
        let results: Vec<_> = (0..10)
            .map(|i| make_result(&format!("p/{}", i), EvalStatus::Pass))
            .collect();
        let score = HumanEvalAdapter::compute_pass_at_k(&results, 1);
        assert!((score - 1.0).abs() < 1e-9, "all pass → 1.0, got {score}");
    }

    #[test]
    fn pass_at_k_all_fail() {
        // n=10, c=0, k=1 → pass@1 = 0.0
        let results: Vec<_> = (0..10)
            .map(|i| make_result(&format!("p/{}", i), EvalStatus::Fail))
            .collect();
        let score = HumanEvalAdapter::compute_pass_at_k(&results, 1);
        assert!((score - 0.0).abs() < 1e-9, "all fail → 0.0, got {score}");
    }

    #[test]
    fn pass_at_k_mixed() {
        // n=10, c=2, k=1 → pass@1 = 0.2
        let mut results: Vec<_> = vec![
            make_result("p0", EvalStatus::Pass),
            make_result("p1", EvalStatus::Pass),
        ];
        results.extend((2..10).map(|i| make_result(&format!("p{i}"), EvalStatus::Fail)));

        let score = HumanEvalAdapter::compute_pass_at_k(&results, 1);
        assert!((score - 0.2).abs() < 1e-9, "2/10 pass@1 → 0.2, got {score}");
    }

    #[test]
    fn pass_at_k_known_case() {
        // Classic example: n=100, c=30, k=10
        // pass@10 = 1 - C(70, 10) / C(100, 10) ≈ 0.973...
        let results: Vec<_> = (0..30)
            .map(|i| make_result(&format!("p{i}"), EvalStatus::Pass))
            .chain((30..100).map(|i| make_result(&format!("p{i}"), EvalStatus::Fail)))
            .collect();

        let score = HumanEvalAdapter::compute_pass_at_k(&results, 10);
        assert!(score > 0.9, "pass@10 with 30/100 should be > 0.9, got {score}");
        assert!(score < 1.0, "not all pass, should be < 1.0");
    }

    #[test]
    fn pass_at_k_empty_results() {
        let results: Vec<EvalResult> = vec![];
        assert_eq!(HumanEvalAdapter::compute_pass_at_k(&results, 1), 0.0);
    }

    #[test]
    fn pass_at_k_zero_k() {
        let results = vec![make_result("p", EvalStatus::Pass)];
        assert_eq!(HumanEvalAdapter::compute_pass_at_k(&results, 0), 0.0);
    }

    #[test]
    fn pass_at_k_k_larger_than_n() {
        let results = vec![make_result("p", EvalStatus::Pass)];
        assert_eq!(HumanEvalAdapter::compute_pass_at_k(&results, 5), 0.0);
    }

    #[test]
    fn pass_at_k_failures_less_than_k() {
        // n=5, c=4 (only 1 failure), k=3 → failures < k → return 1.0
        let results = vec![
            make_result("p0", EvalStatus::Pass),
            make_result("p1", EvalStatus::Pass),
            make_result("p2", EvalStatus::Pass),
            make_result("p3", EvalStatus::Pass),
            make_result("p4", EvalStatus::Fail),
        ];
        let score = HumanEvalAdapter::compute_pass_at_k(&results, 3);
        assert!((score - 1.0).abs() < 1e-9, "failures < k → 1.0, got {score}");
    }

    // ------------------------------------------------------------------
    // Test harness generation
    // ------------------------------------------------------------------

    #[test]
    fn test_harness_generates_valid_python_script() {
        let problem = HumanEvalProblem {
            task_id: "HumanEval/0".into(),
            prompt: String::new(),
            canonical_solution: String::new(),
            test: "def check(candidate):\n    assert candidate(1) == 2\n".into(),
            entry_point: "add_one".into(),
        };

        let tmp = tempfile::tempdir().unwrap();
        let harness = TestHarness::build(&problem, tmp.path());

        // Verify the script contains key elements.
        assert!(harness.script.contains("add_one"));
        assert!(harness.script.contains("def check(candidate)"));
        assert!(harness.script.contains("ALL_TESTS_PASSED"));
        assert!(harness.script.contains("importlib.util"));
    }

    #[test]
    fn test_harness_code_path_in_work_dir() {
        let problem = HumanEvalProblem {
            task_id: "H/0".into(),
            prompt: String::new(),
            canonical_solution: String::new(),
            test: "def check(c): pass\n".into(),
            entry_point: "f!@#n".to_string(), // special chars
        };

        let tmp = tempfile::tempdir().unwrap();
        let harness = TestHarness::build(&problem, tmp.path());

        // code_path should be inside work_dir.
        assert!(harness.code_path.starts_with(tmp.path()));
        // filename should be sanitized.
        let fname = harness.code_path.file_name().unwrap().to_str().unwrap();
        assert!(!fname.contains('!'));
        assert!(!fname.contains('@'));
        assert!(!fname.contains('#'));
    }

    // ------------------------------------------------------------------
    // Adapter integration (unit-level)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn adapter_name_is_human_eval() {
        let adapter = HumanEvalAdapter::new(Path::new("/tmp/dummy.jsonl"));
        assert_eq!(adapter.name(), "human_eval");
    }

    #[tokio::test]
    async fn process_result_no_code_returns_parse_error() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl_path = tmp.path().join("test.jsonl");
        std::fs::write(&jsonl_path, FIXTURE_JSONL).unwrap();

        let adapter = HumanEvalAdapter::new(&jsonl_path);
        // Must load tasks first to populate the cache.
        adapter.load_tasks().await.unwrap();

        let task = EvalTask {
            task_id: "HumanEval/0".into(),
            test_commands: vec!["echo ok".into()],
            ..Default::default()
        };
        let output = AgentOutput::default();

        let result = adapter.process_result(&task, &output).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No code"));
    }

    #[tokio::test]
    async fn process_result_task_not_found_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl_path = tmp.path().join("test.jsonl");
        std::fs::write(&jsonl_path, FIXTURE_JSONL).unwrap();

        let adapter = HumanEvalAdapter::new(&jsonl_path);
        adapter.load_tasks().await.unwrap();

        let task = EvalTask {
            task_id: "HumanEval/999".into(), // not in cache
            test_commands: vec!["echo ok".into()],
            ..Default::default()
        };
        let output = AgentOutput::from_code("def foo(): pass");

        let result = adapter.process_result(&task, &output).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Task not found"));
    }

    // ------------------------------------------------------------------
    // Combinations helper
    // ------------------------------------------------------------------

    #[test]
    fn combinations_small_values() {
        assert!((combinations(5, 2) - 10.0).abs() < 1e-9);
        assert!((combinations(10, 3) - 120.0).abs() < 1e-9);
        assert!((combinations(10, 0) - 1.0).abs() < 1e-9);
        assert!((combinations(10, 10) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn combinations_k_larger_than_n() {
        assert_eq!(combinations(3, 5), 0.0);
    }

    #[test]
    fn combinations_symmetry() {
        assert!((combinations(10, 3) - combinations(10, 7)).abs() < 1e-9);
    }
}
