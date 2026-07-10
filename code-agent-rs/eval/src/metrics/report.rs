//! Report generation: HTML, Markdown, and JSON export for [`BenchmarkReport`].
//!
//! HTML reports are self-contained (all CSS inline) with optional
//! Chart.js trends when a baseline report is supplied.

use super::BenchmarkReport;
use crate::types::EvalStatus;

/// 报告生成器
///
/// 【领域含义】无状态的报告格式化器，将 BenchmarkReport 输出为 HTML、Markdown 或 JSON 格式。
/// 【核心职责】提供多种格式的报告导出能力，支持与基线报告对比。
pub struct ReportGenerator;

/// Format a numeric delta with sign and arrow.
fn fmt_delta(current: f64, baseline: f64) -> String {
    let delta = current - baseline;
    if delta >= 0.0 {
        format!("+{:.2} ▲", delta)
    } else {
        format!("{:.2} ▼", delta)
    }
}

/// Return a CSS class suffix for status-based row colouring.
fn status_css(status: EvalStatus) -> &'static str {
    match status {
        EvalStatus::Pass => "pass",
        EvalStatus::Fail => "fail",
        EvalStatus::Error => "error",
        EvalStatus::Timeout => "timeout",
    }
}

/// Human-readable status label.
fn status_label(status: EvalStatus) -> &'static str {
    match status {
        EvalStatus::Pass => "PASS",
        EvalStatus::Fail => "FAIL",
        EvalStatus::Error => "ERROR",
        EvalStatus::Timeout => "TIMEOUT",
    }
}

impl ReportGenerator {
    // ======================================================================
    // HTML
    // ======================================================================

