//! JSON-RPC method handlers for the App Server.
//!
//! Each public function handles one RPC method. They take shared server state
//! and the parsed request, returning a [`JsonRpcResponse`]. Handlers that
//! produce streaming events (like `threads/submitTurn`) accept an output
//! channel for sending notification lines.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use code_agent_core::agent::{Session, SessionConfig, ThreadManager};
use code_agent_core::model::ModelClient;
use code_agent_core::tools::registry::ToolRegistry;
use code_agent_protocol::{
    PermissionMode, SessionId, ThreadId, TurnInput,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::types::*;

// ---------------------------------------------------------------------------
// ThreadConfig — stored per-thread for fork support
// ---------------------------------------------------------------------------

/// 线程配置
///
/// 【领域含义】用于创建（或重新创建）线程会话的配置值对象。
/// 【核心职责】与会话一起存储在 AppServer 中，使 threads/fork 可以使用相同设置创建独立副本。
#[derive(Clone)]
pub struct ThreadConfig {
    #[allow(dead_code)]
    pub session_id: SessionId,
    pub system_instructions: String,
    pub max_iterations: usize,
    pub permission_mode: PermissionMode,
    pub max_context_tokens: Option<usize>,
}

// ---------------------------------------------------------------------------
// Handler dispatch
// ---------------------------------------------------------------------------

/// 分发 JSON-RPC 请求到对应的处理器
///
/// 【领域含义】根据请求方法名将 JSON-RPC 请求分发到对应的处理器函数。
/// 【核心职责】匹配方法名并调用对应的 handle_* 函数，流式方法的事件通知通过 output_tx 发送。
#[allow(clippy::too_many_arguments)]
pub async fn dispatch(
    thread_manager: &Arc<StdMutex<ThreadManager>>,
    model_client: &Option<Arc<dyn ModelClient>>,
    tool_registry: &Option<Arc<dyn ToolRegistry>>,
    initialized: &Arc<StdMutex<bool>>,
    thread_configs: &Arc<StdMutex<HashMap<ThreadId, ThreadConfig>>>,
    output_tx: &mpsc::UnboundedSender<String>,
    request: JsonRpcRequest,
) -> JsonRpcResponse {
    let method = request.method.as_str();

    match method {
        // ── Lifecycle ──────────────────────────────────────────────
        "initialize" => handle_initialize(request, initialized).await,

        // ── Thread management ──────────────────────────────────────
        "threads/create" => {
            handle_create_thread(
                request,
                thread_manager,
                model_client,
                tool_registry,
                initialized,
                thread_configs,
            )
            .await
        }
        "threads/submitTurn" => {
            handle_submit_turn(
                request,
                thread_manager,
                initialized,
                output_tx,
            )
            .await
        }
        "threads/list" => handle_list_threads(request, thread_manager).await,
        "threads/get" => handle_get_thread(request, thread_manager, thread_configs).await,
        "threads/fork" => {
            handle_fork_thread(
                request,
                thread_manager,
                model_client,
                tool_registry,
                initialized,
                thread_configs,
            )
            .await
        }
        "threads/archive" => handle_archive_thread(request, thread_manager).await,

        // ── Notifications (no response expected) ────────────────────
        "notifications/initialized" => {
            debug!("Client sent 'initialized' notification");
            // Notifications have no id → no response
            JsonRpcResponse::success(None, Value::Null)
        }

        // ── Unknown methods ────────────────────────────────────────
        _ => {
            warn!(method = %method, "Unknown RPC method");
            JsonRpcResponse::error(
                request.id,
                error_codes::METHOD_NOT_FOUND,
                format!("Method not found: {method}"),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// initialize
// ---------------------------------------------------------------------------

async fn handle_initialize(
    request: JsonRpcRequest,
    initialized: &Arc<StdMutex<bool>>,
) -> JsonRpcResponse {
    let params: InitializeParams = match request.params {
        Some(ref p) => match serde_json::from_value(p.clone()) {
            Ok(params) => params,
            Err(e) => {
                return JsonRpcResponse::error(
                    request.id,
                    error_codes::INVALID_PARAMS,
                    format!("Invalid initialize params: {e}"),
                );
            }
        },
        None => InitializeParams {
            protocol_version: default_protocol_version(),
            capabilities: Value::Null,
            client_info: None,
        },
    };

    info!(
        protocol_version = %params.protocol_version,
        "Initializing app server"
    );

    // Mark as initialized
    {
        let mut init = initialized.lock().unwrap();
        *init = true;
    }

    let result = InitializeResult {
        protocol_version: "0.1.0".into(),
        server_info: ServerInfo {
            name: "code-agent-app-server".into(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        },
        capabilities: serde_json::json!({
            "threads": true,
            "streaming": true,
            "forking": true,
            "archiving": true,
        }),
    };

    JsonRpcResponse::success(request.id, serde_json::to_value(result).unwrap())
}

// ---------------------------------------------------------------------------
// threads/create
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn handle_create_thread(
    request: JsonRpcRequest,
    thread_manager: &Arc<StdMutex<ThreadManager>>,
    model_client: &Option<Arc<dyn ModelClient>>,
    tool_registry: &Option<Arc<dyn ToolRegistry>>,
    initialized: &Arc<StdMutex<bool>>,
    thread_configs: &Arc<StdMutex<HashMap<ThreadId, ThreadConfig>>>,
) -> JsonRpcResponse {
    // Check initialized
    if !*initialized.lock().unwrap() {
        return JsonRpcResponse::error(
            request.id,
            error_codes::SERVER_NOT_INITIALIZED,
            "Server not initialized",
        );
    }

    let params: CreateThreadParams = match parse_params(&request) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let mc = match model_client {
        Some(c) => Arc::clone(c),
        None => {
            return JsonRpcResponse::error(
                request.id,
                error_codes::INTERNAL_ERROR,
                "No model client configured",
            );
        }
    };
    let tr = match tool_registry {
        Some(r) => Arc::clone(r),
        None => {
            return JsonRpcResponse::error(
                request.id,
                error_codes::INTERNAL_ERROR,
                "No tool registry configured",
            );
        }
    };

    let thread_id = ThreadId::from(params.thread_id.as_str());
    let session_id = SessionId::from(format!("session-{}", uuid::Uuid::new_v4()));

    let config = SessionConfig {
        id: session_id.clone(),
        system_instructions: params.system_instructions.clone(),
        max_iterations: params.max_iterations,
        permission_mode: PermissionMode::Auto,
        model_client: mc,
        tool_registry: tr,
        tool_router: None,
        hook_registry: None,
        external_cancel: None,
        max_context_tokens: None,
        temperature: None,
        knowledge: None,
        quality_gate: None,
    };

    let session = Session::new(config.clone()).await;

    // Store the config for forking
    {
        let mut configs = thread_configs.lock().unwrap();
        configs.insert(
            thread_id.clone(),
            ThreadConfig {
                session_id: session_id.clone(),
                system_instructions: params.system_instructions,
                max_iterations: params.max_iterations,
                permission_mode: PermissionMode::Auto,
                max_context_tokens: None,
            },
        );
    }

    // Register the thread
    let mut tm = thread_manager.lock().unwrap();
    match tm.create_thread(thread_id.clone(), session, String::new()) {
        Ok(()) => {
            info!(thread_id = %thread_id, "Thread created");
            JsonRpcResponse::success(
                request.id,
                serde_json::json!({
                    "thread_id": thread_id.0,
                    "session_id": session_id.0,
                }),
            )
        }
        Err(e) => {
            // Clean up config on failure
            let mut configs = thread_configs.lock().unwrap();
            configs.remove(&thread_id);
            let code = if e.contains("already exists") {
                error_codes::THREAD_ALREADY_EXISTS
            } else if e.contains("max concurrent") {
                error_codes::MAX_THREADS_REACHED
            } else {
                error_codes::INTERNAL_ERROR
            };
            JsonRpcResponse::error(request.id, code, e)
        }
    }
}

// ---------------------------------------------------------------------------
// threads/submitTurn
// ---------------------------------------------------------------------------

async fn handle_submit_turn(
    request: JsonRpcRequest,
    thread_manager: &Arc<StdMutex<ThreadManager>>,
    initialized: &Arc<StdMutex<bool>>,
    output_tx: &mpsc::UnboundedSender<String>,
) -> JsonRpcResponse {
    if !*initialized.lock().unwrap() {
        return JsonRpcResponse::error(
            request.id.clone(),
            error_codes::SERVER_NOT_INITIALIZED,
            "Server not initialized",
        );
    }

    let params: SubmitTurnParams = match parse_params(&request) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let thread_id = ThreadId::from(params.thread_id.as_str());
    let req_id = request.id.clone();

    // Remove the session from the manager (to avoid holding lock across await)
    let mut session = {
        let mut tm = thread_manager.lock().unwrap();
        match tm.remove_thread(&thread_id) {
            Some(s) => s,
            None => {
                return JsonRpcResponse::error(
                    req_id,
                    error_codes::THREAD_NOT_FOUND,
                    format!("Thread not found: {thread_id}"),
                );
            }
        }
    };

    // Build the turn input
    let turn_input = TurnInput {
        thread_id: thread_id.clone(),
        messages: params.messages,
    };

    let tx = output_tx.clone();
    let tm_clone = Arc::clone(thread_manager);
    let tid = thread_id.clone();

    // Spawn the turn execution in a background task
    tokio::spawn(async move {
        let events = session.run_turn(turn_input).await;

        // Stream each event as a notification
        for event in &events {
            let notification = build_event_notification(event);
            if let Ok(json) = serde_json::to_string(&notification) {
                let _ = tx.send(json);
            }
        }

        // Re-insert the session into the thread manager
        let mut tm = tm_clone.lock().unwrap();
        if let Err(e) = tm.create_thread(tid.clone(), session, String::new()) {
            error!(thread_id = %tid, error = %e, "Failed to re-insert session after turn");
        }
    });

    // Return immediately — events stream as notifications
    JsonRpcResponse::success(
        req_id,
        serde_json::json!({
            "thread_id": thread_id.0,
            "status": "accepted",
        }),
    )
}

// ---------------------------------------------------------------------------
// threads/list
// ---------------------------------------------------------------------------

async fn handle_list_threads(
    request: JsonRpcRequest,
    thread_manager: &Arc<StdMutex<ThreadManager>>,
) -> JsonRpcResponse {
    let tm = thread_manager.lock().unwrap();
    let threads: Vec<String> = tm.list_threads().into_iter().map(|t| t.0).collect();

    JsonRpcResponse::success(
        request.id,
        serde_json::json!({
            "threads": threads,
            "count": threads.len(),
            "max_concurrent": tm.max_concurrent(),
        }),
    )
}

// ---------------------------------------------------------------------------
// threads/get
// ---------------------------------------------------------------------------

async fn handle_get_thread(
    request: JsonRpcRequest,
    thread_manager: &Arc<StdMutex<ThreadManager>>,
    thread_configs: &Arc<StdMutex<HashMap<ThreadId, ThreadConfig>>>,
) -> JsonRpcResponse {
    let params: GetThreadParams = match parse_params(&request) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let thread_id = ThreadId::from(params.thread_id.as_str());

    let mut tm = thread_manager.lock().unwrap();
    match tm.get_thread(&thread_id) {
        Some(session) => {
            let status = session.status();
            let configs = thread_configs.lock().unwrap();
            let system_instructions = configs
                .get(&thread_id)
                .map(|c| c.system_instructions.clone())
                .unwrap_or_default();

            let result = GetThreadResult {
                thread_id: thread_id.0.clone(),
                session_id: status.id.0,
                turn_count: status.turn_count,
                status: format!("{:?}", status.status).to_lowercase(),
                system_instructions,
            };

            JsonRpcResponse::success(
                request.id,
                serde_json::to_value(result).unwrap(),
            )
        }
        None => JsonRpcResponse::error(
            request.id,
            error_codes::THREAD_NOT_FOUND,
            format!("Thread not found: {thread_id}"),
        ),
    }
}

// ---------------------------------------------------------------------------
// threads/fork
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn handle_fork_thread(
    request: JsonRpcRequest,
    thread_manager: &Arc<StdMutex<ThreadManager>>,
    model_client: &Option<Arc<dyn ModelClient>>,
    tool_registry: &Option<Arc<dyn ToolRegistry>>,
    initialized: &Arc<StdMutex<bool>>,
    thread_configs: &Arc<StdMutex<HashMap<ThreadId, ThreadConfig>>>,
) -> JsonRpcResponse {
    if !*initialized.lock().unwrap() {
        return JsonRpcResponse::error(
            request.id,
            error_codes::SERVER_NOT_INITIALIZED,
            "Server not initialized",
        );
    }

    let params: ForkThreadParams = match parse_params(&request) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let source_id = ThreadId::from(params.thread_id.as_str());
    let new_id = ThreadId::from(params.new_thread_id.as_str());

    // Get config from the source thread
    let source_config = {
        let configs = thread_configs.lock().unwrap();
        match configs.get(&source_id).cloned() {
            Some(c) => c,
            None => {
                return JsonRpcResponse::error(
                    request.id,
                    error_codes::THREAD_NOT_FOUND,
                    format!("Source thread not found: {source_id}"),
                );
            }
        }
    };

    let mc = match model_client {
        Some(c) => Arc::clone(c),
        None => {
            return JsonRpcResponse::error(
                request.id,
                error_codes::INTERNAL_ERROR,
                "No model client configured",
            );
        }
    };
    let tr = match tool_registry {
        Some(r) => Arc::clone(r),
        None => {
            return JsonRpcResponse::error(
                request.id,
                error_codes::INTERNAL_ERROR,
                "No tool registry configured",
            );
        }
    };

    let new_session_id = SessionId::from(format!("session-{}", uuid::Uuid::new_v4()));

    let config = SessionConfig {
        id: new_session_id.clone(),
        system_instructions: source_config.system_instructions.clone(),
        max_iterations: source_config.max_iterations,
        permission_mode: source_config.permission_mode.clone(),
        model_client: mc,
        tool_registry: tr,
        tool_router: None,
        hook_registry: None,
        external_cancel: None,
        max_context_tokens: source_config.max_context_tokens,
        temperature: None,
        knowledge: None,
        quality_gate: None,
    };

    let session = Session::new(config).await;

    // Register the forked thread
    let mut tm = thread_manager.lock().unwrap();
    match tm.create_thread(new_id.clone(), session, String::new()) {
        Ok(()) => {
            // Store fork config
            let mut configs = thread_configs.lock().unwrap();
            configs.insert(
                new_id.clone(),
                ThreadConfig {
                    session_id: new_session_id.clone(),
                    system_instructions: source_config.system_instructions.clone(),
                    max_iterations: source_config.max_iterations,
                    permission_mode: source_config.permission_mode,
                    max_context_tokens: source_config.max_context_tokens,
                },
            );

            info!(source = %source_id, fork = %new_id, "Thread forked");
            JsonRpcResponse::success(
                request.id,
                serde_json::json!({
                    "thread_id": new_id.0,
                    "session_id": new_session_id.0,
                    "forked_from": source_id.0,
                }),
            )
        }
        Err(e) => {
            let code = if e.contains("already exists") {
                error_codes::THREAD_ALREADY_EXISTS
            } else {
                error_codes::INTERNAL_ERROR
            };
            JsonRpcResponse::error(request.id, code, e)
        }
    }
}

// ---------------------------------------------------------------------------
// threads/archive
// ---------------------------------------------------------------------------

async fn handle_archive_thread(
    request: JsonRpcRequest,
    thread_manager: &Arc<StdMutex<ThreadManager>>,
) -> JsonRpcResponse {
    let params: ArchiveThreadParams = match parse_params(&request) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let thread_id = ThreadId::from(params.thread_id.as_str());

    let mut tm = thread_manager.lock().unwrap();
    match tm.remove_thread(&thread_id) {
        Some(_session) => {
            info!(thread_id = %thread_id, "Thread archived");
            JsonRpcResponse::success(
                request.id,
                serde_json::json!({
                    "thread_id": thread_id.0,
                    "archived": true,
                }),
            )
        }
        None => JsonRpcResponse::error(
            request.id,
            error_codes::THREAD_NOT_FOUND,
            format!("Thread not found: {thread_id}"),
        ),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse the `params` field of a request as the given type.
#[allow(clippy::result_large_err)]
fn parse_params<T: serde::de::DeserializeOwned>(
    request: &JsonRpcRequest,
) -> Result<T, JsonRpcResponse> {
    match &request.params {
        Some(p) => serde_json::from_value(p.clone()).map_err(|e| {
            JsonRpcResponse::error(
                request.id.clone(),
                error_codes::INVALID_PARAMS,
                format!("Invalid params: {e}"),
            )
        }),
        None => {
            // Try to deserialize from null → use defaults
            serde_json::from_value(Value::Null).map_err(|e| {
                JsonRpcResponse::error(
                    request.id.clone(),
                    error_codes::INVALID_PARAMS,
                    format!("Missing params: {e}"),
                )
            })
        }
    }
}
