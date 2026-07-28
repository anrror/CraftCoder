//! Regression detection — compare two [`BenchmarkReport`] snapshots and flag
//! statistically meaningful degradations.

use super::BenchmarkReport;
use serde::Serialize;

// ---------------------------------------------------------------------------
// RegressionSeverity
// ---------------------------------------------------------------------------

/// 回归严重级别
///
/// 【领域含义】表示回归信号的严重程度等级，用于区分信息性变化、警告和严重退化。
/// 【核心职责】根据指标变化幅度分类回归严重性，供 CI 门禁和告警使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RegressionSeverity {
    /// 信息 — 变化 ≤ 5% 绝对差值，仅通知。
    Info,
    /// 警告 — 回归 > 5% 且 ≤ 10%。
    Warning,
    /// 严重 — 回归 > 10% 绝对差值。
    Critical,
}

// ---------------------------------------------------------------------------
// RegressionSignal
// ---------------------------------------------------------------------------

/// 回归信号
///
/// 【领域含义】表示两次运行之间单个指标的对比结果，用于检测性能退化。
/// 【核心职责】封装指标名称、前后值、变化量和严重级别，供回归检测和报告使用。
#[derive(Debug, Clone, Serialize)]
pub struct RegressionSignal {
    /// 指标名称 — 人类可读的指标名（如 "resolve_rate"）。
    pub metric: String,

    /// 基线值
    pub before: f64,

    /// 当前值
    pub after: f64,

    /// 变化量 — `after - before`（负值 = 回归，正值 = 改进）。
    pub delta: f64,

    /// 是否回归 — 指标退化是否超过信息阈值。
    pub is_regression: bool,

    /// 严重级别
    pub severity: RegressionSeverity,
}

// ---------------------------------------------------------------------------
// RegressionDetector
// ---------------------------------------------------------------------------

/// 回归检测器
///
/// 【领域含义】无状态的 BenchmarkReport 快照比较器，用于检测性能退化。
/// 【核心职责】比较当前报告与基线报告，生成回归信号列表，供 CI 门禁决策。
pub struct RegressionDetector;

impl RegressionDetector {
    /// 检测回归
    ///
    /// 【领域含义】比较当前报告与基线报告，返回回归信号列表。
    /// 【核心职责】对比 resolve_rate、pass@1、pass@5、Token 消耗、交互轮次和成本估算。
    /// 仅比较两个报告中都存在的指标。
    ///
    /// 阈值：
    /// - **严重**: 下降 > 10 个百分点
    /// - **警告**: 下降 > 5 个百分点
    /// - **信息**: 下降 ≤ 5 个百分点（不标记为回归）
    pub fn detect(
        current: &BenchmarkReport,
        baseline: &BenchmarkReport,
    ) -> Vec<RegressionSignal> {
        let mut signals = Vec::new();

        // resolve_rate
        signals.push(Self::build_signal(
            "resolve_rate",
            baseline.resolve_rate,
            current.resolve_rate,
            true, // lower is worse
        ));

        // pass@1
        signals.push(Self::build_signal(
            "pass_at_1",
            baseline.pass_at_1,
            current.pass_at_1,
            true,
        ));

        // pass@5 (only if both reports have it)
        if let (Some(b), Some(c)) = (baseline.pass_at_5, current.pass_at_5) {
            signals.push(Self::build_signal("pass_at_5", b, c, true));
        }

        // avg_tokens_per_task (higher is worse for efficiency, so invert)
        signals.push(Self::build_signal(
            "avg_tokens_per_task",
            baseline.avg_tokens_per_task,
            current.avg_tokens_per_task,
            false, // higher is worse
        ));

        // avg_turns_per_task (higher is worse)
        signals.push(Self::build_signal(
            "avg_turns_per_task",
            baseline.avg_turns_per_task,
            current.avg_turns_per_task,
            false,
        ));

        // total_cost_estimate (higher is worse)
        signals.push(Self::build_signal(
            "total_cost_estimate",
            baseline.total_cost_estimate,
            current.total_cost_estimate,
            false,
        ));

        signals
    }