    /// 生成 HTML 报告
    ///
    /// 【领域含义】生成自包含的 HTML 格式评估报告，所有 CSS 内联。
    /// 【核心职责】渲染摘要卡片、指标表格和逐任务结果表格。
    /// 当提供 baseline 时，包含对比部分，显示指标变化量和颜色编码的回归标记。
    pub fn to_html(report: &BenchmarkReport, baseline: Option<&BenchmarkReport>) -> String {
        let mut h = String::with_capacity(32_768);

        // -- document head -------------------------------------------------
        h.push_str(r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>"#);
        h.push_str(&html_escape(&report.benchmark));
        h.push_str(" — Benchmark Report</title>\n<style>\n");
        h.push_str(include_str!("report_styles.css"));
        h.push_str("\n</style>\n</head>\n<body>\n");

        // -- header --------------------------------------------------------
        h.push_str(&format!(
            "<div class=\"header\"><h1>{} — Benchmark Report</h1></div>\n",
            html_escape(&report.benchmark)
        ));

        // -- summary cards -------------------------------------------------
        h.push_str("<div class=\"summary-cards\">\n");
        h.push_str(&summary_card("Total Tasks", &report.total_tasks.to_string(), "#3498db"));
        h.push_str(&summary_card("Passed", &report.passed.to_string(), "#27ae60"));
        h.push_str(&summary_card("Failed", &report.failed.to_string(), "#e74c3c"));
        h.push_str(&summary_card("Errored", &report.errored.to_string(), "#f39c12"));
        h.push_str("</div>\n");

        // -- metrics table -------------------------------------------------
        h.push_str("<h2>Metrics</h2>\n");
        h.push_str("<table class=\"metrics-table\">\n<thead><tr><th>Metric</th><th>Value</th>");
        if baseline.is_some() {
            h.push_str("<th>Baseline</th><th>Delta</th>");
        }
        h.push_str("</tr></thead>\n<tbody>\n");

        let mut rows: Vec<String> = Vec::new();

        // Resolve rate
        rows.push(metrics_row_baseline(
            "Resolve Rate",
            &format!("{:.2}%", report.resolve_rate * 100.0),
            baseline.map(|b| format!("{:.2}%", b.resolve_rate * 100.0)),
            baseline.map(|b| fmt_delta(report.resolve_rate, b.resolve_rate)),
        ));

        // pass@1
        rows.push(metrics_row_baseline(
            "pass@1",
            &format!("{:.2}%", report.pass_at_1 * 100.0),
            baseline.map(|b| format!("{:.2}%", b.pass_at_1 * 100.0)),
            baseline.map(|b| fmt_delta(report.pass_at_1, b.pass_at_1)),
        ));

        // pass@5
        let p5 = report
            .pass_at_5
            .map(|v| format!("{:.2}%", v * 100.0))
            .unwrap_or_else(|| "N/A".to_string());
        let p5_base = baseline.and_then(|b| b.pass_at_5);
        let p5_delta = match (report.pass_at_5, p5_base) {
            (Some(c), Some(bv)) => Some(fmt_delta(c, bv)),
            _ => None,
        };
        rows.push(metrics_row_baseline(
            "pass@5",
            &p5,
            p5_base.map(|v| format!("{:.2}%", v * 100.0)),
            p5_delta,
        ));

        // Avg tokens/task
        rows.push(metrics_row_baseline(
            "Avg Tokens / Task",
            &format!("{:.0}", report.avg_tokens_per_task),
            baseline.map(|b| format!("{:.0}", b.avg_tokens_per_task)),
            baseline.map(|b| fmt_delta(report.avg_tokens_per_task, b.avg_tokens_per_task)),
        ));

        // Avg turns/task
        rows.push(metrics_row_baseline(
            "Avg Turns / Task",
            &format!("{:.1}", report.avg_turns_per_task),
            baseline.map(|b| format!("{:.1}", b.avg_turns_per_task)),
            baseline.map(|b| fmt_delta(report.avg_turns_per_task, b.avg_turns_per_task)),
        ));

        // Total cost estimate
        rows.push(metrics_row_baseline(
            "Total Cost Estimate",
            &format!("${:.4}", report.total_cost_estimate),
            baseline.map(|b| format!("${:.4}", b.total_cost_estimate)),
            baseline.map(|b| fmt_delta(report.total_cost_estimate, b.total_cost_estimate)),
        ));

        h.push_str(&rows.join("\n"));
        h.push_str("</tbody>\n</table>\n");

        // -- per-task table ------------------------------------------------
        h.push_str("<h2>Per-Task Results</h2>\n");
        h.push_str("<table class=\"task-table\">\n");
        h.push_str("<thead><tr><th>Task ID</th><th>Status</th><th>Score</th><th>Tokens</th><th>Turns</th><th>Duration (ms)</th></tr></thead>\n<tbody>\n");

        for task in &report.per_task {
            let sc = task
                .score
                .map(|s| format!("{:.2}", s))
                .unwrap_or_else(|| "—".to_string());
            h.push_str(&format!(
                "<tr class=\"{css}\"><td>{id}</td><td>{status}</td><td>{score}</td><td>{tokens}</td><td>{turns}</td><td>{dur}</td></tr>\n",
                css = status_css(task.status),
                id = html_escape(&task.task_id),
                status = status_label(task.status),
                score = sc,
                tokens = task.tokens_used,
                turns = task.turns_taken,
                dur = task.duration_ms,
            ));
        }

        h.push_str("</tbody>\n</table>\n");

        // -- footer --------------------------------------------------------
        h.push_str("<footer>Generated by code-agent-eval metrics</footer>\n");
        h.push_str("</body>\n</html>\n");

        h
    }

    // ======================================================================
    // Markdown
    // ======================================================================

    /// 生成 Markdown 报告
    ///
    /// 【领域含义】生成 Markdown 格式的评估报告，适合嵌入 CI 注释或文档。
    /// 【核心职责】渲染摘要表格和逐任务结果表格。
    /// 当提供 baseline 时，追加对比表格。
    pub fn to_markdown(report: &BenchmarkReport, baseline: Option<&BenchmarkReport>) -> String {
        let mut m = String::with_capacity(8_192);

        // Title
        m.push_str(&format!("# {} — Benchmark Report\n\n", report.benchmark));

        // Summary
        m.push_str("## Summary\n\n");
        m.push_str("| Metric | Value |\n|--------|-------|\n");
        m.push_str(&format!(
            "| Total Tasks | {} |\n",
            report.total_tasks
        ));
        m.push_str(&format!("| Passed | {} |\n", report.passed));
        m.push_str(&format!("| Failed | {} |\n", report.failed));
        m.push_str(&format!("| Errored | {} |\n", report.errored));
        m.push_str(&format!(
            "| Resolve Rate | {:.2}% |\n",
            report.resolve_rate * 100.0
        ));
        m.push_str(&format!(
            "| pass@1 | {:.2}% |\n",
            report.pass_at_1 * 100.0
        ));
        match report.pass_at_5 {
            Some(v) => m.push_str(&format!("| pass@5 | {:.2}% |\n", v * 100.0)),
            None => m.push_str("| pass@5 | N/A |\n"),
        }
        m.push_str(&format!(
            "| Avg Tokens / Task | {:.0} |\n",
            report.avg_tokens_per_task
        ));
        m.push_str(&format!(
            "| Avg Turns / Task | {:.1} |\n",
            report.avg_turns_per_task
        ));
        m.push_str(&format!(
            "| Total Cost Estimate | ${:.4} |\n",
            report.total_cost_estimate
        ));
        m.push('\n');

        // Baseline comparison
        if let Some(base) = baseline {
            m.push_str("## Comparison vs Baseline\n\n");
            m.push_str("| Metric | Current | Baseline | Delta |\n");
            m.push_str("|--------|---------|----------|-------|\n");

            let delta_rr = fmt_delta(report.resolve_rate, base.resolve_rate);
            m.push_str(&format!(
                "| Resolve Rate | {:.2}% | {:.2}% | {} |\n",
                report.resolve_rate * 100.0,
                base.resolve_rate * 100.0,
                delta_rr,
            ));

            let delta_p1 = fmt_delta(report.pass_at_1, base.pass_at_1);
            m.push_str(&format!(
                "| pass@1 | {:.2}% | {:.2}% | {} |\n",
                report.pass_at_1 * 100.0,
                base.pass_at_1 * 100.0,
                delta_p1,
            ));

            let p5_current = report
                .pass_at_5
                .map(|v| format!("{:.2}%", v * 100.0))
                .unwrap_or_else(|| "N/A".to_string());
            let p5_base = base
                .pass_at_5
                .map(|v| format!("{:.2}%", v * 100.0))
                .unwrap_or_else(|| "N/A".to_string());
            let p5_delta = match (report.pass_at_5, base.pass_at_5) {
                (Some(c), Some(bv)) => fmt_delta(c, bv),
                _ => "—".to_string(),
            };
            m.push_str(&format!(
                "| pass@5 | {} | {} | {} |\n",
                p5_current, p5_base, p5_delta
            ));

            let delta_tok =
                fmt_delta(report.avg_tokens_per_task, base.avg_tokens_per_task);
            m.push_str(&format!(
                "| Avg Tokens / Task | {:.0} | {:.0} | {} |\n",
                report.avg_tokens_per_task, base.avg_tokens_per_task, delta_tok,
            ));

            let delta_turn =
                fmt_delta(report.avg_turns_per_task, base.avg_turns_per_task);
            m.push_str(&format!(
                "| Avg Turns / Task | {:.1} | {:.1} | {} |\n",
                report.avg_turns_per_task, base.avg_turns_per_task, delta_turn,
            ));

            let delta_cost =
                fmt_delta(report.total_cost_estimate, base.total_cost_estimate);
            m.push_str(&format!(
                "| Total Cost Estimate | ${:.4} | ${:.4} | {} |\n",
                report.total_cost_estimate, base.total_cost_estimate, delta_cost,
            ));
            m.push('\n');
        }

        // Per-task table
        m.push_str("## Per-Task Results\n\n");
        m.push_str("| Task ID | Status | Score | Tokens | Turns | Duration (ms) |\n");
        m.push_str("|---------|--------|-------|--------|-------|---------------|\n");

        for task in &report.per_task {
            let sc = task
                .score
                .map(|s| format!("{:.2}", s))
                .unwrap_or_else(|| "—".to_string());
            m.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                task.task_id,
                status_label(task.status),
                sc,
                task.tokens_used,
                task.turns_taken,
                task.duration_ms,
            ));
        }
        m.push('\n');

