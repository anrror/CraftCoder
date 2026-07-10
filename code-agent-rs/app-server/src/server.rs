//! JSON-RPC 2.0 server transport over stdio.
//!
//! The server reads JSON-RPC requests from stdin (one JSON object per line)
//! and writes responses and event notifications to stdout. A dedicated writer
//! task ensures that concurrent event emission from background turn tasks does
//! not interleave with response writes.

use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use crate::handler;
use crate::types::*;

/// 运行 stdio JSON-RPC 2.0 服务器
///
/// 【领域含义】通过 stdio 运行 JSON-RPC 2.0 服务器传输层。
/// 【核心职责】读取 stdin 的 JSON-RPC 请求（每行一个 JSON 对象），将响应和事件通知写入 stdout。
/// 专用写入器任务确保后台轮次任务的并发事件发射不会与响应写入交错。
///
/// # 参数
/// * `thread_manager` — 共享的线程管理器，用于会话生命周期管理。
/// * `model_client` — 可选的模型客户端，用于创建 Agent 会话。
/// * `tool_registry` — 可选的工具注册表，用于创建 Agent 会话。
/// * `initialized` — 共享的初始化标志。
/// * `thread_configs` — 每线程配置，用于 fork 支持。
///
/// 此函数阻塞直到 stdin 关闭（EOF）。
pub async fn run_stdio_server(
    thread_manager: Arc<StdMutex<code_agent_core::agent::ThreadManager>>,
    model_client: Option<Arc<dyn code_agent_core::model::ModelClient>>,
    tool_registry: Option<Arc<code_agent_core::tools::registry::ToolRegistry>>,
    initialized: Arc<StdMutex<bool>>,
    thread_configs: Arc<
        StdMutex<
            std::collections::HashMap<
                code_agent_protocol::ThreadId,
                handler::ThreadConfig,
            >,
        >,
    >,
) {
    // Channel for all stdout writes (responses + event notifications)
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // Spawn a dedicated writer task so all stdout writes are serialized.
    let writer_handle = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            println!("{line}");
        }
    });

    // Read stdin line-by-line
    let stdin = tokio::io::stdin();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break, // EOF
            Err(e) => {
                tracing::error!(error = %e, "Failed to read from stdin");
                break;
            }
        };

        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        // Parse the request
        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                let error_resp = JsonRpcResponse::error_null_id(
                    error_codes::PARSE_ERROR,
                    format!("Parse error: {e}"),
                );
                if let Ok(json) = serde_json::to_string(&error_resp) {
                    let _ = tx.send(json);
                }
                continue;
            }
        };

        // Check if valid JSON-RPC
        if request.jsonrpc != "2.0" {
            let error_resp = JsonRpcResponse::error(
                request.id.clone(),
                error_codes::INVALID_REQUEST,
                "jsonrpc must be '2.0'",
            );
            if let Ok(json) = serde_json::to_string(&error_resp) {
                let _ = tx.send(json);
            }
            continue;
        }

        // Notifications (no id) — handle but don't send response
        let is_notification = request.id.is_none();

        // Dispatch to handler
        let response = handler::dispatch(
            &thread_manager,
            &model_client,
            &tool_registry,
            &initialized,
            &thread_configs,
            &tx,
            request,
        )
        .await;

        // Send response (unless it was a notification or has a null-id from a non-notification handler)
        if !is_notification || response.id.is_some() {
            if let Ok(json) = serde_json::to_string(&response) {
                let _ = tx.send(json);
            }
        }
    }

    // Close the writer channel and wait for the writer to finish
    drop(tx);
    let _ = writer_handle.await;
}

/// 发送原始 JSON-RPC 消息到 stdout
///
/// 【领域含义】发送原始 JSON-RPC 消息到 stdout（用于测试和外部调用方）。
/// 【核心职责】序列化消息为 JSON 字符串并通过通道发送。
pub fn send_message(tx: &mpsc::UnboundedSender<String>, message: &Value) {
    if let Ok(json) = serde_json::to_string(message) {
        let _ = tx.send(json);
    }
}
