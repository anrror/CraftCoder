//! Integration tests for the eval crate.
//!
//! These tests exercise the public API without requiring a running Docker
//! daemon or Python installation. The Python subprocess path is deliberately
//! set to an invalid path so the runner returns clean errors rather than
//! panicking.

use code_agent_eval::{
    check_docker, DockerSandbox, EvalMetrics, EvalResult, EvalRunner, EvalStatus, EvalTask,
};
use serde_json;
use std::path::Path;

// ---------------------------------------------------------------------------
// JSON round-trip tests (types.rs)
// ---------------------------------------------------------------------------

#[test]
fn parse_eval_task_full() {
    let json = r#"{
        "task_id": "HumanEval/0",
        "setup_commands": ["pip install pytest"],
        "test_commands": ["python -m pytest test_solution.py"],
        "expected_output": null,
        "timeout_secs": 120,
        "repository": "https://github.com/example/repo.git",
        "base_commit": "abc123def"
    }"#;

    let task: EvalTask = serde_json::from_str(json).expect("deserialize full EvalTask");
    assert_eq!(task.task_id, "HumanEval/0");
    assert_eq!(task.setup_commands, vec!["pip install pytest"]);
    assert_eq!(task.test_commands, vec!["python -m pytest test_solution.py"]);
    assert_eq!(task.expected_output, None);
    assert_eq!(task.timeout_secs, 120);
    assert_eq!(
        task.repository,
        Some("https://github.com/example/repo.git".into())
    );
    assert_eq!(task.base_commit, Some("abc123def".into()));
}

#[test]
fn parse_eval_task_minimal() {
    let json = r#"{"task_id": "min", "test_commands": ["true"]}"#;
    let task: EvalTask = serde_json::from_str(json).expect("minimal task");
    assert_eq!(task.task_id, "min");
    assert_eq!(task.timeout_secs, 120); // default
    assert_eq!(task.expected_output, None);
}

#[test]
fn parse_eval_task_missing_test_commands_is_error() {
    let json = r#"{"task_id": "bad"}"#;
    let err = serde_json::from_str::<EvalTask>(json).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("test_commands"), "expected missing field error, got: {msg}");
}

#[test]
fn serialize_eval_result_roundtrip() {
    let original = EvalResult {
        task_id: "t/0".into(),
        status: EvalStatus::Pass,
        score: Some(1.0),
        logs: vec!["line1".into(), "line2".into()],
        patch: Some("--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new".into()),
        metrics: EvalMetrics {
            duration_ms: 1234,
            tokens_used: 567,
            turns_taken: 3,
        },
    };

    let json = serde_json::to_string(&original).expect("serialize");
    let roundtripped: EvalResult = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(roundtripped.task_id, original.task_id);
    assert_eq!(roundtripped.status, original.status);
    assert_eq!(roundtripped.logs, original.logs);
    assert_eq!(roundtripped.patch, original.patch);
    assert_eq!(roundtripped.metrics.duration_ms, 1234);
    assert_eq!(roundtripped.metrics.tokens_used, 567);
    assert_eq!(roundtripped.metrics.turns_taken, 3);
}

#[test]
fn serialize_eval_result_array_roundtrip() {
    let results = vec![
        EvalResult {
            task_id: "a".into(),
            status: EvalStatus::Pass,
            score: Some(1.0),
            logs: vec![],
            patch: None,
            metrics: Default::default(),
        },
        EvalResult::error("b", vec!["fail".into()]),
        EvalResult::timeout("c", 5000),
    ];

    let json = serde_json::to_string(&results).expect("serialize array");
    let parsed: Vec<EvalResult> = serde_json::from_str(&json).expect("deserialize array");

    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed[0].status, EvalStatus::Pass);
    assert_eq!(parsed[1].status, EvalStatus::Error);
    assert_eq!(parsed[2].status, EvalStatus::Timeout);
}

// ---------------------------------------------------------------------------
// Docker availability detection
// ---------------------------------------------------------------------------

#[test]
fn check_docker_does_not_panic() {
    // Docker may or may not be present — we only verify the function
    // doesn't crash.
    let available = check_docker();
    assert!(available || !available);
}

#[test]
fn docker_sandbox_default_works() {
    let sandbox = DockerSandbox::default();
    let avail = sandbox.is_available();
    assert!(avail || !avail, "is_available() must return bool without panic");
}

