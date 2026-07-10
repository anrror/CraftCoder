//! Axum route handlers for the web API surface.
//!
//! # Endpoints
//!
//! | Method   | Path                    | Description                          |
//! |----------|-------------------------|--------------------------------------|
//! | `POST`   | `/threads`              | Create a new thread                  |
//! | `POST`   | `/threads/:id/turns`    | Submit input, SSE-stream response    |
//! | `GET`    | `/threads/:id/events`   | Get event history for a thread       |
//! | `DELETE` | `/threads/:id`          | Delete a thread                      |

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, Sse},
        IntoResponse, Json,
    },
    routing::{delete, get, post},
    Router,
};
use code_agent_protocol::{Message, ThreadId, TurnInput};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;
use tracing::{error, info};

use crate::{check_auth, sse_response_event, AppState};

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// Request body for `POST /threads`.
#[derive(Debug, Deserialize, Serialize)]
pub struct CreateThreadRequest {
    /// Optional thread ID (auto-generated if not provided).
    #[serde(default)]
    pub thread_id: Option<String>,
    /// System instructions for the agent.
    #[serde(default)]
    pub system_instructions: String,
    /// Maximum ReAct iterations per turn.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,
}

fn default_max_iterations() -> usize {
    20
}

/// Request body for `POST /threads/:id/turns`.
#[derive(Debug, Deserialize)]
pub struct SubmitTurnRequest {
    /// The user message content.
    pub message: String,
}

/// Response for `POST /threads`.
#[derive(Debug, Serialize)]
pub struct CreateThreadResponse {
    pub thread_id: String,
    pub session_id: String,
}

/// Response for `GET /threads/:id/events`.
#[derive(Debug, Serialize)]
pub struct EventsResponse {
    pub thread_id: String,
    pub events: Vec<serde_json::Value>,
}

/// Error response body.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

// ---------------------------------------------------------------------------
// Auth header extraction
// ---------------------------------------------------------------------------

/// Extract the API key from the `Authorization` header.
fn extract_api_key(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

/// `POST /threads` — create a new thread.
pub async fn create_thread(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreateThreadRequest>,
) -> impl IntoResponse {
    // Auth check
    if !check_auth(&state.api_key, extract_api_key(&headers)) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Unauthorized"})),
        );
    }

    let thread_id = ThreadId::from(
        body.thread_id
            .unwrap_or_else(|| format!("thread-{}", uuid::Uuid::new_v4())),
    );

    match state
        .create_thread(thread_id.clone(), body.system_instructions, body.max_iterations)
        .await
    {
        Ok(result) => {
            info!(thread_id = %thread_id, "Thread created via REST API");
            (StatusCode::CREATED, Json(result))
        }
        Err(e) => {
            let code = if e.contains("already exists") {
                StatusCode::CONFLICT
            } else if e.contains("max concurrent") {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (code, Json(serde_json::json!({"error": e})))
        }
    }
}

/// `POST /threads/:id/turns` — submit input and SSE-stream the response.
pub async fn submit_turn(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(thread_id_str): Path<String>,
    Json(body): Json<SubmitTurnRequest>,
) -> impl IntoResponse {
    // Auth check
    if !check_auth(&state.api_key, extract_api_key(&headers)) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Unauthorized"})),
        ));
    }

    let thread_id = ThreadId::from(thread_id_str.as_str());

    // Remove the session from the manager (to avoid holding lock across await)
    let mut session = {
        let mut tm = state.thread_manager.lock().await;
        match tm.remove_thread(&thread_id) {
            Some(s) => s,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error": format!("Thread not found: {thread_id}")})),
                ));
            }
        }
    };

    let turn_input = TurnInput {
        thread_id: thread_id.clone(),
        messages: vec![Message::UserMessage {
            content: body.message,
        }],
    };

    let state_clone = Arc::clone(&state);
    let tid = thread_id.clone();

    // Spawn the turn execution in a background task.
    // The SSE stream will be fed via a channel.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    tokio::spawn(async move {
        let events = session.run_turn(turn_input).await;

        // Record events into history
        let event_strings: Vec<String> = events.iter().map(sse_response_event).collect();
        state_clone.record_events(&tid, &event_strings).await;

        // Send each event as SSE
        for event_str in &event_strings {
            if tx.send(event_str.clone()).is_err() {
                break;
            }
        }

        // Re-insert the session into the thread manager
        let mut tm = state_clone.thread_manager.lock().await;
        if let Err(e) = tm.create_thread(tid.clone(), session) {
            error!(thread_id = %tid, error = %e, "Failed to re-insert session after turn");
        }
    });

    // Build SSE stream from the channel receiver
    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);

    let sse_stream = stream.map(|data| {
        // Parse the SSE-formatted string and extract the event/data
        // The data is already in SSE format from sse_response_event
        Ok::<_, std::convert::Infallible>(Event::default().data(data))
    });

    Ok::<_, (StatusCode, Json<serde_json::Value>)>(Sse::new(sse_stream))
}