    /// Build a single [`RegressionSignal`].
    ///
    /// `lower_is_better`: when `true`, a negative delta is a regression
    /// (e.g. resolve_rate). When `false`, a positive delta is a regression
    /// (e.g. cost or token count).
    fn build_signal(
        metric: &str,
        before: f64,
        after: f64,
        lower_is_better: bool,
    ) -> RegressionSignal {
        let delta = after - before;

        // The affective delta — how much "worse" it got.
        let degradation = if lower_is_better { -delta } else { delta };

        // Compute relative degradation percentage.
        // For rate metrics (0..1), this gives percentage-point change.
        // For unbounded metrics (tokens, cost), this gives relative change.
        let baseline_abs = if before.abs() < f64::EPSILON { 1.0 } else { before.abs() };
        let degradation_pct = (degradation / baseline_abs) * 100.0;

        let (is_regression, severity) = if degradation_pct > 10.0 {
            (true, RegressionSeverity::Critical)
        } else if degradation_pct > 5.0 {
            (true, RegressionSeverity::Warning)
        } else {
            (false, RegressionSeverity::Info)
        };

        RegressionSignal {
            metric: metric.to_string(),
            before,
            after,
            delta,
            is_regression,
            severity,
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MetricsCalculator;
    use crate::types::{EvalMetrics, EvalResult, EvalStatus};

    fn make_report(
        name: &str,
        pass_count: usize,
        fail_count: usize,
        tokens_per_task: u32,
        turns_per_task: u32,
    ) -> BenchmarkReport {
        let calc = MetricsCalculator::default();
        let mut results: Vec<EvalResult> = Vec::new();
        for i in 0..pass_count {
            results.push(EvalResult {
                task_id: format!("t/{i}"),
                status: EvalStatus::Pass,
                score: Some(1.0),
                logs: vec![],
                patch: None,
                metrics: EvalMetrics {
                    duration_ms: 100,
                    tokens_used: tokens_per_task,
                    turns_taken: turns_per_task,
                },
            });
        }
        for i in pass_count..(pass_count + fail_count) {
            results.push(EvalResult {
                task_id: format!("t/{i}"),
                status: EvalStatus::Fail,
                score: Some(0.0),
                logs: vec![],
                patch: None,
                metrics: EvalMetrics {
                    duration_ms: 100,
                    tokens_used: tokens_per_task,
                    turns_taken: turns_per_task,
                },
            });
        }
        calc.compute_all_named(&results, name)
    }

    // ------------------------------------------------------------------
    // detection: 10 % drop
    // ------------------------------------------------------------------

    #[test]
    fn regression_detects_10_pct_drop() {
        // Baseline: 100% pass rate
        let baseline = make_report("baseline", 10, 0, 500, 1);
        // Current: 90% pass rate (10% drop)
        let current = make_report("current", 9, 1, 500, 1);

        let signals = RegressionDetector::detect(&current, &baseline);

        let resolve_signal = signals
            .iter()
            .find(|s| s.metric == "resolve_rate")
            .expect("must have resolve_rate signal");

        // baseline = 1.0, current = 0.9, delta = -0.1 (10 pp drop)
        assert!((resolve_signal.delta + 0.1).abs() < 1e-10);
        assert!(resolve_signal.is_regression);
        assert_eq!(resolve_signal.severity, RegressionSeverity::Warning);
    }

    // ------------------------------------------------------------------
    // detection: 20 % drop → critical
    // ------------------------------------------------------------------

    #[test]
    fn regression_severity_critical() {
        // Baseline: 100%
        let baseline = make_report("baseline", 10, 0, 500, 1);
        // Current: 70% (30% drop) — critical
        let current = make_report("current", 7, 3, 500, 1);

        let signals = RegressionDetector::detect(&current, &baseline);
        let resolve_signal = signals
            .iter()
            .find(|s| s.metric == "resolve_rate")
            .expect("must have resolve_rate");

        assert!(resolve_signal.is_regression);
        assert_eq!(resolve_signal.severity, RegressionSeverity::Critical);
    }

    // ------------------------------------------------------------------
    // no regression when similar
    // ------------------------------------------------------------------

    #[test]
    fn no_regression_when_similar() {
        let calc = MetricsCalculator::default();

        let results_baseline: Vec<EvalResult> = (0..10)
            .map(|i| EvalResult {
                task_id: format!("t/{i}"),
                status: EvalStatus::Pass,
                score: Some(1.0),
                logs: vec![],
                patch: None,
                metrics: EvalMetrics {
                    duration_ms: 100,
                    tokens_used: 500,
                    turns_taken: 1,
                },
            })
            .collect();

        let results_current: Vec<EvalResult> = (0..10)
            .map(|i| EvalResult {
                task_id: format!("t/{i}"),
                status: EvalStatus::Pass,
                score: Some(1.0),
                logs: vec![],
                patch: None,
                metrics: EvalMetrics {
                    duration_ms: 100,
                    tokens_used: 510, // 2% more tokens
                    turns_taken: 1,
                },
            })
            .collect();

        let baseline = calc.compute_all_named(&results_baseline, "b");
        let current = calc.compute_all_named(&results_current, "c");

        let signals = RegressionDetector::detect(&current, &baseline);
        let token_signal = signals
            .iter()
            .find(|s| s.metric == "avg_tokens_per_task")
            .expect("must have token signal");

        // 10 token increase out of 500 = 2% → not a regression
        assert!(!token_signal.is_regression);
        assert_eq!(token_signal.severity, RegressionSeverity::Info);
    }

    // ------------------------------------------------------------------
    // empty results handled
    // ------------------------------------------------------------------

    #[test]
    fn regression_with_empty_reports() {
        let calc = MetricsCalculator::default();
        let baseline = calc.compute_all_named(&[], "empty_base");
        let current = calc.compute_all_named(&[], "empty_cur");

        let signals = RegressionDetector::detect(&current, &baseline);
        // Should not panic, and all signals should have zero delta
        for s in &signals {
            assert!((s.delta).abs() < f64::EPSILON, "delta for {}: {}", s.metric, s.delta);
            assert!(!s.is_regression);
        }
    }

    // ------------------------------------------------------------------
    // single result edge case
    // ------------------------------------------------------------------

    #[test]
    fn regression_single_result() {
        let calc = MetricsCalculator::default();

        let baseline_results = vec![EvalResult {
            task_id: "only".into(),
            status: EvalStatus::Pass,
            score: Some(1.0),
            logs: vec![],
            patch: None,
            metrics: EvalMetrics {
                duration_ms: 100,
                tokens_used: 100,
                turns_taken: 1,
            },
        }];

        let current_results = vec![EvalResult {
            task_id: "only".into(),
            status: EvalStatus::Fail,
            score: Some(0.0),
            logs: vec![],
            patch: None,
            metrics: EvalMetrics {
                duration_ms: 200,
                tokens_used: 200,
                turns_taken: 2,
            },
        }];

        let baseline = calc.compute_all_named(&baseline_results, "b");
        let current = calc.compute_all_named(&current_results, "c");

        let signals = RegressionDetector::detect(&current, &baseline);

        let resolve_signal = signals
            .iter()
            .find(|s| s.metric == "resolve_rate")
            .expect("resolve_rate signal");

        // 100% → 0% = critical regression
        assert!(resolve_signal.is_regression);
        assert_eq!(resolve_signal.severity, RegressionSeverity::Critical);
        assert!((resolve_signal.delta + 1.0).abs() < 1e-10);

        // Token increase: 100 → 200 = 100% increase → critical regression
        let token_signal = signals
            .iter()
            .find(|s| s.metric == "avg_tokens_per_task")
            .expect("token signal");
        assert!(token_signal.is_regression);
        assert_eq!(token_signal.severity, RegressionSeverity::Critical);
    }

    // ------------------------------------------------------------------
    // severity ordering
    // ------------------------------------------------------------------

    #[test]
    fn regression_severity_ordering() {
        assert!(RegressionSeverity::Critical > RegressionSeverity::Warning);
        assert!(RegressionSeverity::Warning > RegressionSeverity::Info);
    }

    // ------------------------------------------------------------------
    // all signals present
    // ------------------------------------------------------------------

    #[test]
    fn regression_signals_count() {
        let baseline = make_report("b", 10, 0, 500, 1);
        let current = make_report("c", 8, 2, 600, 2);

        let signals = RegressionDetector::detect(&current, &baseline);
        // 6 signals: resolve_rate, pass_at_1, avg_tokens_per_task,
        // avg_turns_per_task, total_cost_estimate
        // pass@5 omitted because n=1 per task in make_report
        assert!(signals.len() >= 5, "got {} signals", signals.len());

        let names: Vec<&str> = signals.iter().map(|s| s.metric.as_str()).collect();
        assert!(names.contains(&"resolve_rate"));
        assert!(names.contains(&"pass_at_1"));
        assert!(names.contains(&"avg_tokens_per_task"));
        assert!(names.contains(&"avg_turns_per_task"));
        assert!(names.contains(&"total_cost_estimate"));
    }
}
