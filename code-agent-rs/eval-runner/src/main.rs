//! `eval-runner` — CLI for the continuous evaluation pipeline.
//!
//! Exposes three commands to the CI workflow:
//! - `run <benchmark>` — execute a benchmark suite
//! - `compare` — compare two result files, generate report
//! - `check-regression` — detect performance regressions

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use serde::Serialize;

use code_agent_eval::{
    adapters::human_eval::HumanEvalAdapter,
    adapters::swe_bench::SWEBenchLiveAdapter,
    EvalResult, EvalRunner, EvalTask, MetricsCalculator,
    RegressionDetector, RegressionSeverity, ReportGenerator,
};

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "eval-runner", version, about = "Continuous evaluation runner for AI Coding Agent")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a benchmark suite.
    Run {
        /// Benchmark name: "humaneval" or "swe-bench-live"
        benchmark: String,
        /// Path to dataset file or cache directory
        #[arg(long)]
        dataset: Option<PathBuf>,
        /// Cache / output directory (default: .omo/eval-results)
        #[arg(long, default_value = ".omo/eval-results")]
        cache_dir: PathBuf,
        /// Number of tasks to run
        #[arg(long, default_value = "50")]
        tasks: usize,
        /// Dataset split (for SWE-bench: lite, verified, full)
        #[arg(long, default_value = "lite")]
        split: String,
        /// Output JSON file for results
        #[arg(long)]
        output: Option<PathBuf>,
        /// Path to Python eval CLI script
        #[arg(long)]
        python_script: Option<PathBuf>,
    },

    /// Compare two result files and generate a report.
    Compare {
        /// Path to baseline result JSON
        #[arg(long)]
        baseline: PathBuf,
        /// Path to current result JSON
        #[arg(long)]
        current: PathBuf,
        /// Output report file (default: eval-report.md)
        #[arg(long, default_value = "eval-report.md")]
        output: PathBuf,
        /// Output format: markdown, html, or json
        #[arg(long, default_value = "markdown")]
        format: String,
    },

    /// Check for performance regressions against a baseline.
    CheckRegression {
        /// Path to baseline result JSON
        #[arg(long)]
        baseline: PathBuf,
        /// Path to current result JSON
        #[arg(long)]
        current: PathBuf,
        /// Regression threshold in percentage points (default: 3.0)
        #[arg(long, default_value = "3.0")]
        threshold: f64,
    },
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "eval_runner=info".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Run {
            benchmark,
            dataset,
            cache_dir,
            tasks,
            split,
            output,
            python_script,
        } => cmd_run(benchmark, dataset, cache_dir, tasks, split, output, python_script),
        Command::Compare {
            baseline,
            current,
            output,
            format,
        } => cmd_compare(baseline, current, output, format),
        Command::CheckRegression {
            baseline,
            current,
            threshold,
        } => cmd_check_regression(baseline, current, threshold),
    }
}

// ---------------------------------------------------------------------------
// cmd_run
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn cmd_run(
    benchmark: String,
    dataset: Option<PathBuf>,
    cache_dir: PathBuf,
    tasks: usize,
    split: String,
    output: Option<PathBuf>,
    python_script: Option<PathBuf>,
) -> anyhow::Result<()> {
    tracing::info!(benchmark = %benchmark, tasks, split = %split, "Starting benchmark");

    match benchmark.as_str() {
        "humaneval" | "human_eval" | "HumanEval" => {
            run_humaneval_bench(dataset, &cache_dir, tasks, output, python_script)
        }
        "swe-bench-live" | "swe_bench_live" | "swb" => {
            run_swebench_bench(dataset, &cache_dir, tasks, &split, output, python_script)
        }
        other => anyhow::bail!(
            "Unknown benchmark '{other}'. Supported: humaneval, swe-bench-live"
        ),
    }
}

fn run_humaneval_bench(
    dataset: Option<PathBuf>,
    cache_dir: &Path,
    tasks: usize,
    output: Option<PathBuf>,
    python_script: Option<PathBuf>,
) -> anyhow::Result<()> {
    let dataset_path = dataset
        .ok_or_else(|| anyhow::anyhow!("--dataset PATH is required for HumanEval (JSONL file)"))?;

    if !dataset_path.exists() {
        anyhow::bail!("Dataset not found: {}", dataset_path.display());
    }

    let adapter = HumanEvalAdapter::new(&dataset_path);
    let problems = adapter
        .parse_problems()
        .map_err(|e| anyhow::anyhow!("Failed to parse HumanEval dataset: {e}"))?;

    let eval_tasks: Vec<EvalTask> = problems
        .iter()
        .take(tasks)
        .map(HumanEvalAdapter::problem_to_task)
        .collect();

    let total = eval_tasks.len();
    tracing::info!(count = total, "Loaded HumanEval tasks");

    let script = python_script
        .unwrap_or_else(|| PathBuf::from("code-agent-eval-py/code_agent_eval/cli.py"));

    let runner = EvalRunner::new(&script);
    let results = execute_tasks(&runner, eval_tasks)?;

    let calc = MetricsCalculator::default();
    let report = calc.compute_all_named(&results, "HumanEval");

    let out = output.unwrap_or_else(|| cache_dir.join("humaneval-results.json"));
    write_results_file(&results, &report, &out)
}

