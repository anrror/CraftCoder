//! Mock LSP 服务器二进制文件（集成测试用）
//!
//! 【领域含义】实现一个最小化的 LSP 服务器，对常见请求返回预设的固定响应。
//! 通过 stdin/stdout 使用 JSON-RPC 2.0 和 Content-Length 帧协议通信。
//!
//! 【核心职责】用于 `tests/lsp_integration_tests.rs` 测试 LSP 客户端，无需真实语言服务器。

use std::io::{BufRead, BufReader, Read, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    let reader = BufReader::new(stdin.lock());

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => break,
        };

        let trimmed = line.trim_end_matches(['\r', '\n']);

        if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
            let content_length: usize = len_str.trim().parse().unwrap_or(0);
            let mut body = vec![0u8; content_length];
            stdin.lock().read_exact(&mut body).expect("failed to read body");

            let request: serde_json::Value =
                serde_json::from_slice(&body).expect("failed to parse request");

            let response = handle_request(&request);

            // Write response (null responses are for notifications)
            if !response.is_null() {
                let resp_body =
                    serde_json::to_vec(&response).expect("failed to serialize response");
                let header = format!("Content-Length: {}\r\n\r\n", resp_body.len());
                stdout.write_all(header.as_bytes()).unwrap();
                stdout.write_all(&resp_body).unwrap();
                stdout.flush().unwrap();
            }

            // Exit notification terminates the server
            if request.get("method").and_then(|m| m.as_str()) == Some("exit") {
                break;
            }
        }
    }
}

/// 处理 LSP 请求并返回预设响应
///
/// 【领域含义】根据请求的方法名返回对应的预设 JSON-RPC 响应。
/// 【核心职责】匹配方法名 → 构建预设响应 → 返回 JSON Value。
fn handle_request(request: &serde_json::Value) -> serde_json::Value {
    let method = request
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or("");
    let id = request.get("id").cloned();

    let is_notification = id.is_none() || id.as_ref() == Some(&serde_json::Value::Null);

    match (method, is_notification) {
        ("initialize", false) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "capabilities": {
                    "textDocumentSync": 1,
                    "hoverProvider": true,
                    "completionProvider": { "triggerCharacters": ["."] },
                    "definitionProvider": true,
                    "referencesProvider": true,
                    "documentSymbolProvider": true,
                    "diagnosticProvider": {
                        "interFileDependencies": false,
                        "workspaceDiagnostics": false
                    }
                },
                "serverInfo": {
                    "name": "mock-lsp-server",
                    "version": "0.1.0"
                }
            }
        }),

        ("shutdown", false) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": null
        }),

        ("textDocument/definition", false) => {
            let uri = param_str(request, "textDocument/uri", "file:///unknown");
            let line = param_u64(request, "position/line", 0);
            let character = param_u64(request, "position/character", 0);

            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": [{
                    "uri": uri,
                    "range": {
                        "start": {"line": line, "character": character},
                        "end": {"line": line, "character": character + 10}
                    }
                }]
            })
        }

        ("textDocument/references", false) => {
            let uri = param_str(request, "textDocument/uri", "file:///unknown");
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": [
                    {
                        "uri": uri,
                        "range": {
                            "start": {"line": 5, "character": 0},
                            "end": {"line": 5, "character": 8}
                        }
                    },
                    {
                        "uri": uri,
                        "range": {
                            "start": {"line": 12, "character": 4},
                            "end": {"line": 12, "character": 12}
                        }
                    }
                ]
            })
        }

        ("textDocument/hover", false) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "contents": {
                    "kind": "markdown",
                    "value": "```rust\n/// A mock function.\npub fn mock_function() -> String\n```"
                }
            }
        }),

        ("textDocument/documentSymbol", false) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": [
                {
                    "name": "main",
                    "kind": 12,
                    "range": { "start": {"line": 0, "character": 0}, "end": {"line": 5, "character": 1} },
                    "selectionRange": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 4} }
                },
                {
                    "name": "MyStruct",
                    "kind": 23,
                    "range": { "start": {"line": 7, "character": 0}, "end": {"line": 10, "character": 1} },
                    "selectionRange": { "start": {"line": 7, "character": 0}, "end": {"line": 7, "character": 8} }
                }
            ]
        }),

        ("textDocument/didOpen", true) => {
            let uri = param_str(request, "textDocument/uri", "file:///unknown");

            // Send publishDiagnostics notification
            let notif = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "textDocument/publishDiagnostics",
                "params": {
                    "uri": uri,
                    "diagnostics": [
                        {
                            "range": { "start": {"line": 0, "character": 14}, "end": {"line": 0, "character": 16} },
                            "severity": 1,
                            "message": "expected expression, found `;`",
                            "source": "mock"
                        },
                        {
                            "range": { "start": {"line": 0, "character": 5}, "end": {"line": 0, "character": 9} },
                            "severity": 2,
                            "message": "unused variable: `main`",
                            "source": "mock"
                        }
                    ]
                }
            });

            let body = serde_json::to_vec(&notif).unwrap();
            let header = format!("Content-Length: {}\r\n\r\n", body.len());
            let mut stdout = std::io::stdout();
            stdout.write_all(header.as_bytes()).unwrap();
            stdout.write_all(&body).unwrap();
            stdout.flush().unwrap();

            serde_json::Value::Null
        }

        ("textDocument/diagnostic", false) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "kind": "full",
                "items": [{
                    "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 5} },
                    "severity": 1,
                    "message": "mock error",
                    "source": "mock"
                }]
            }
        }),

        // Notifications and unknown methods
        (_, true) => serde_json::Value::Null,
        (_, false) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("Method not found: {method}")
            }
        }),
    }
}

/// 提取嵌套字符串参数（如 `textDocument/uri`）
///
/// 【领域含义】从 JSON-RPC 请求的 `params` 中按路径提取字符串参数。
/// 【核心职责】按 `/` 分隔的路径逐层访问 → 返回字符串值或默认值。
fn param_str(request: &serde_json::Value, path: &str, default: &str) -> String {
    let mut current = &request["params"];
    for segment in path.split('/') {
        current = &current[segment];
    }
    current.as_str().unwrap_or(default).to_string()
}

/// 提取嵌套 u64 参数
///
/// 【领域含义】从 JSON-RPC 请求的 `params` 中按路径提取 u64 数字参数。
/// 【核心职责】按 `/` 分隔的路径逐层访问 → 返回 u64 值或默认值。
fn param_u64(request: &serde_json::Value, path: &str, default: u64) -> u64 {
    let mut current = &request["params"];
    for segment in path.split('/') {
        current = &current[segment];
    }
    current.as_u64().unwrap_or(default)
}