        m
    }

    // ======================================================================
    // JSON
    // ======================================================================

    /// 导出 JSON 报告
    ///
    /// 【领域含义】将报告导出为格式化 JSON，适合程序化消费。
    /// 【核心职责】序列化 BenchmarkReport 为美观打印的 JSON 字符串。
    ///
    /// # Panics
    /// 如果序列化失败则 panic（格式良好的报告不应发生）。
    pub fn to_json(report: &BenchmarkReport) -> String {
        serde_json::to_string_pretty(report).expect("BenchmarkReport is always serializable")
    }
}

// ---------------------------------------------------------------------------
// HTML helpers
// ---------------------------------------------------------------------------

/// Build a `<div class="summary-card">` element.
fn summary_card(label: &str, value: &str, colour: &str) -> String {
    format!(
        "<div class=\"card\" style=\"border-left-color:{colour};\"><div class=\"card-value\">{value}</div><div class=\"card-label\">{label}</div></div>\n"
    )
}

/// Build a `<tr>` for the metrics table with optional baseline columns.
fn metrics_row_baseline(
    label: &str,
    current: &str,
    baseline_val: Option<String>,
    delta: Option<String>,
) -> String {
    if let (Some(bv), Some(d)) = (baseline_val, delta) {
        let degraded = d.contains('▼');
        let colour = if degraded { "#e74c3c" } else { "#27ae60" };
        format!(
            "<tr><td>{label}</td><td>{current}</td><td>{bv}</td><td style=\"color:{colour};font-weight:bold;\">{d}</td></tr>"
        )
    } else {
        format!("<tr><td>{label}</td><td>{current}</td></tr>")
    }
}