#[allow(clippy::too_many_arguments)]
fn run_swebench_bench(
    dataset: Option<PathBuf>,
    cache_dir: &Path,
    tasks: usize,
    split: &str,
    output: Option<PathBuf>,
    python_script: Option<PathBuf>,
) -> anyhow::Result<()> {
    let data_dir = dataset.unwrap_or_else(|| cache_dir.to_path_buf());

    let adapter = SWEBenchLiveAdapter::new(&data_dir, split);

    let rt = tokio::runtime::Runtime::new()?;
    let eval_tasks = rt
        .block_on(async { adapter.load_tasks(Some(tasks)).await })
        .map_err(|e| anyhow::anyhow!("Failed to load SWE-bench tasks: {e}"))?;

    let total = eval_tasks.len();
    tracing::info!(count = total, "Loaded SWE-bench-Live tasks");

    let script = python_script
        .unwrap_or_else(|| PathBuf::from("code-agent-eval-py/code_agent_eval/cli.py"));

    let runner = EvalRunner::new(&script);
    let results = execute_tasks(&runner, eval_tasks)?;

    let calc = MetricsCalculator::default();
    let report = calc.compute_all_named(&results, "SWE-bench-Live");

    let out = output.unwrap_or_else(|| cache_dir.join("swe-bench-results.json"));
    write_results_file(&results, &report, &out)
}

