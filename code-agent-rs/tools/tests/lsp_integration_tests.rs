//! Integration tests for the LSP client using the mock LSP server binary.
//!
//! The mock server binary (`mock-lsp-server`) is located via Cargo's
//! `CARGO_BIN_EXE_*` environment variable, which is set automatically
//! during `cargo test`.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Spawn a mock LSP server process.
fn spawn_mock_server() -> std::process::Child {
    let exe = env!("CARGO_BIN_EXE_mock-lsp-server");
    Command::new(exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn mock LSP server")
}

/// Send a JSON-RPC message (with Content-Length header).
fn send_msg(stdin: &mut std::process::ChildStdin, value: &serde_json::Value) {
    let body = serde_json::to_vec(value).expect("failed to serialize JSON");
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin.write_all(header.as_bytes()).unwrap();
    stdin.write_all(&body).unwrap();
    stdin.flush().unwrap();
}

/// Receive a JSON-RPC message (with Content-Length header).
fn recv_msg(stdout: &mut BufReader<std::process::ChildStdout>) -> serde_json::Value {
    let mut line = String::new();
    loop {
        line.clear();
        let n = stdout.read_line(&mut line).unwrap();
        assert!(n > 0, "EOF while reading header");
        let trimmed = line.trim_end_matches(|c| c == '\r' || c == '\n');
        if trimmed.is_empty() {
            continue;
        }
        if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
            let content_length: usize = len_str.trim().parse().unwrap();
            let mut body = vec![0u8; content_length];
            stdout.read_exact(&mut body).unwrap();
            return serde_json::from_slice(&body).expect("failed to parse JSON response");
        }
    }
}

/// Perform init handshake, return initialized stream handles.
fn init_server(
    child: &mut std::process::Child,
) -> (
    std::process::ChildStdin,
    BufReader<std::process::ChildStdout>,
) {
    let stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // Send initialize
    send_msg(&mut stdin.clone(), &serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"processId": null, "rootUri": "file:///test", "capabilities": {}}
    }));

    let init_resp = recv_msg(&mut stdout);
    assert_eq!(init_resp["id"], 1);
    assert!(
        init_resp["result"]["capabilities"]["definitionProvider"]
            .as_bool()
            .unwrap_or(false)
    );

    // Send initialized
    send_msg(&mut stdin.clone(), &serde_json::json!({
        "jsonrpc": "2.0", "method": "initialized", "params": {}
    }));

    (stdin, stdout)
}

/// Shut down the mock server.
fn shutdown_server(stdin: &mut std::process::ChildStdin, child: &mut std::process::Child) {
    send_msg(stdin, &serde_json::json!({"jsonrpc": "2.0", "method": "shutdown", "params": null}));
    send_msg(stdin, &serde_json::json!({"jsonrpc": "2.0", "method": "exit", "params": null}));
    let status = child.wait().unwrap();
    assert!(status.success(), "mock server exited with: {:?}", status);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn initialize_and_definition() {
    let mut child = spawn_mock_server();
    let (mut stdin, mut stdout) = init_server(&mut child);

    send_msg(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "textDocument/definition",
        "params": {
            "textDocument": {"uri": "file:///test/src/main.rs"},
            "position": {"line": 10, "character": 5}
        }
    }));

    let resp = recv_msg(&mut stdout);
    assert_eq!(resp["id"], 2);
    let locations = resp["result"].as_array().unwrap();
    assert!(!locations.is_empty());
    assert_eq!(locations[0]["uri"], "file:///test/src/main.rs");

    shutdown_server(&mut stdin, &mut child);
}

#[test]
fn hover_response() {
    let mut child = spawn_mock_server();
    let (mut stdin, mut stdout) = init_server(&mut child);

    send_msg(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "textDocument/hover",
        "params": {
            "textDocument": {"uri": "file:///test/src/main.rs"},
            "position": {"line": 3, "character": 10}
        }
    }));

    let resp = recv_msg(&mut stdout);
    assert_eq!(resp["id"], 2);
    let contents = resp["result"]["contents"]["value"].as_str().unwrap();
    assert!(contents.contains("mock"));

    shutdown_server(&mut stdin, &mut child);
}

#[test]
fn references_response() {
    let mut child = spawn_mock_server();
    let (mut stdin, mut stdout) = init_server(&mut child);

    send_msg(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "textDocument/references",
        "params": {
            "textDocument": {"uri": "file:///test/src/main.rs"},
            "position": {"line": 5, "character": 3},
            "context": {"includeDeclaration": true}
        }
    }));

    let resp = recv_msg(&mut stdout);
    assert_eq!(resp["id"], 2);
    let refs = resp["result"].as_array().unwrap();
    assert_eq!(refs.len(), 2);

    shutdown_server(&mut stdin, &mut child);
}