#[test]
fn docker_sandbox_new_works() {
    let sandbox = DockerSandbox::new();
    let _ = sandbox.is_available();
}

// ---------------------------------------------------------------------------
// EvalRunner construction
// ---------------------------------------------------------------------------

#[test]
fn runner_new_with_nonexistent_path() {
    let runner = EvalRunner::new(Path::new("/definitely/not/a/real/cli.py"));
    let docker = runner.docker_available();
    assert!(docker || !docker);
}

#[test]
fn runner_with_concurrency_limits() {
    let runner = EvalRunner::with_concurrency(Path::new("/nonexistent/cli.py"), 4);
    let _ = runner.docker_available();
}

// ---------------------------------------------------------------------------
// Empty batch → empty results
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_batch_empty_returns_empty() {
    let runner = EvalRunner::new(Path::new("/nonexistent/cli.py"));
    let results = runner.run_batch(Vec::new()).await.expect("empty batch should succeed");
    assert!(results.is_empty(), "empty input → empty output");
}

// ---------------------------------------------------------------------------
// Python script not found → returns error results
// ---------------------------------------------------------------------------

#[tokio::test]
async fn python_not_found_returns_error_results() {
    let runner = EvalRunner::new(Path::new("/nonexistent/cli.py"));
    let tasks = vec![
        EvalTask {
            task_id: "t1".into(),
            test_commands: vec!["echo ok".into()],
            ..Default::default()
        },
        EvalTask {
            task_id: "t2".into(),
            test_commands: vec!["false".into()],
            ..Default::default()
        },
    ];

    let result = runner.run_batch(tasks).await;
    // The runner should *not* panic; it returns an error or error-marked results.
    match result {
        Ok(results) => {
            // If Python doesn't exist, the subprocess will fail and we get
            // error-marked results.
            assert_eq!(results.len(), 2, "should return a result for every input task");
            for r in &results {
                assert!(
                    r.status.is_failure(),
                    "task {} should be error/timeout, got {:?}",
                    r.task_id,
                    r.status
                );
            }
        }
        Err(e) => {
            // Also acceptable: the runner returns an error.
            let msg = e.to_string();
            assert!(
                msg.contains("Python") || msg.contains("not found"),
                "error should mention Python unavailability: {msg}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Concurrent batch execution (smoke test)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_batch_with_nonexistent_python_does_not_panic() {
    let runner = EvalRunner::with_concurrency(Path::new("/nonexistent/cli.py"), 4);
    let mut tasks = Vec::new();
    for i in 0..20 {
        tasks.push(EvalTask {
            task_id: format!("task_{i}"),
            test_commands: vec!["echo ok".into()],
            ..Default::default()
        });
    }

    let result = runner.run_batch(tasks).await;
    assert!(result.is_ok() || result.is_err(), "must not panic under concurrency");
}

// ---------------------------------------------------------------------------
// EvalStatus helpers
// ---------------------------------------------------------------------------

#[test]
fn eval_status_is_pass() {
    assert!(EvalStatus::Pass.is_pass());
    assert!(!EvalStatus::Fail.is_pass());
    assert!(!EvalStatus::Error.is_pass());
    assert!(!EvalStatus::Timeout.is_pass());
}

#[test]
fn eval_status_is_failure() {
    assert!(!EvalStatus::Pass.is_failure());
    assert!(EvalStatus::Fail.is_failure());
    assert!(EvalStatus::Error.is_failure());
    assert!(EvalStatus::Timeout.is_failure());
}

// ---------------------------------------------------------------------------
// EvalResult constructors
// ---------------------------------------------------------------------------

#[test]
fn eval_result_error_and_timeout_constructors() {
    let err = EvalResult::error("id1", vec!["msg".into()]);
    assert_eq!(err.task_id, "id1");
    assert_eq!(err.status, EvalStatus::Error);
    assert_eq!(err.logs, vec!["msg"]);

    let timeout = EvalResult::timeout("id2", 10000);
    assert_eq!(timeout.task_id, "id2");
    assert_eq!(timeout.status, EvalStatus::Timeout);
    assert_eq!(timeout.metrics.duration_ms, 10000);
}

// ---------------------------------------------------------------------------
// EvalMetrics default
// ---------------------------------------------------------------------------

#[test]
fn eval_metrics_default_is_zero() {
    let m = EvalMetrics::default();
    assert_eq!(m.duration_ms, 0);
    assert_eq!(m.tokens_used, 0);
    assert_eq!(m.turns_taken, 0);
}
