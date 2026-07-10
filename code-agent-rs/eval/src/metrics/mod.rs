//! Metrics computation for benchmark evaluation results.
//!
//! Provides [`MetricsCalculator`] for computing pass@k, resolve rate,
//! token efficiency, and [`BenchmarkReport`] aggregation. Companion
//! modules [`report`] and [`regression`] handle output formatting and
//! comparative analysis.

pub mod regression;
pub mod report;

use crate::types::{EvalResult, EvalStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// TaskResult — per-task row in a benchmark report
// ---------------------------------------------------------------------------

/// 任务结果行
///
/// 【领域含义】BenchmarkReport 中单个任务的结果行，是报告的最小数据单元。
/// 【核心职责】封装单个任务的标识、状态、分数和性能指标，供报告生成和序列化使用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    /// 任务标识
    pub task_id: String,
    /// 结果状态
    pub status: EvalStatus,
    /// 分数（0.0 - 1.0）
    pub score: Option<f64>,
    /// 消耗的 Token 数
    pub tokens_used: u32,
    /// 交互轮次
    pub turns_taken: u32,
    /// 耗时（毫秒）
    pub duration_ms: u64,
}

impl From<&EvalResult> for TaskResult {
    fn from(r: &EvalResult) -> Self {
        Self {
            task_id: r.task_id.clone(),
            status: r.status,
            score: r.score,
            tokens_used: r.metrics.tokens_used,
            turns_taken: r.metrics.turns_taken,
            duration_ms: r.metrics.duration_ms,
        }
    }
}

// ---------------------------------------------------------------------------
// BenchmarkReport
// ---------------------------------------------------------------------------

/// 基准测试报告
///
/// 【领域含义】从一组 EvalResult 计算得出的聚合指标报告，是评估流程的最终产出物。
/// 【核心职责】汇总通过/失败/错误数量、计算 pass@k、平均 Token 消耗、成本估算等指标。
#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkReport {
    /// 基准测试名称 — 人类可读的标识符（如 "HumanEval"、"SWE-bench-Live"）。
    pub benchmark: String,

    /// 总任务数
    pub total_tasks: usize,

    /// 通过数 — EvalStatus::Pass 的任务数量。
    pub passed: usize,

    /// 失败数 — EvalStatus::Fail 的任务数量。
    pub failed: usize,

    /// 错误数 — EvalStatus::Error 或 EvalStatus::Timeout 的任务数量。
    pub errored: usize,

    /// pass@1 — 所有任务组的 pass@1 平均值。
    pub pass_at_1: f64,

    /// pass@5 — 所有任务组的 pass@5 平均值，当任一任务样本数少于 5 时为 None。
    pub pass_at_5: Option<f64>,

    /// 解决率 — 通过任务数占总任务数的比例（每个任务运行一次时 ≈ pass@1）。
    pub resolve_rate: f64,

    /// 平均 Token 消耗 — 所有任务的平均 Token 数。
    pub avg_tokens_per_task: f64,

    /// 平均交互轮次 — 所有任务的平均 Agent 轮次数。
    pub avg_turns_per_task: f64,

    /// 总成本估算 — 以美元计（默认 $0.002 / 1K Token）。
    pub total_cost_estimate: f64,

    /// 逐任务结果行
    pub per_task: Vec<TaskResult>,
}

// ---------------------------------------------------------------------------
// MetricsCalculator
// ---------------------------------------------------------------------------

/// 指标计算器
///
/// 【领域含义】无状态的基准测试指标计算器，所有方法均为纯函数。
/// 【核心职责】计算 pass@k、解决率、Token 效率等评估指标，生成 BenchmarkReport。
/// 可通过 MetricsCalculator::new 配置 Token 单价，或使用默认值。
pub struct MetricsCalculator {
    /// 每 1000 Token 的成本（美元），默认 0.002。
    cost_per_1k_tokens: f64,
}

impl Default for MetricsCalculator {
    fn default() -> Self {
        Self {
            cost_per_1k_tokens: 0.002,
        }
    }
}

impl MetricsCalculator {
    /// 创建计算器
    ///
    /// 【领域含义】构造 MetricsCalculator 实例，指定自定义的 Token 单价。
    /// 【核心职责】设置每 1000 Token 的美元成本，用于成本估算。
    pub fn new(cost_per_1k_tokens: f64) -> Self {
        Self { cost_per_1k_tokens }
    }

    // ------------------------------------------------------------------
    // Core metric functions (pure, usable without &self)
    // ------------------------------------------------------------------