/// `GET /threads/:id/events` — get event history for a thread.
pub async fn get_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(thread_id_str): Path<String>,
) -> impl IntoResponse {
    // Auth check
    if !check_auth(&state.api_key, extract_api_key(&headers)) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Unauthorized"})),
        );
    }

    let thread_id = ThreadId::from(thread_id_str.as_str());

    match state.get_event_history(&thread_id).await {
        Some(event_strings) => {
            // Parse each SSE string back into a JSON value
            let events: Vec<serde_json::Value> = event_strings
                .iter()
                .filter_map(|s| {
                    // SSE format: "event: xxx\ndata: {...}\n\n"
                    // Extract the data line
                    s.lines()
                        .find(|line| line.starts_with("data: "))
                        .and_then(|line| serde_json::from_str(line.trim_start_matches("data: ")).ok())
                })
                .collect();

            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "thread_id": thread_id.0,
                    "events": events,
                })),
            )
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("Thread not found: {thread_id}")})),
        ),
    }
}

/// `DELETE /threads/:id` — delete a thread.
pub async fn delete_thread(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(thread_id_str): Path<String>,
) -> impl IntoResponse {
    // Auth check
    if !check_auth(&state.api_key, extract_api_key(&headers)) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Unauthorized"})),
        );
    }

    let thread_id = ThreadId::from(thread_id_str.as_str());

    match state.delete_thread(&thread_id).await {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "thread_id": thread_id.0,
                "deleted": true,
            })),
        ),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": e})),
        ),
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build the Axum router for all web API routes.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/threads", post(create_thread))
        .route("/threads/:id/turns", post(submit_turn))
        .route("/threads/:id/events", get(get_events))
        .route("/threads/:id", delete(delete_thread))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;
    use axum::{
        body::Body,
        http::{Method, Request},
    };
    use code_agent_core::model::ModelClient;
    use code_agent_core::tools::registry::ToolRegistry;
    use code_agent_protocol::ResponseEvent;
    use std::sync::Arc;
    use tower::util::ServiceExt;

    // ── Mock Model Client ──────────────────────────────────────────────

    struct MockModelClient;

    #[async_trait::async_trait]
    impl ModelClient for MockModelClient {
        fn model_name(&self) -> &str {
            "mock"
        }

        async fn complete_stream(
            &self,
            _messages: &[Message],
            _tools: &[code_agent_core::model::ToolDefinition],
        ) -> code_agent_core::model::ModelResult<
            Box<dyn futures::Stream<Item = ResponseEvent> + Send + Unpin>,
        > {
            Ok(Box::new(futures::stream::iter(vec![
                ResponseEvent::AgentMessageDelta {
                    content: "Hello from mock!".into(),
                },
            ])))
        }

        fn last_token_usage(&self) -> Option<code_agent_core::model::types::TokenUsage> {
            None
        }
    }

    fn make_test_state() -> Arc<AppState> {
        let mc = Arc::new(MockModelClient);
        let tr = Arc::new(ToolRegistry::new());
        Arc::new(
            AppState::new(None)
                .with_model_client(mc)
                .with_tool_registry(tr),
        )
    }

    #[tokio::test]
    async fn test_create_thread() {
        let state = make_test_state();
        let app = build_router(state);

        let req = Request::builder()
            .method(Method::POST)
            .uri("/threads")
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::to_string(&CreateThreadRequest {
                    thread_id: Some("test-thread".into()),
                    system_instructions: "You are a test assistant.".into(),
                    max_iterations: 10,
                })
                .unwrap(),
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["thread_id"], "test-thread");
        assert!(json["session_id"].as_str().unwrap().starts_with("session-"));
    }

    #[tokio::test]
    async fn test_create_duplicate_thread() {
        let state = make_test_state();
        let app = build_router(state);

        let body = serde_json::to_string(&CreateThreadRequest {
            thread_id: Some("dup".into()),
            system_instructions: "".into(),
            max_iterations: 10,
        })
        .unwrap();

        // First create
        let req1 = Request::builder()
            .method(Method::POST)
            .uri("/threads")
            .header("Content-Type", "application/json")
            .body(Body::from(body.clone()))
            .unwrap();
        let resp1 = app.clone().oneshot(req1).await.unwrap();
        assert_eq!(resp1.status(), StatusCode::CREATED);

        // Duplicate
        let req2 = Request::builder()
            .method(Method::POST)
            .uri("/threads")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let resp2 = app.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_delete_thread() {
        let state = make_test_state();
        let app = build_router(state);

        // Create first
        let create_req = Request::builder()
            .method(Method::POST)
            .uri("/threads")
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::to_string(&CreateThreadRequest {
                    thread_id: Some("to-delete".into()),
                    system_instructions: "".into(),
                    max_iterations: 10,
                })
                .unwrap(),
            ))
            .unwrap();
        let create_resp = app.clone().oneshot(create_req).await.unwrap();
        assert_eq!(create_resp.status(), StatusCode::CREATED);

        // Delete
        let del_req = Request::builder()
            .method(Method::DELETE)
            .uri("/threads/to-delete")
            .body(Body::empty())
            .unwrap();
        let del_resp = app.clone().oneshot(del_req).await.unwrap();
        assert_eq!(del_resp.status(), StatusCode::OK);

        // Delete again — should 404
        let del_req2 = Request::builder()
            .method(Method::DELETE)
            .uri("/threads/to-delete")
            .body(Body::empty())
            .unwrap();
        let del_resp2 = app.oneshot(del_req2).await.unwrap();
        assert_eq!(del_resp2.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_events_nonexistent() {
        let state = make_test_state();
        let app = build_router(state);

        let req = Request::builder()
            .method(Method::GET)
            .uri("/threads/no-such-thread/events")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_auth_required() {
        let state = Arc::new(AppState::new(Some("secret-key".into())));
        let app = build_router(state);

        // No auth header
        let req = Request::builder()
            .method(Method::POST)
            .uri("/threads")
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::to_string(&CreateThreadRequest {
                    thread_id: None,
                    system_instructions: "".into(),
                    max_iterations: 10,
                })
                .unwrap(),
            ))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_auth_with_valid_key() {
        let mc = Arc::new(MockModelClient);
        let tr = Arc::new(ToolRegistry::new());
        let state = Arc::new(
            AppState::new(Some("valid-key".into()))
                .with_model_client(mc)
                .with_tool_registry(tr),
        );
        let app = build_router(state);

        let req = Request::builder()
            .method(Method::POST)
            .uri("/threads")
            .header("Content-Type", "application/json")
            .header("Authorization", "Bearer valid-key")
            .body(Body::from(
                serde_json::to_string(&CreateThreadRequest {
                    thread_id: Some("auth-test".into()),
                    system_instructions: "".into(),
                    max_iterations: 10,
                })
                .unwrap(),
            ))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }
}
