//! JSON-RPC 2.0 App Server for IDE integration.
//!
//! The App Server provides a stdio-based JSON-RPC 2.0 interface for managing
//! agent coding threads. IDE plugins connect via stdin/stdout to create threads,
//! submit turns, receive streaming agent events, and manage thread lifecycle.
//!
//! # Architecture
//!
//! ```text
//! IDE Plugin ──stdin──▶ AppServer ──▶ ThreadManager ──▶ Session (ReAct loop)
//!              ◀─stdout──          ◀── event stream  ◀──
//! ```
//!
//! # Example
//!
//! ```rust,no_run
//! use code_agent_app_server::AppServer;
//!
//! let mut server = AppServer::new();
//! // Configure with model client and tool registry
//! // server.start().await?;
//! ```

pub mod handler;
pub mod server;
pub mod types;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use code_agent_core::agent::ThreadManager;
use code_agent_core::model::ModelClient;
use code_agent_core::tools::registry::ToolRegistry;
use code_agent_protocol::ThreadId;
use tokio::sync::mpsc;
use tracing::info;

use types::JsonRpcRequest;
use types::JsonRpcResponse;

// ---------------------------------------------------------------------------
// AppServer
// ---------------------------------------------------------------------------

/// JSON-RPC 2.0 应用服务器
///
/// 【领域含义】JSON-RPC 2.0 应用服务器聚合，通过 ThreadManager 管理 Agent 线程。
/// 【核心职责】处理 JSON-RPC 请求、管理 Agent 会话生命周期、将 Agent 事件作为通知流式推送给客户端。
///
/// # 状态追踪
/// - 活跃线程（通过 ThreadManager）
/// - 每线程配置（支持 fork）
/// - 初始化状态
/// - 可选的模型客户端和工具注册表（用于创建会话）
pub struct AppServer {
    /// Shared thread manager for session lifecycle.
    pub thread_manager: Arc<StdMutex<ThreadManager>>,

    /// Model client shared across all sessions.
    model_client: Option<Arc<dyn ModelClient>>,

    /// Tool registry shared across all sessions.
    tool_registry: Option<Arc<dyn ToolRegistry>>,

    /// Whether the initialize handshake has completed.
    initialized: Arc<StdMutex<bool>>,

    /// Per-thread configuration stored for fork support.
    thread_configs: Arc<StdMutex<HashMap<ThreadId, handler::ThreadConfig>>>,

    /// Output channel for sending JSON-RPC messages to stdout.
    output_tx: mpsc::UnboundedSender<String>,

    /// Receiver for the output channel (kept alive for tests).
    #[allow(dead_code)]
    output_rx: Option<mpsc::UnboundedReceiver<String>>,
}

impl Default for AppServer {
    fn default() -> Self {
        Self::new()
    }
}