    /// 计算无偏 pass@k 估计值（Chen et al., 2021）
    ///
    /// 【领域含义】HumanEval 论文中定义的无偏 pass@k 估计器。
    /// 【核心职责】根据乘积公式计算 pass@k，避免组合数溢出。
    ///
    /// # 公式
    /// ```text
    /// pass@k = 1 - Π_{i=0}^{k-1} (n - c - i) / (n - i)   如果 n - c ≥ k
    ///          1.0                                          否则
    /// ```
    pub fn pass_at_k(n: usize, c: usize, k: usize) -> f64 {
        if k == 0 {
            return 0.0;
        }
        if n.saturating_sub(c) < k {
            return 1.0;
        }
        let nf = n as f64;
        let cf = c as f64;
        let mut product = 1.0_f64;
        for i in 0..k {
            product *= (nf - cf - i as f64) / (nf - i as f64);
        }
        1.0 - product
    }

    /// 计算解决率
    ///
    /// 【领域含义】通过任务数占总任务数的比例，是评估 Agent 整体性能的核心指标。
    /// 【核心职责】统计 EvalStatus::Pass 的任务占比。
    pub fn resolve_rate(results: &[EvalResult]) -> f64 {
        if results.is_empty() {
            return 0.0;
        }
        let passed = results.iter().filter(|r| r.status == EvalStatus::Pass).count();
        passed as f64 / results.len() as f64
    }

    /// 计算 Token 效率
    ///
    /// 【领域含义】通过任务的 Token 消耗平均值，衡量 Agent 的 Token 使用效率。
    /// 【核心职责】统计通过任务的 Token 消耗均值，无通过任务时返回 0.0。
    pub fn token_efficiency(results: &[EvalResult]) -> f64 {
        let passed: Vec<&EvalResult> =
            results.iter().filter(|r| r.status == EvalStatus::Pass).collect();
        if passed.is_empty() {
            return 0.0;
        }
        let total_tokens: u64 = passed.iter().map(|r| r.metrics.tokens_used as u64).sum();
        total_tokens as f64 / passed.len() as f64
    }

    // ------------------------------------------------------------------
    // Full aggregation
    // ------------------------------------------------------------------

    /// 计算完整报告
    ///
    /// 【领域含义】从评估结果计算完整的 BenchmarkReport。
    /// 【核心职责】按 task_id 分组（支持多次采样场景）、统计状态分布、计算 pass@k 和成本估算。
    /// benchmark 字段使用默认名称 "unnamed"；使用 compute_all_named 可指定有意义标签。
    pub fn compute_all(&self, results: &[EvalResult]) -> BenchmarkReport {
        self.compute_all_named(results, "unnamed")
    }