#[test]
fn unsupported_method_returns_error() {
    let mut child = spawn_mock_server();
    let (mut stdin, mut stdout) = init_server(&mut child);

    send_msg(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0", "id": 99, "method": "textDocument/makeSandwich",
        "params": {}
    }));

    let resp = recv_msg(&mut stdout);
    assert_eq!(resp["id"], 99);
    let error_code = resp["error"]["code"].as_i64().unwrap_or(0);
    assert!(error_code != 0, "expected error, got: {resp}");

    shutdown_server(&mut stdin, &mut child);
}

#[test]
fn document_symbols_response() {
    let mut child = spawn_mock_server();
    let (mut stdin, mut stdout) = init_server(&mut child);

    send_msg(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "textDocument/documentSymbol",
        "params": {
            "textDocument": {"uri": "file:///test/src/lib.rs"}
        }
    }));

    let resp = recv_msg(&mut stdout);
    assert_eq!(resp["id"], 3);
    let symbols = resp["result"].as_array().unwrap();
    assert!(!symbols.is_empty());
    let has_function = symbols.iter().any(|s| s["kind"] == 12);
    assert!(has_function, "expected at least one function symbol");

    shutdown_server(&mut stdin, &mut child);
}

#[test]
fn publish_diagnostics_notification() {
    let mut child = spawn_mock_server();
    let (mut stdin, mut stdout) = init_server(&mut child);

    send_msg(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": "file:///test/src/error.rs",
                "languageId": "rust",
                "version": 1,
                "text": "fn main() { let x = ; }"
            }
        }
    }));

    // Should receive publishDiagnostics notification
    let notif = recv_msg(&mut stdout);
    assert_eq!(notif["method"], "textDocument/publishDiagnostics");
    assert!(notif.get("id").map_or(true, |v| v.is_null()));
    let diags = notif["params"]["diagnostics"].as_array().unwrap();
    assert!(!diags.is_empty());

    shutdown_server(&mut stdin, &mut child);
}

#[test]
fn cold_start_timing() {
    let start = std::time::Instant::now();

    let mut child = spawn_mock_server();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    send_msg(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"processId": null, "rootUri": "file:///test", "capabilities": {}}
    }));

    let _ = recv_msg(&mut stdout);
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(3),
        "cold start took {:?}, expected < 3s",
        elapsed
    );

    send_msg(&mut stdin, &serde_json::json!({"jsonrpc": "2.0", "method": "shutdown", "params": null}));
    send_msg(&mut stdin, &serde_json::json!({"jsonrpc": "2.0", "method": "exit", "params": null}));
    child.wait().unwrap();
}

#[test]
fn multiple_sequential_requests() {
    let mut child = spawn_mock_server();
    let (mut stdin, mut stdout) = init_server(&mut child);

    // Send 3 definition requests back-to-back
    for i in 0..3 {
        let req_id = 10 + i;
        send_msg(&mut stdin, &serde_json::json!({
            "jsonrpc": "2.0", "id": req_id, "method": "textDocument/definition",
            "params": {
                "textDocument": {"uri": format!("file:///test/file{i}.rs")},
                "position": {"line": i as u64, "character": 0}
            }
        }));
    }

    // Read all 3 responses
    for i in 0..3 {
        let resp = recv_msg(&mut stdout);
        assert_eq!(resp["id"], 10 + i);
        assert!(resp["result"].is_array());
    }

    shutdown_server(&mut stdin, &mut child);
}

#[test]
fn diagnostic_pull_request() {
    let mut child = spawn_mock_server();
    let (mut stdin, mut stdout) = init_server(&mut child);

    send_msg(&mut stdin, &serde_json::json!({
        "jsonrpc": "2.0", "id": 5, "method": "textDocument/diagnostic",
        "params": {
            "textDocument": {"uri": "file:///test/src/broken.rs"}
        }
    }));

    let resp = recv_msg(&mut stdout);
    assert_eq!(resp["id"], 5);
    let items = resp["result"]["items"].as_array().unwrap();
    assert!(!items.is_empty());
    assert_eq!(items[0]["severity"], 1);
    assert_eq!(items[0]["message"], "mock error");

    shutdown_server(&mut stdin, &mut child);
}
