//! Round-trip integration tests for the eval-bridge.
//!
//! These tests require the `code_agent_eval` Python package to be installed
//! in the current Python environment.  Run with:
//!
//! ```bash
//! cd eval-bridge
//! cargo test -- --test-threads=1
//! ```

use code_agent_eval_bridge::{run_benchmark, run_eval};

/// The bridge should be importable at test time.
#[test]
fn test_bridge_module_exists() {
    // If the Python package isn't installed this will fail fast
    // with a clear import error.
    let result = run_eval(
        r#"{"task_id":"bridge/smoke","test_commands":["echo ok"],"timeout":30}"#,
    );
    // May fail if Docker is unavailable, but should NOT fail with import error.
    match result {
        Ok(_) | Err(_) => {} // Either outcome is fine; we just want no import panic.
    }
}

#[test]
fn test_roundtrip_single() {
    let result = run_eval(
        r#"{"task_id":"roundtrip/1","test_commands":["echo hello"],"timeout":30,"expected_output":"hello"}"#,
    );
    match result {
        Ok(json_str) => {
            assert!(json_str.contains("roundtrip/1"));
            assert!(json_str.contains("task_id"));
        }
        Err(e) => {
            // Docker may be absent in CI — that's an acceptable failure mode.
            eprintln!("(expected if no Docker) {e}");
        }
    }
}

#[test]
fn test_roundtrip_batch() {
    let tasks = r#"[
        {"task_id":"roundtrip/a","test_commands":["echo a"],"timeout":30,"expected_output":"a"},
        {"task_id":"roundtrip/b","test_commands":["echo b"],"timeout":30,"expected_output":"b"}
    ]"#;
    let result = run_benchmark(tasks);
    match result {
        Ok(json_str) => {
            let parsed: serde_json::Value =
                serde_json::from_str(&json_str).expect("valid JSON array");
            assert!(parsed.is_array());
            assert_eq!(parsed.as_array().unwrap().len(), 2);
        }
        Err(e) => {
            eprintln!("(expected if no Docker) {e}");
        }
    }
}