    /// 计算完整报告（指定基准测试名称）
    ///
    /// 【领域含义】从评估结果计算完整的 BenchmarkReport，并指定基准测试名称。
    /// 【核心职责】与 compute_all 相同，但允许设置有意义的 benchmark 名称。
    pub fn compute_all_named(&self, results: &[EvalResult], benchmark: &str) -> BenchmarkReport {
        // --- Group results by task_id ------------------------------------
        let mut groups: HashMap<&str, Vec<&EvalResult>> = HashMap::new();
        for r in results {
            groups.entry(r.task_id.as_str()).or_default().push(r);
        }

        // --- Per-task metrics --------------------------------------------
        let per_task: Vec<TaskResult> = results.iter().map(TaskResult::from).collect();

        // --- Status counts -----------------------------------------------
        let passed = results.iter().filter(|r| r.status == EvalStatus::Pass).count();
        let failed = results.iter().filter(|r| r.status == EvalStatus::Fail).count();
        let errored = results
            .iter()
            .filter(|r| matches!(r.status, EvalStatus::Error | EvalStatus::Timeout))
            .count();

        // --- pass@1 and pass@5 (averaged over task groups) ---------------
        let mut sum_pass_at_1 = 0.0_f64;
        let mut sum_pass_at_5 = 0.0_f64;
        let mut all_have_5_samples = true;

        for group in groups.values() {
            let n = group.len();
            let c = group.iter().filter(|r| r.status == EvalStatus::Pass).count();

            sum_pass_at_1 += Self::pass_at_k(n, c, 1);
            if n >= 5 {
                sum_pass_at_5 += Self::pass_at_k(n, c, 5);
            } else {
                all_have_5_samples = false;
            }
        }

        let group_count = groups.len().max(1);
        let pass_at_1 = sum_pass_at_1 / group_count as f64;
        let pass_at_5 = if all_have_5_samples && !groups.is_empty() {
            Some(sum_pass_at_5 / groups.len() as f64)
        } else {
            None
        };

        // --- Averages ----------------------------------------------------
        let total = results.len().max(1);
        let avg_tokens_per_task = results.iter().map(|r| r.metrics.tokens_used as u64).sum::<u64>()
            as f64
            / total as f64;
        let avg_turns_per_task = results.iter().map(|r| r.metrics.turns_taken as u64).sum::<u64>()
            as f64
            / total as f64;

        // --- Cost estimate -----------------------------------------------
        let total_tokens: u64 = results.iter().map(|r| r.metrics.tokens_used as u64).sum();
        let total_cost_estimate = (total_tokens as f64 / 1000.0) * self.cost_per_1k_tokens;

        let resolve_rate = if results.is_empty() {
            0.0
        } else {
            passed as f64 / results.len() as f64
        };

        BenchmarkReport {
            benchmark: benchmark.to_string(),
            total_tasks: results.len(),
            passed,
            failed,
            errored,
            pass_at_1,
            pass_at_5,
            resolve_rate,
            avg_tokens_per_task,
            avg_turns_per_task,
            total_cost_estimate,
            per_task,
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EvalMetrics, EvalResult, EvalStatus};

    // ------------------------------------------------------------------
    // helpers
    // ------------------------------------------------------------------

    fn result_pass(id: &str, tokens: u32, turns: u32, duration_ms: u64) -> EvalResult {
        EvalResult {
            task_id: id.to_string(),
            status: EvalStatus::Pass,
            score: Some(1.0),
            logs: vec![],
            patch: None,
            metrics: EvalMetrics {
                duration_ms,
                tokens_used: tokens,
                turns_taken: turns,
            },
        }
    }

    fn result_fail(id: &str, tokens: u32) -> EvalResult {
        EvalResult {
            task_id: id.to_string(),
            status: EvalStatus::Fail,
            score: Some(0.0),
            logs: vec![],
            patch: None,
            metrics: EvalMetrics {
                duration_ms: 0,
                tokens_used: tokens,
                turns_taken: 0,
            },
        }
    }

    fn result_error(id: &str) -> EvalResult {
        EvalResult::error(id, vec!["oops".into()])
    }

    fn result_timeout(id: &str) -> EvalResult {
        EvalResult::timeout(id, 30_000)
    }

    // ------------------------------------------------------------------
    // pass_at_k
    // ------------------------------------------------------------------

    #[test]
    fn pass_at_1_all_pass() {
        // n=10, c=10, k=1 → product = (10-10)/10 = 0, pass = 1.0
        assert!((MetricsCalculator::pass_at_k(10, 10, 1) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn pass_at_1_all_fail() {
        // n=10, c=0, k=1 → product = 10/10 = 1, pass = 0.0
        assert!((MetricsCalculator::pass_at_k(10, 0, 1) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn pass_at_1_half_pass() {
        // n=10, c=5, k=1 → product = 5/10 = 0.5, pass = 0.5
        let v = MetricsCalculator::pass_at_k(10, 5, 1);
        assert!((v - 0.5).abs() < 1e-10, "got {v}");
    }

    #[test]
    fn pass_at_5_half_pass() {
        // n=10, c=5, k=5
        // product = 5/10 * 4/9 * 3/8 * 2/7 * 1/6
        //         = 0.5 * 0.444... * 0.375 * 0.2857... * 0.1666...
        // let's compute: 5*4*3*2*1 / (10*9*8*7*6) = 120 / 30240 = 1/252 ≈ 0.003968
        // pass = 1 - 0.003968 = 0.996032
        let v = MetricsCalculator::pass_at_k(10, 5, 5);
        let product = (5.0 * 4.0 * 3.0 * 2.0 * 1.0) / (10.0 * 9.0 * 8.0 * 7.0 * 6.0);
        let expected = 1.0 - product;
        assert!((v - expected).abs() < 1e-10, "got {v}, expected {expected}");
    }

    #[test]
    fn pass_at_k_not_enough_samples() {
        // n=3, c=2, k=5 → n-c = 1 < 5, so return 1.0
        assert!((MetricsCalculator::pass_at_k(3, 2, 5) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn pass_at_k_zero() {
        assert!((MetricsCalculator::pass_at_k(10, 5, 0) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn pass_at_10_verified() {
        // Known values from HumanEval paper for single problem with n=200
        // c=30: pass@10 should be ~0.868
        // C(170,10)/C(200,10)
        // product formula: prod_{i=0..9} (170-i)/(200-i)
        let n = 200;
        let c = 30;
        let k = 10;
        let v = MetricsCalculator::pass_at_k(n, c, k);
        // Quick cross-check: product = Π (170-i)/(200-i)
        let mut product = 1.0_f64;
        for i in 0..k {
            product *= (n - c - i) as f64 / (n - i) as f64;
        }
        let expected = 1.0 - product;
        assert!((v - expected).abs() < 1e-10, "got {v}, expected {expected}");
        // Rough sanity: with 30/200=15% pass rate, pass@10 should be ~80-86%
        assert!(v > 0.75 && v < 0.95, "pass@10={v} out of expected range");
    }

    // ------------------------------------------------------------------
    // resolve_rate
    // ------------------------------------------------------------------

    #[test]
    fn resolve_rate_mixed() {
        let results = vec![
            result_pass("a", 100, 1, 100),
            result_fail("b", 50),
            result_pass("c", 200, 2, 200),
        ];
        assert!((MetricsCalculator::resolve_rate(&results) - 2.0 / 3.0).abs() < 1e-10);
    }

    #[test]
    fn resolve_rate_empty() {
        let results: Vec<EvalResult> = vec![];
        assert!((MetricsCalculator::resolve_rate(&results) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn resolve_rate_all_fail() {
        let results = vec![result_fail("a", 10), result_fail("b", 20)];
        assert!((MetricsCalculator::resolve_rate(&results) - 0.0).abs() < f64::EPSILON);
    }

    // ------------------------------------------------------------------
    // token_efficiency
    // ------------------------------------------------------------------

    #[test]
    fn token_efficiency_mixed() {
        let results = vec![
            result_pass("a", 100, 1, 0),
            result_fail("b", 50),
            result_pass("c", 200, 2, 0),
        ];
        // avg = (100 + 200) / 2 = 150
        let v = MetricsCalculator::token_efficiency(&results);
        assert!((v - 150.0).abs() < 1e-10, "got {v}");
    }

    #[test]
    fn token_efficiency_no_passes() {
        let results = vec![result_fail("a", 100)];
        assert!((MetricsCalculator::token_efficiency(&results) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn token_efficiency_empty() {
        let results: Vec<EvalResult> = vec![];
        assert!((MetricsCalculator::token_efficiency(&results) - 0.0).abs() < f64::EPSILON);
    }

    // ------------------------------------------------------------------
    // compute_all
    // ------------------------------------------------------------------

    #[test]
    fn compute_all_basic() {
        let calc = MetricsCalculator::default();
        let results = vec![
            result_pass("t/0", 1000, 3, 500),
            result_pass("t/1", 2000, 5, 800),
            result_fail("t/2", 500),
            result_error("t/3"),
            result_timeout("t/4"),
        ];

        let report = calc.compute_all_named(&results, "TestBench");

        assert_eq!(report.benchmark, "TestBench");
        assert_eq!(report.total_tasks, 5);
        assert_eq!(report.passed, 2);
        assert_eq!(report.failed, 1);
        assert_eq!(report.errored, 2);

        // resolve_rate = 2/5 = 0.4
        assert!((report.resolve_rate - 0.4).abs() < 1e-10);

        // pass_at_1: each task has n=1, so pass@1 = passed/total = 0.4
        assert!((report.pass_at_1 - 0.4).abs() < 1e-10);

        // pass_at_5: n=1 per task < 5, so None
        assert!(report.pass_at_5.is_none());

        // avg tokens: (1000+2000+500+0+0)/5 = 3500/5 = 700
        assert!((report.avg_tokens_per_task - 700.0).abs() < 1e-10);

        // avg turns: (3+5+0+0+0)/5 = 8/5 = 1.6
        assert!((report.avg_turns_per_task - 1.6).abs() < 1e-10);

        // cost: 3500 / 1000 * 0.002 = 0.007
        assert!((report.total_cost_estimate - 0.007).abs() < 1e-10);

        assert_eq!(report.per_task.len(), 5);
    }

    #[test]
    fn compute_all_empty_results() {
        let calc = MetricsCalculator::default();
        let report = calc.compute_all(&[]);
        assert_eq!(report.total_tasks, 0);
        assert_eq!(report.passed, 0);
        assert_eq!(report.failed, 0);
        assert_eq!(report.errored, 0);
        assert!((report.resolve_rate - 0.0).abs() < f64::EPSILON);
        assert!((report.pass_at_1 - 0.0).abs() < f64::EPSILON);
        assert!(report.pass_at_5.is_none());
        assert!(report.per_task.is_empty());
    }

    #[test]
    fn compute_all_single_result() {
        let calc = MetricsCalculator::default();
        let results = vec![result_pass("only", 42, 1, 100)];
        let report = calc.compute_all(&results);
        assert_eq!(report.total_tasks, 1);
        assert_eq!(report.passed, 1);
        assert_eq!(report.pass_at_1, 1.0);
        assert!(report.pass_at_5.is_none()); // n=1 < 5
        assert!((report.resolve_rate - 1.0).abs() < 1e-10);
    }

    #[test]
    fn compute_all_with_multiple_samples_per_task() {
        // Task "t/x" has 10 samples, 8 pass; "t/y" has 10 samples, 3 pass
        let mut results: Vec<EvalResult> = Vec::new();
        for _ in 0..8 {
            results.push(result_pass("t/x", 100, 1, 0));
        }
        for _ in 0..2 {
            results.push(result_fail("t/x", 100));
        }
        for _ in 0..3 {
            results.push(result_pass("t/y", 100, 1, 0));
        }
        for _ in 0..7 {
            results.push(result_fail("t/y", 100));
        }

        let calc = MetricsCalculator::default();
        let report = calc.compute_all_named(&results, "MultiSample");

        // pass@1 for "t/x": n=10, c=8 → 1 - 2/10 = 0.8
        // pass@1 for "t/y": n=10, c=3 → 1 - 7/10 = 0.3
        // avg pass@1 = (0.8 + 0.3) / 2 = 0.55
        assert!((report.pass_at_1 - 0.55).abs() < 1e-10,
            "expected 0.55, got {}", report.pass_at_1);

        // pass@5 for "t/x": n=10, c=8, n-c=2 < 5 → 1.0
        // pass@5 for "t/y": n=10, c=3, n-c=7 ≥ 5 → compute
        //   product = 7/10 * 6/9 * 5/8 * 4/7 * 3/6 = product
        //   = 0.7 * 0.666... * 0.625 * 0.5714... * 0.5
        //   = (7*6*5*4*3)/(10*9*8*7*6) = 2520/30240 = 0.08333...
        // pass@5 = 1 - 0.08333 = 0.91666...
        // avg pass@5 = (1.0 + 0.91666...) / 2 ≈ 0.95833
        let expected_pass_at_5 = {
            let p_tx = 1.0; // n-c = 2 < 5
            let p_ty = MetricsCalculator::pass_at_k(10, 3, 5);
            (p_tx + p_ty) / 2.0
        };
        let got = report.pass_at_5.expect("should have pass_at_5");
        assert!((got - expected_pass_at_5).abs() < 1e-10,
            "expected {expected_pass_at_5}, got {got}");

        assert_eq!(report.total_tasks, 20);
    }

    #[test]
    fn compute_all_cost_estimate() {
        let calc = MetricsCalculator::new(0.03); // $0.03 per 1K tokens
        let results = vec![
            result_pass("a", 5_000, 1, 0),
            result_pass("b", 5_000, 1, 0),
        ];
        let report = calc.compute_all(&results);
        // 10_000 / 1000 * 0.03 = 0.30
        assert!((report.total_cost_estimate - 0.30).abs() < 1e-10);
    }

    #[test]
    fn task_result_from_eval_result() {
        let er = result_pass("id", 42, 3, 999);
        let tr = TaskResult::from(&er);
        assert_eq!(tr.task_id, "id");
        assert_eq!(tr.status, EvalStatus::Pass);
        assert_eq!(tr.tokens_used, 42);
        assert_eq!(tr.turns_taken, 3);
        assert_eq!(tr.duration_ms, 999);
    }

    #[test]
    fn benchmark_report_json_roundtrip() {
        let calc = MetricsCalculator::default();
        let results = vec![
            result_pass("a", 100, 2, 200),
            result_fail("b", 50),
        ];
        let report = calc.compute_all_named(&results, "Roundtrip");
        let json = serde_json::to_string(&report).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");

        assert_eq!(parsed["benchmark"].as_str().unwrap(), "Roundtrip");
        assert_eq!(parsed["total_tasks"].as_u64().unwrap(), 2);
        assert_eq!(parsed["passed"].as_u64().unwrap(), 1);
        assert_eq!(parsed["failed"].as_u64().unwrap(), 1);
        assert_eq!(parsed["per_task"].as_array().unwrap().len(), 2);
    }
}