/// Run tasks through EvalRunner, degrading gracefully when Python is unavailable.
fn execute_tasks(runner: &EvalRunner, tasks: Vec<EvalTask>) -> anyhow::Result<Vec<EvalResult>> {
    let rt = tokio::runtime::Runtime::new()?;
    match rt.block_on(runner.run_batch(tasks)) {
        Ok(results) => Ok(results),
        Err(code_agent_eval::EvalError::PythonNotFound(msg)) => {
            tracing::warn!(
                "Python not available: {}. Returning empty results (CI comparison pipeline still works).",
                msg
            );
            Ok(Vec::new())
        }
        Err(e) => {
            tracing::error!("Eval runner failed: {e}");
            anyhow::bail!("Eval runner error: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// cmd_compare
// ---------------------------------------------------------------------------

fn cmd_compare(
    baseline_path: PathBuf,
    current_path: PathBuf,
    output_path: PathBuf,
    format: String,
) -> anyhow::Result<()> {
    let baseline_results = load_eval_results(&baseline_path)?;
    let current_results = load_eval_results(&current_path)?;

    let calc = MetricsCalculator::default();
    let baseline_report = calc.compute_all_named(&baseline_results, "Baseline");
    let current_report = calc.compute_all_named(&current_results, "Current");

    // Detect regressions
    let signals = RegressionDetector::detect(&current_report, &baseline_report);
    let has_regression = signals.iter().any(|s| s.is_regression);

    let content = match format.as_str() {
        "html" => ReportGenerator::to_html(&current_report, Some(&baseline_report)),
        "json" => ReportGenerator::to_json(&current_report),
        _ => build_markdown_report(&current_report, &baseline_report, &signals, has_regression),
    };

    std::fs::write(&output_path, &content)?;
    tracing::info!(path = %output_path.display(), "Report written");

    if has_regression {
        tracing::warn!("Regressions detected.");
    } else {
        tracing::info!("No regressions detected.");
    }

    Ok(())
}

fn build_markdown_report(
    current: &code_agent_eval::BenchmarkReport,
    baseline: &code_agent_eval::BenchmarkReport,
    signals: &[code_agent_eval::RegressionSignal],
    has_regression: bool,
) -> String {
    let mut md = ReportGenerator::to_markdown(current, Some(baseline));

    // Regression summary section
    md.push_str("\n## Regression Analysis\n\n");
    if has_regression {
        md.push_str(":warning: **Regressions detected.** See details below.\n\n");
    } else {
        md.push_str(":white_check_mark: **No regressions detected.**\n\n");
    }

    md.push_str("| Metric | Baseline | Current | Delta | Severity |\n");
    md.push_str("|--------|----------|---------|-------|----------|\n");

    for s in signals {
        let severity_label = match s.severity {
            RegressionSeverity::Critical => ":red_circle: CRITICAL",
            RegressionSeverity::Warning => ":warning: WARNING",
            RegressionSeverity::Info => ":information_source: INFO",
        };
        md.push_str(&format!(
            "| {} | {:.2} | {:.2} | {:+.2} | {} |\n",
            s.metric, s.before, s.after, s.delta, severity_label,
        ));
    }

    md
}

// ---------------------------------------------------------------------------
// cmd_check_regression
// ---------------------------------------------------------------------------

fn cmd_check_regression(
    baseline_path: PathBuf,
    current_path: PathBuf,
    threshold: f64,
) -> anyhow::Result<()> {
    let baseline_results = load_eval_results(&baseline_path)?;
    let current_results = load_eval_results(&current_path)?;

    let calc = MetricsCalculator::default();
    let baseline_report = calc.compute_all_named(&baseline_results, "Baseline");
    let current_report = calc.compute_all_named(&current_results, "Current");

    let signals = RegressionDetector::detect(&current_report, &baseline_report);

    let regressions: Vec<_> = signals.iter().filter(|s| s.is_regression).collect();

    for r in &regressions {
        let sev = match r.severity {
            RegressionSeverity::Critical => "CRITICAL",
            RegressionSeverity::Warning => "WARNING",
            RegressionSeverity::Info => "INFO",
        };
        tracing::error!(
            metric = %r.metric,
            before = r.before,
            after = r.after,
            delta = r.delta,
            severity = sev,
            "REGRESSION DETECTED"
        );
    }

    let critical = regressions
        .iter()
        .filter(|r| r.severity >= RegressionSeverity::Warning)
        .count();

    if critical > 0 {
        anyhow::bail!(
            "{} regression(s) detected (severity >= WARNING). Threshold: {}%. Failing CI.",
            critical,
            threshold,
        );
    }

    tracing::info!("All metrics within threshold — no regressions.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Serialization helpers
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ResultsFile {
    benchmark: String,
    total_tasks: usize,
    passed: usize,
    failed: usize,
    errored: usize,
    pass_at_1: f64,
    resolve_rate: f64,
    avg_tokens_per_task: f64,
    avg_turns_per_task: f64,
    total_cost_estimate: f64,
    per_task: Vec<code_agent_eval::TaskResult>,
    raw_results: Vec<EvalResult>,
}

fn write_results_file(
    results: &[EvalResult],
    report: &code_agent_eval::BenchmarkReport,
    output_path: &Path,
) -> anyhow::Result<()> {
    let file = ResultsFile {
        benchmark: report.benchmark.clone(),
        total_tasks: report.total_tasks,
        passed: report.passed,
        failed: report.failed,
        errored: report.errored,
        pass_at_1: report.pass_at_1,
        resolve_rate: report.resolve_rate,
        avg_tokens_per_task: report.avg_tokens_per_task,
        avg_turns_per_task: report.avg_turns_per_task,
        total_cost_estimate: report.total_cost_estimate,
        per_task: report.per_task.clone(),
        raw_results: results.to_vec(),
    };

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(&file)?;
    std::fs::write(output_path, &json)?;
    tracing::info!(path = %output_path.display(), "Results saved");

    Ok(())
}

/// Load eval results from JSON — supports the `ResultsFile` wrapper format
/// and raw `Vec<EvalResult>` arrays.
fn load_eval_results(path: &Path) -> anyhow::Result<Vec<EvalResult>> {
    let content =
        std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("Cannot read {}: {e}", path.display()))?;

    // Try ResultsFile wrapper
    if let Ok(wrapper) = serde_json::from_str::<serde_json::Value>(&content) {
        if let Some(raw) = wrapper.get("raw_results") {
            return serde_json::from_value(raw.clone())
                .map_err(|e| anyhow::anyhow!("Failed to parse raw_results from {}: {e}", path.display()));
        }
        // Fallback: reconstruct from per_task
        if let Some(per_task) = wrapper.get("per_task") {
            let task_results: Vec<code_agent_eval::TaskResult> =
                serde_json::from_value(per_task.clone())?;
            return Ok(task_results
                .into_iter()
                .map(|tr| EvalResult {
                    task_id: tr.task_id,
                    status: tr.status,
                    score: tr.score,
                    logs: Vec::new(),
                    patch: None,
                    metrics: code_agent_eval::EvalMetrics {
                        duration_ms: tr.duration_ms,
                        tokens_used: tr.tokens_used,
                        turns_taken: tr.turns_taken,
                    },
                })
                .collect());
        }
    }

    // Try raw array
    serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("Failed to parse eval results from {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use code_agent_eval::{EvalMetrics, EvalStatus};

    fn make_pass(task_id: &str, tokens: u32) -> EvalResult {
        EvalResult {
            task_id: task_id.into(),
            status: EvalStatus::Pass,
            score: Some(1.0),
            logs: vec![],
            patch: None,
            metrics: EvalMetrics {
                duration_ms: 100,
                tokens_used: tokens,
                turns_taken: 1,
            },
        }
    }

    fn make_fail(task_id: &str) -> EvalResult {
        EvalResult {
            task_id: task_id.into(),
            status: EvalStatus::Fail,
            score: Some(0.0),
            logs: vec![],
            patch: None,
            metrics: EvalMetrics::default(),
        }
    }

    #[test]
    fn write_and_load_results_roundtrip() {
        let results = vec![
            make_pass("t/0", 500),
            make_pass("t/1", 300),
            make_fail("t/2"),
        ];

        let calc = MetricsCalculator::default();
        let report = calc.compute_all_named(&results, "TestBench");

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("results.json");

        write_results_file(&results, &report, &path).unwrap();
        let loaded = load_eval_results(&path).unwrap();

        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].task_id, "t/0");
        assert_eq!(loaded[0].status, EvalStatus::Pass);
        assert_eq!(loaded[2].status, EvalStatus::Fail);
    }

    #[test]
    fn load_results_from_raw_array() {
        let results = vec![make_pass("only", 100)];
        let json = serde_json::to_string(&results).unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("raw.json");
        std::fs::write(&path, &json).unwrap();

        let loaded = load_eval_results(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].task_id, "only");
    }

    #[test]
    fn compare_with_identical_results() {
        let results = vec![
            make_pass("a", 100),
            make_pass("b", 200),
        ];

        let calc = MetricsCalculator::default();
        let report = calc.compute_all_named(&results, "test");

        let tmp = tempfile::tempdir().unwrap();
        let path_a = tmp.path().join("a.json");
        let path_b = tmp.path().join("b.json");

        write_results_file(&results, &report, &path_a).unwrap();
        write_results_file(&results, &report, &path_b).unwrap();

        cmd_compare(
            path_a.clone(),
            path_b.clone(),
            tmp.path().join("report.md"),
            "markdown".into(),
        )
        .unwrap();

        let report_content = std::fs::read_to_string(tmp.path().join("report.md")).unwrap();
        assert!(report_content.contains("No regressions detected"));
    }

    #[test]
    fn compare_with_regression_detected() {
        let baseline = vec![make_pass("a", 100), make_pass("b", 100)];
        let current = vec![make_pass("a", 100), make_fail("b")];

        let calc = MetricsCalculator::default();
        let bl_report = calc.compute_all_named(&baseline, "bl");
        let cur_report = calc.compute_all_named(&current, "cur");

        let tmp = tempfile::tempdir().unwrap();
        let bl_path = tmp.path().join("bl.json");
        let cur_path = tmp.path().join("cur.json");

        write_results_file(&baseline, &bl_report, &bl_path).unwrap();
        write_results_file(&current, &cur_report, &cur_path).unwrap();

        cmd_compare(
            bl_path,
            cur_path,
            tmp.path().join("report.md"),
            "markdown".into(),
        )
        .unwrap();

        let report = std::fs::read_to_string(tmp.path().join("report.md")).unwrap();
        assert!(report.contains("Regressions detected"));
    }

    #[test]
    fn check_regression_no_regression_ok() {
        let results = vec![make_pass("a", 100)];
        let calc = MetricsCalculator::default();
        let report = calc.compute_all_named(&results, "test");

        let tmp = tempfile::tempdir().unwrap();
        let bl = tmp.path().join("bl.json");
        let cur = tmp.path().join("cur.json");

        write_results_file(&results, &report, &bl).unwrap();
        write_results_file(&results, &report, &cur).unwrap();

        // Should not error
        cmd_check_regression(bl, cur, 3.0).unwrap();
    }

    #[test]
    fn check_regression_with_drop_fails() {
        let baseline = vec![make_pass("a", 100), make_pass("b", 100)];
        let current = vec![make_pass("a", 100), make_fail("b")];

        let calc = MetricsCalculator::default();
        let bl_r = calc.compute_all_named(&baseline, "bl");
        let cu_r = calc.compute_all_named(&current, "cu");

        let tmp = tempfile::tempdir().unwrap();
        let bl = tmp.path().join("bl.json");
        let cur = tmp.path().join("cur.json");

        write_results_file(&baseline, &bl_r, &bl).unwrap();
        write_results_file(&current, &cu_r, &cur).unwrap();

        // 50% drop → should fail
        let result = cmd_check_regression(bl, cur, 3.0);
        assert!(result.is_err());
    }

    #[test]
    fn load_results_missing_file_errors() {
        let result = load_eval_results(Path::new("/nonexistent/results.json"));
        assert!(result.is_err());
    }
}