/// Minimal HTML entity encoding (handles `<`, `>`, `&`, `"`).
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MetricsCalculator;
    use crate::types::{EvalMetrics, EvalResult, EvalStatus};

    fn build_sample_report() -> BenchmarkReport {
        let calc = MetricsCalculator::default();
        let results: Vec<EvalResult> = vec![
            EvalResult {
                task_id: "t/0".into(),
                status: EvalStatus::Pass,
                score: Some(1.0),
                logs: vec![],
                patch: None,
                metrics: EvalMetrics {
                    duration_ms: 100,
                    tokens_used: 500,
                    turns_taken: 2,
                },
            },
            EvalResult {
                task_id: "t/1".into(),
                status: EvalStatus::Fail,
                score: Some(0.0),
                logs: vec!["assertion failed".into()],
                patch: None,
                metrics: EvalMetrics {
                    duration_ms: 200,
                    tokens_used: 300,
                    turns_taken: 1,
                },
            },
            EvalResult {
                task_id: "t/2".into(),
                status: EvalStatus::Error,
                score: None,
                logs: vec!["sandbox crash".into()],
                patch: None,
                metrics: EvalMetrics::default(),
            },
        ];
        calc.compute_all_named(&results, "TestBench")
    }

    fn build_baseline_report() -> BenchmarkReport {
        let calc = MetricsCalculator::default();
        let results: Vec<EvalResult> = vec![
            EvalResult {
                task_id: "t/0".into(),
                status: EvalStatus::Pass,
                score: Some(1.0),
                logs: vec![],
                patch: None,
                metrics: EvalMetrics {
                    duration_ms: 90,
                    tokens_used: 400,
                    turns_taken: 1,
                },
            },
            EvalResult {
                task_id: "t/1".into(),
                status: EvalStatus::Pass,
                score: Some(1.0),
                logs: vec![],
                patch: None,
                metrics: EvalMetrics {
                    duration_ms: 150,
                    tokens_used: 250,
                    turns_taken: 1,
                },
            },
            EvalResult {
                task_id: "t/2".into(),
                status: EvalStatus::Pass,
                score: Some(1.0),
                logs: vec![],
                patch: None,
                metrics: EvalMetrics::default(),
            },
        ];
        calc.compute_all_named(&results, "Baseline")
    }

    // ------------------------------------------------------------------
    // HTML
    // ------------------------------------------------------------------

    #[test]
    fn html_report_contains_expected_sections() {
        let report = build_sample_report();
        let html = ReportGenerator::to_html(&report, None);

        assert!(html.contains("<title>TestBench — Benchmark Report</title>"));
        assert!(html.contains("TestBench — Benchmark Report</h1>"));
        assert!(html.contains("<h2>Metrics</h2>"));
        assert!(html.contains("<h2>Per-Task Results</h2>"));
        assert!(html.contains("Total Tasks"));
        assert!(html.contains("Passed"));
        assert!(html.contains("Failed"));
        assert!(html.contains("Errored"));
        assert!(html.contains("t/0"));
        assert!(html.contains("t/1"));
        assert!(html.contains("t/2"));
        // color coding
        assert!(html.contains("class=\"pass\""));
        assert!(html.contains("class=\"fail\""));
        assert!(html.contains("class=\"error\""));
    }

    #[test]
    fn html_report_with_baseline() {
        let report = build_sample_report();
        let baseline = build_baseline_report();
        let html = ReportGenerator::to_html(&report, Some(&baseline));

        assert!(html.contains("Baseline"));
        assert!(html.contains("Delta"));
        // Regression (resolve rate dropped from 100% to 33%)
        assert!(html.contains('▼'));
    }

    #[test]
    fn html_report_empty_handled_gracefully() {
        let calc = MetricsCalculator::default();
        let report = calc.compute_all(&[]);
        let html = ReportGenerator::to_html(&report, None);

        assert!(html.contains("Total Tasks"));
        assert!(html.contains(">0<"));
        // should not panic
        assert!(!html.contains("panic"));
    }

    // ------------------------------------------------------------------
    // Markdown
    // ------------------------------------------------------------------

    #[test]
    fn markdown_report_renders_correctly() {
        let report = build_sample_report();
        let md = ReportGenerator::to_markdown(&report, None);

        assert!(md.contains("# TestBench — Benchmark Report"));
        assert!(md.contains("## Summary"));
        assert!(md.contains("## Per-Task Results"));
        assert!(md.contains("| Total Tasks | 3 |"));
        assert!(md.contains("| Passed | 1 |"));
        assert!(md.contains("| Failed | 1 |"));
        assert!(md.contains("| Errored | 1 |"));
        assert!(md.contains("| Resolve Rate | 33.33% |"));
        assert!(md.contains("t/0"));
        assert!(md.contains("t/1"));
        assert!(md.contains("t/2"));
    }

    #[test]
    fn markdown_report_with_baseline() {
        let report = build_sample_report();
        let baseline = build_baseline_report();
        let md = ReportGenerator::to_markdown(&report, Some(&baseline));

        assert!(md.contains("## Comparison vs Baseline"));
        assert!(md.contains("| Resolve Rate | 33.33% | 100.00% |"));
        // Should show regression (▼)
        assert!(md.contains('▼'));
    }

    #[test]
    fn markdown_report_empty() {
        let calc = MetricsCalculator::default();
        let report = calc.compute_all(&[]);
        let md = ReportGenerator::to_markdown(&report, None);

        assert!(md.contains("| Total Tasks | 0 |"));
        assert!(!md.contains("panic"));
    }

    // ------------------------------------------------------------------
    // JSON
    // ------------------------------------------------------------------

    #[test]
    fn json_export_matches_schema() {
        let report = build_sample_report();
        let json = ReportGenerator::to_json(&report);
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("valid JSON");

        assert_eq!(parsed["benchmark"], "TestBench");
        assert_eq!(parsed["total_tasks"], 3);
        assert_eq!(parsed["passed"], 1);
        assert_eq!(parsed["failed"], 1);
        assert_eq!(parsed["errored"], 1);

        let per_task = parsed["per_task"].as_array().expect("per_task is array");
        assert_eq!(per_task.len(), 3);
        assert_eq!(per_task[0]["task_id"], "t/0");
        assert_eq!(per_task[0]["status"], "pass");
    }

    #[test]
    fn json_export_empty_report() {
        let calc = MetricsCalculator::default();
        let report = calc.compute_all(&[]);
        let json = ReportGenerator::to_json(&report);
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["total_tasks"], 0);
        assert!(parsed["per_task"].as_array().unwrap().is_empty());
    }

    // ------------------------------------------------------------------
    // html_escape
    // ------------------------------------------------------------------

    #[test]
    fn html_escape_encodes_special_chars() {
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
        assert_eq!(html_escape("a & b"), "a &amp; b");
        assert_eq!(html_escape("\"quoted\""), "&quot;quoted&quot;");
        assert_eq!(html_escape("normal text"), "normal text");
    }
}