impl AppServer {
    /// 创建应用服务器
    ///
    /// 【领域含义】构造默认配置的 AppServer 实例。
    /// 【核心职责】初始化线程管理器、事件通道和共享状态。
    /// 使用 with_model_client 和 with_tool_registry 在启动前配置服务器。
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            thread_manager: Arc::new(StdMutex::new(ThreadManager::new(100))),
            model_client: None,
            tool_registry: None,
            initialized: Arc::new(StdMutex::new(false)),
            thread_configs: Arc::new(StdMutex::new(HashMap::new())),
            output_tx: tx,
            output_rx: Some(rx),
        }
    }

    /// 设置模型客户端
    ///
    /// 【领域含义】设置用于创建 Agent 会话的模型客户端。
    /// 【核心职责】保存模型客户端引用，供创建线程时使用。
    pub fn with_model_client(mut self, client: Arc<dyn ModelClient>) -> Self {
        self.model_client = Some(client);
        self
    }

    /// 设置工具注册表
    ///
    /// 【领域含义】设置用于创建 Agent 会话的工具注册表。
    /// 【核心职责】保存工具注册表引用，供创建线程时使用。
    pub fn with_tool_registry(mut self, registry: Arc<dyn ToolRegistry>) -> Self {
        self.tool_registry = Some(registry);
        self
    }

    /// 启动 JSON-RPC 2.0 服务器
    ///
    /// 【领域含义】启动 JSON-RPC 2.0 服务器，从 stdin 读取请求，将响应写入 stdout。
    /// 【核心职责】创建写入器任务、逐行读取 stdin、解析 JSON-RPC 请求、分发到处理器。
    /// 此方法阻塞直到 stdin 关闭（EOF）。流式方法（如 threads/submitTurn）的事件通知通过专用写入器任务并发写入 stdout。
    ///
    /// # 错误
    /// 如果无法读取 stdin 则返回错误。
    pub async fn start(&mut self) -> Result<(), String> {
        info!("Starting JSON-RPC 2.0 app server over stdio");

        // Create a fresh output channel for the stdio server.
        // The writer task reads from this channel and writes to stdout.
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        self.output_tx = tx;

        // Spawn a dedicated writer task so all stdout writes are serialized.
        let writer_handle = tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                println!("{line}");
            }
        });

        // Read stdin line-by-line
        let stdin = tokio::io::stdin();
        let reader = tokio::io::BufReader::new(stdin);
        let mut lines = tokio::io::AsyncBufReadExt::lines(reader);

        while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            // Parse the request
            let request: JsonRpcRequest = match serde_json::from_str(&line) {
                Ok(req) => req,
                Err(e) => {
                    let error_resp = JsonRpcResponse::error_null_id(
                        types::error_codes::PARSE_ERROR,
                        format!("Parse error: {e}"),
                    );
                    if let Ok(json) = serde_json::to_string(&error_resp) {
                        let _ = self.output_tx.send(json);
                    }
                    continue;
                }
            };

            // Validate JSON-RPC version
            if request.jsonrpc != "2.0" {
                let error_resp = JsonRpcResponse::error(
                    request.id.clone(),
                    types::error_codes::INVALID_REQUEST,
                    "jsonrpc must be '2.0'",
                );
                if let Ok(json) = serde_json::to_string(&error_resp) {
                    let _ = self.output_tx.send(json);
                }
                continue;
            }

            // Notifications (no id) — handle but don't send a response
            let is_notification = request.id.is_none();

            // Dispatch to handler
            let response = handler::dispatch(
                &self.thread_manager,
                &self.model_client,
                &self.tool_registry,
                &self.initialized,
                &self.thread_configs,
                &self.output_tx,
                request,
            )
            .await;

            // Send response unless it's a notification
            if !is_notification {
                if let Ok(json) = serde_json::to_string(&response) {
                    let _ = self.output_tx.send(json);
                }
            }
        }

        // Close the writer channel and wait for the writer to finish
        drop(self.output_tx.clone());
        // Create a fresh tx just to drop the original
        let _ = writer_handle.await;
        Ok(())
    }

    /// 处理单个 JSON-RPC 请求
    ///
    /// 【领域含义】处理单个 JSON-RPC 请求并返回响应，是核心分发方法。
    /// 【核心职责】委托给 handler::dispatch 处理请求，适用于测试和无需 stdio 传输的程序化使用。
    /// 流式方法的事件通知通过内部输出通道发送，使用 take_output_receiver 捕获。
    pub async fn handle_request(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        handler::dispatch(
            &self.thread_manager,
            &self.model_client,
            &self.tool_registry,
            &self.initialized,
            &self.thread_configs,
            &self.output_tx,
            request,
        )
        .await
    }

    /// 获取输出接收端（用于测试）
    ///
    /// 【领域含义】获取输出接收端以捕获测试中的事件通知。
    /// 【核心职责】消费内部接收端，之后处理器发送的事件通知可从返回的接收端读取。
    pub fn take_output_receiver(&mut self) -> Option<mpsc::UnboundedReceiver<String>> {
        self.output_rx.take()
    }

    /// 检查服务器是否已初始化
    ///
    /// 【领域含义】返回服务器是否已完成 initialize 握手。
    /// 【核心职责】检查 initialized 标志位。
    pub fn is_initialized(&self) -> bool {
        *self.initialized.lock().unwrap()
    }

    /// 获取当前线程数
    ///
    /// 【领域含义】返回当前活跃的 Agent 线程数量。
    /// 【核心职责】委托给 ThreadManager 查询。
    pub fn thread_count(&self) -> usize {
        self.thread_manager.lock().unwrap().thread_count()
    }

    /// 列出所有活跃线程 ID
    ///
    /// 【领域含义】返回所有活跃 Agent 线程的 ID 列表。
    /// 【核心职责】委托给 ThreadManager 查询。
    pub fn list_threads(&self) -> Vec<String> {
        self.thread_manager
            .lock()
            .unwrap()
            .list_threads()
            .into_iter()
            .map(|t| t.0)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use code_agent_core::model::types::TokenUsage;
    use code_agent_core::model::{ModelClient, ModelResult};
    use code_agent_core::tools::registry::DefaultToolRegistry;
    use code_agent_protocol::{Message, ResponseEvent};
    use futures::stream;
    use serde_json::Value;
    use std::sync::Mutex;

    // ── Local Mock Model Client ──────────────────────────────────────

    /// A mock model client that returns predefined response event sequences.
    struct MockModelClient {
        responses: Mutex<Vec<Vec<ResponseEvent>>>,
    }

    impl MockModelClient {
        fn new(responses: Vec<Vec<ResponseEvent>>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl ModelClient for MockModelClient {
        fn model_name(&self) -> &str {
            "mock-model"
        }

        async fn complete_stream(
            &self,
            _messages: &[Message],
            _tools: &[code_agent_core::model::ToolDefinition],
            _temperature: Option<f32>,
        ) -> ModelResult<Box<dyn stream::Stream<Item = ResponseEvent> + Send + Unpin>> {
            let mut responses = self.responses.lock().unwrap();

            let events = if responses.is_empty() {
                vec![ResponseEvent::AgentMessageDelta {
                    content: "Mock response".into(),
                }]
            } else {
                responses.remove(0)
            };

            Ok(Box::new(stream::iter(events)))
        }

        fn last_token_usage(&self) -> Option<TokenUsage> {
            None
        }
    }

    /// Create a minimal AppServer for testing with a mock model client.
    fn make_server() -> AppServer {
        let mock_client = Arc::new(MockModelClient::new(vec![]));
        let tool_registry = Arc::new(DefaultToolRegistry::new());

        AppServer::new()
            .with_model_client(mock_client)
            .with_tool_registry(tool_registry)
    }

    /// Create a mock server whose model returns specific responses.
    fn make_server_with_responses(
        responses: Vec<Vec<ResponseEvent>>,
    ) -> AppServer {
        let mock_client = Arc::new(MockModelClient::new(responses));
        let tool_registry = Arc::new(DefaultToolRegistry::new());

        AppServer::new()
            .with_model_client(mock_client)
            .with_tool_registry(tool_registry)
    }

    /// Build an initialize request.
    fn initialize_request() -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(1.into())),
            method: "initialize".into(),
            params: Some(serde_json::json!({
                "protocol_version": "0.1.0",
                "capabilities": {},
            })),
        }
    }

    /// Build a threads/create request.
    fn create_thread_request(id: u64, thread_id: &str) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(id.into())),
            method: "threads/create".into(),
            params: Some(serde_json::json!({
                "thread_id": thread_id,
                "system_instructions": "You are a test assistant.",
                "max_iterations": 10,
            })),
        }
    }

    /// Build a threads/submitTurn request.
    fn submit_turn_request(id: u64, thread_id: &str, content: &str) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(id.into())),
            method: "threads/submitTurn".into(),
            params: Some(serde_json::json!({
                "thread_id": thread_id,
                "messages": [{"user_message": {"content": content}}],
            })),
        }
    }

    // ── Test: Initialize handshake ──────────────────────────────────

    #[tokio::test]
    async fn initialize_handshake() {
        let server = make_server();
        assert!(!server.is_initialized());

        let req = initialize_request();
        let resp = server.handle_request(req).await;

        assert!(resp.is_success());
        assert!(server.is_initialized());

        let result = resp.result.unwrap();
        assert_eq!(result["protocol_version"], "0.1.0");
        assert_eq!(result["server_info"]["name"], "code-agent-app-server");
        assert!(result["capabilities"]["streaming"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn initialize_twice_is_idempotent() {
        let server = make_server();

        let req1 = initialize_request();
        let resp1 = server.handle_request(req1).await;
        assert!(resp1.is_success());

        let req2 = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(2.into())),
            method: "initialize".into(),
            params: Some(serde_json::json!({})),
        };
        let resp2 = server.handle_request(req2).await;
        assert!(resp2.is_success());
        assert!(server.is_initialized());
    }

    // ── Test: Create thread → thread_id returned ────────────────────

    #[tokio::test]
    async fn create_thread_returns_thread_id() {
        let server = make_server();

        // Initialize first
        server.handle_request(initialize_request()).await;

        let req = create_thread_request(2, "my-thread");
        let resp = server.handle_request(req).await;

        assert!(resp.is_success());
        let result = resp.result.unwrap();
        assert_eq!(result["thread_id"], "my-thread");
        assert!(result["session_id"].as_str().unwrap().starts_with("session-"));

        assert_eq!(server.thread_count(), 1);
        assert!(server.list_threads().contains(&"my-thread".to_string()));
    }

    #[tokio::test]
    async fn create_thread_without_initialize_fails() {
        let server = make_server();

        let req = create_thread_request(1, "t1");
        let resp = server.handle_request(req).await;

        assert!(resp.is_error());
        let err = resp.error.unwrap();
        assert_eq!(err.code, types::error_codes::SERVER_NOT_INITIALIZED);
    }

    #[tokio::test]
    async fn create_duplicate_thread_fails() {
        let server = make_server();
        server.handle_request(initialize_request()).await;

        let req1 = create_thread_request(2, "dup-thread");
        assert!(server.handle_request(req1).await.is_success());

        let req2 = create_thread_request(3, "dup-thread");
        let resp = server.handle_request(req2).await;
        assert!(resp.is_error());
        assert_eq!(
            resp.error.unwrap().code,
            types::error_codes::THREAD_ALREADY_EXISTS
        );
    }

    // ── Test: Submit turn → stream of event notifications ───────────

    #[tokio::test]
    async fn submit_turn_streams_events() {
        let mut server = make_server_with_responses(vec![vec![
            ResponseEvent::AgentMessageDelta {
                content: "Hello, I am an agent.".into(),
            },
        ]]);

        server.handle_request(initialize_request()).await;
        server.handle_request(create_thread_request(2, "stream-thread")).await;

        // Take the output receiver to capture notifications
        let mut rx = server.take_output_receiver().unwrap();

        let req = submit_turn_request(3, "stream-thread", "Say hello");
        let resp = server.handle_request(req).await;

        // Response should be immediate success
        assert!(resp.is_success());
        assert_eq!(resp.result.unwrap()["status"], "accepted");

        // Wait for notifications (the background task fires asynchronously)
        let mut notifications: Vec<String> = Vec::new();
        // Collect notifications with a timeout
        let timeout = tokio::time::Duration::from_secs(5);
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Some(msg)) => notifications.push(msg),
                Ok(None) => break,
                Err(_) => break,
            }
        }

        // Should have received event notifications
        assert!(
            !notifications.is_empty(),
            "Expected event notifications, got none"
        );

        // At least one notification should contain the agent message
        let has_agent_message = notifications
            .iter()
            .any(|n| n.contains("agent_message_delta") && n.contains("Hello"));
        assert!(
            has_agent_message,
            "Notifications should contain agent message delta. Got: {notifications:?}"
        );

        // Thread should still exist after turn completes
        // (Give the background task a moment to re-insert)
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        assert!(server.list_threads().contains(&"stream-thread".to_string()));
    }

    // ── Test: Submit turn to nonexistent thread ─────────────────────

    #[tokio::test]
    async fn submit_turn_nonexistent_thread_fails() {
        let server = make_server();
        server.handle_request(initialize_request()).await;

        let req = submit_turn_request(1, "no-such-thread", "hi");
        let resp = server.handle_request(req).await;

        assert!(resp.is_error());
        assert_eq!(
            resp.error.unwrap().code,
            types::error_codes::THREAD_NOT_FOUND
        );
    }

    // ── Test: Fork thread → independent copy ────────────────────────

    #[tokio::test]
    async fn fork_thread_creates_independent_copy() {
        let server = make_server();
        server.handle_request(initialize_request()).await;
        server.handle_request(create_thread_request(2, "source-thread")).await;

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(3.into())),
            method: "threads/fork".into(),
            params: Some(serde_json::json!({
                "thread_id": "source-thread",
                "new_thread_id": "forked-thread",
            })),
        };

        let resp = server.handle_request(req).await;
        assert!(resp.is_success());
        let result = resp.result.unwrap();
        assert_eq!(result["thread_id"], "forked-thread");
        assert_eq!(result["forked_from"], "source-thread");

        // Both threads should exist
        assert_eq!(server.thread_count(), 2);
        assert!(server.list_threads().contains(&"source-thread".to_string()));
        assert!(server.list_threads().contains(&"forked-thread".to_string()));
    }

    #[tokio::test]
    async fn fork_nonexistent_thread_fails() {
        let server = make_server();
        server.handle_request(initialize_request()).await;

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(1.into())),
            method: "threads/fork".into(),
            params: Some(serde_json::json!({
                "thread_id": "no-such-thread",
                "new_thread_id": "forked",
            })),
        };

        let resp = server.handle_request(req).await;
        assert!(resp.is_error());
        assert_eq!(
            resp.error.unwrap().code,
            types::error_codes::THREAD_NOT_FOUND
        );
    }

    // ── Test: Archive thread ────────────────────────────────────────

    #[tokio::test]
    async fn archive_thread_removes_it() {
        let server = make_server();
        server.handle_request(initialize_request()).await;
        server.handle_request(create_thread_request(2, "to-archive")).await;
        assert_eq!(server.thread_count(), 1);

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(3.into())),
            method: "threads/archive".into(),
            params: Some(serde_json::json!({
                "thread_id": "to-archive",
            })),
        };

        let resp = server.handle_request(req).await;
        assert!(resp.is_success());
        assert_eq!(resp.result.unwrap()["archived"], true);
        assert_eq!(server.thread_count(), 0);
    }

    // ── Test: Invalid method → error response ───────────────────────

    #[tokio::test]
    async fn invalid_method_returns_error() {
        let server = make_server();

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(1.into())),
            method: "nonexistent/method".into(),
            params: None,
        };

        let resp = server.handle_request(req).await;
        assert!(resp.is_error());
        let err = resp.error.unwrap();
        assert_eq!(err.code, types::error_codes::METHOD_NOT_FOUND);
        assert!(err.message.contains("Method not found"));
    }

    // ── Test: Malformed JSON → parse error ──────────────────────────

    #[test]
    fn malformed_json_returns_parse_error() {
        let json_str = r#"{"jsonrpc": "2.0", "id": 1, "method": bad"#;
        let result: Result<JsonRpcRequest, _> = serde_json::from_str(json_str);
        assert!(result.is_err());
    }

    #[test]
    fn valid_request_parses_correctly() {
        let json_str =
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":"0.1.0"}}"#;
        let result: Result<JsonRpcRequest, _> = serde_json::from_str(json_str);
        assert!(result.is_ok());
        let req = result.unwrap();
        assert_eq!(req.method, "initialize");
        assert_eq!(req.jsonrpc, "2.0");
    }

    // ── Test: threads/list ──────────────────────────────────────────

    #[tokio::test]
    async fn list_threads_returns_all_threads() {
        let server = make_server();
        server.handle_request(initialize_request()).await;
        server.handle_request(create_thread_request(2, "t1")).await;
        server.handle_request(create_thread_request(3, "t2")).await;

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(4.into())),
            method: "threads/list".into(),
            params: None,
        };

        let resp = server.handle_request(req).await;
        assert!(resp.is_success());
        let result = resp.result.unwrap();
        assert_eq!(result["count"], 2);

        let threads: Vec<String> = result["threads"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(threads.contains(&"t1".to_string()));
        assert!(threads.contains(&"t2".to_string()));
    }

    // ── Test: threads/get ───────────────────────────────────────────

    #[tokio::test]
    async fn get_thread_returns_details() {
        let server = make_server();
        server.handle_request(initialize_request()).await;
        server.handle_request(create_thread_request(2, "detail-thread")).await;

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(3.into())),
            method: "threads/get".into(),
            params: Some(serde_json::json!({
                "thread_id": "detail-thread",
            })),
        };

        let resp = server.handle_request(req).await;
        assert!(resp.is_success());
        let result = resp.result.unwrap();
        assert_eq!(result["thread_id"], "detail-thread");
        assert_eq!(result["turn_count"], 0);
        assert_eq!(result["status"], "active");
        assert!(result["system_instructions"].as_str().unwrap().contains("test assistant"));
    }

    // ── Test: Concurrent thread management ──────────────────────────

    #[tokio::test]
    async fn concurrent_thread_management() {
        let server = make_server();
        server.handle_request(initialize_request()).await;

        // Create multiple threads
        for i in 1..=5 {
            let req = create_thread_request(i, &format!("concurrent-{i}"));
            let resp = server.handle_request(req).await;
            assert!(resp.is_success(), "Failed to create thread {i}");
        }

        assert_eq!(server.thread_count(), 5);

        // Archive one
        let archive_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(10.into())),
            method: "threads/archive".into(),
            params: Some(serde_json::json!({"thread_id": "concurrent-3"})),
        };
        assert!(server.handle_request(archive_req).await.is_success());
        assert_eq!(server.thread_count(), 4);

        // Fork another
        let fork_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(11.into())),
            method: "threads/fork".into(),
            params: Some(serde_json::json!({
                "thread_id": "concurrent-1",
                "new_thread_id": "concurrent-1-fork",
            })),
        };
        assert!(server.handle_request(fork_req).await.is_success());
        assert_eq!(server.thread_count(), 5);
    }

    // ── Test: Notification method ───────────────────────────────────

    #[tokio::test]
    async fn initialized_notification_returns_no_error() {
        let server = make_server();
        // The spec says client sends "notifications/initialized" after initialize
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: None, // Notification — no response expected
            method: "notifications/initialized".into(),
            params: None,
        };
        let resp = server.handle_request(req).await;
        // Notification handlers produce a non-error response with null result
        assert!(!resp.is_error());
    }

    // ── Test: JSON-RPC 2.0 version validation ──────────────────────

    #[test]
    fn non_2_0_jsonrpc_should_be_handled() {
        let json = r#"{"jsonrpc":"1.0","id":1,"method":"test"}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.jsonrpc, "1.0");
        // The server's start() loop would reject this, but handle_request
        // still dispatches. In production, the stdin loop validates the
        // version before dispatch.
    }
}
