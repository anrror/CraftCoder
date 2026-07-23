//! Axum-based REST + SSE web API surface for the AI Coding Agent.
//!
//! Provides HTTP endpoints for managing agent threads:
//! - `POST /threads` — create a new thread
//! - `POST /threads/:id/turns` — submit input, SSE-stream the response
//! - `GET /threads/:id/events` — get event history for a thread
//! - `DELETE /threads/:id` — delete a thread
//!
//! SSE events mirror the [`ResponseEvent`](code_agent_protocol::ResponseEvent) types
//! from the core protocol.

pub mod routes;

use std::collections::HashMap;
use std::sync::Arc;

use code_agent_core::agent::{Session, SessionConfig, ThreadManager};
use code_agent_core::model::ModelClient;
use code_agent_core::tools::registry::ToolRegistry;
use code_agent_protocol::{PermissionMode, SessionId, ThreadId};
use tokio::sync::{Mutex, RwLock};
use tracing::info;

// ---------------------------------------------------------------------------
// Shared application state
// ---------------------------------------------------------------------------

/// Thread-level event history stored as a ring buffer.
#[derive(Clone, Debug)]
pub struct EventHistory {
    /// Ordered list of SSE-serializable event JSON strings.
    events: Arc<RwLock<Vec<String>>>,
    /// Maximum events to keep per thread.
    max_events: usize,
}

impl EventHistory {
    pub fn new(max_events: usize) -> Self {
        Self {
            events: Arc::new(RwLock::new(Vec::with_capacity(max_events))),
            max_events,
        }
    }

    /// Append an event JSON string to the history.
    pub async fn push(&self, event_json: String) {
        let mut guard = self.events.write().await;
        if guard.len() >= self.max_events {
            guard.remove(0);
        }
        guard.push(event_json);
    }

    /// Get a snapshot of all stored events.
    pub async fn snapshot(&self) -> Vec<String> {
        self.events.read().await.clone()
    }
}

/// Configuration stored per thread for re-creation / fork support.
#[derive(Clone)]
pub struct ThreadConfig {
    pub session_id: SessionId,
    pub system_instructions: String,
    pub max_iterations: usize,
    pub permission_mode: PermissionMode,
}

/// Shared application state accessible from all route handlers.
pub struct AppState {
    /// Thread manager (wrapped in async Mutex since Axum handlers are async).
    pub thread_manager: Arc<Mutex<ThreadManager>>,
    /// Model client for creating agent sessions.
    pub model_client: Option<Arc<dyn ModelClient>>,
    /// Tool registry for creating agent sessions.
    pub tool_registry: Option<Arc<dyn ToolRegistry>>,
    /// Per-thread configuration.
    pub thread_configs: Arc<RwLock<HashMap<ThreadId, ThreadConfig>>>,
    /// Per-thread event history.
    pub event_histories: Arc<RwLock<HashMap<ThreadId, EventHistory>>>,
    /// API key for authentication (None = localhost-only default).
    pub api_key: Option<String>,
}

impl AppState {
    /// Create a new AppState with default configuration.
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            thread_manager: Arc::new(Mutex::new(ThreadManager::new(100))),
            model_client: None,
            tool_registry: None,
            thread_configs: Arc::new(RwLock::new(HashMap::new())),
            event_histories: Arc::new(RwLock::new(HashMap::new())),
            api_key,
        }
    }

    /// Set the model client.
    pub fn with_model_client(mut self, client: Arc<dyn ModelClient>) -> Self {
        self.model_client = Some(client);
        self
    }

    /// Set the tool registry.
    pub fn with_tool_registry(mut self, registry: Arc<dyn ToolRegistry>) -> Self {
        self.tool_registry = Some(registry);
        self
    }

    /// Create a new thread with the given parameters.
    pub async fn create_thread(
        &self,
        thread_id: ThreadId,
        system_instructions: String,
        max_iterations: usize,
    ) -> Result<serde_json::Value, String> {
        let mc = self
            .model_client
            .as_ref()
            .ok_or_else(|| "No model client configured".to_string())?;
        let tr = self
            .tool_registry
            .as_ref()
            .ok_or_else(|| "No tool registry configured".to_string())?;

        let session_id = SessionId::from(format!("session-{}", uuid::Uuid::new_v4()));

        let config = SessionConfig {
            id: session_id.clone(),
            system_instructions: system_instructions.clone(),
            max_iterations,
            permission_mode: PermissionMode::Auto,
            model_client: Arc::clone(mc),
            tool_registry: Arc::clone(tr),
            external_cancel: None,
            max_context_tokens: None,
        };

        let session = Session::new(config).await;

        // Store config
        {
            let mut configs = self.thread_configs.write().await;
            configs.insert(
                thread_id.clone(),
                ThreadConfig {
                    session_id: session_id.clone(),
                    system_instructions,
                    max_iterations,
                    permission_mode: PermissionMode::Auto,
                },
            );
        }

        // Initialize event history
        {
            let mut histories = self.event_histories.write().await;
            histories.insert(thread_id.clone(), EventHistory::new(1000));
        }

        // Register thread
        let mut tm = self.thread_manager.lock().await;
        match tm.create_thread(thread_id.clone(), session) {
            Ok(()) => Ok(serde_json::json!({
                "thread_id": thread_id.0,
                "session_id": session_id.0,
            })),
            Err(e) => {
                // Cleanup on failure
                let mut configs = self.thread_configs.write().await;
                configs.remove(&thread_id);
                let mut histories = self.event_histories.write().await;
                histories.remove(&thread_id);
                Err(e)
            }
        }
    }

    /// Delete (archive) a thread.
    pub async fn delete_thread(&self, thread_id: &ThreadId) -> Result<(), String> {
        let mut tm = self.thread_manager.lock().await;
        match tm.remove_thread(thread_id) {
            Some(_) => {
                let mut configs = self.thread_configs.write().await;
                configs.remove(thread_id);
                let mut histories = self.event_histories.write().await;
                histories.remove(thread_id);
                info!(thread_id = %thread_id, "Thread deleted");
                Ok(())
            }
            None => Err(format!("Thread not found: {thread_id}")),
        }
    }

    /// Get event history for a thread.
    pub async fn get_event_history(&self, thread_id: &ThreadId) -> Option<Vec<String>> {
        let histories = self.event_histories.read().await;
        match histories.get(thread_id) {
            Some(h) => Some(h.snapshot().await),
            None => None,
        }
    }

    /// Record events into a thread's history.
    pub async fn record_events(&self, thread_id: &ThreadId, events: &[String]) {
        let histories = self.event_histories.read().await;
        if let Some(history) = histories.get(thread_id) {
            for event in events {
                history.push(event.clone()).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SSE event helpers
// ---------------------------------------------------------------------------

/// Build an SSE data line from a JSON-RPC style event notification.
pub fn sse_event(event_type: &str, data: &serde_json::Value) -> String {
    format!(
        "event: {}\ndata: {}\n\n",
        event_type,
        serde_json::to_string(data).unwrap_or_default()
    )
}

/// Build an SSE data line for a ResponseEvent.
pub fn sse_response_event(event: &code_agent_protocol::ResponseEvent) -> String {
    let event_type = match event {
        code_agent_protocol::ResponseEvent::TurnStarted { .. } => "turn_started",
        code_agent_protocol::ResponseEvent::AgentMessageDelta { .. } => "agent_message_delta",
        code_agent_protocol::ResponseEvent::ToolCallBegin { .. } => "tool_call_begin",
        code_agent_protocol::ResponseEvent::ToolCallEnd { .. } => "tool_call_end",
        code_agent_protocol::ResponseEvent::TurnComplete { .. } => "turn_complete",
        code_agent_protocol::ResponseEvent::Error { .. } => "error",
        code_agent_protocol::ResponseEvent::TokenUsage { .. } => "token_usage",
    };
    let json = serde_json::to_value(event).unwrap_or_default();
    sse_event(event_type, &json)
}

// ---------------------------------------------------------------------------
// Auth helpers
// ---------------------------------------------------------------------------

/// Check if the request has a valid API key.
/// If no API key is configured, allow all requests (localhost default).
pub fn check_auth(api_key: &Option<String>, header_key: Option<&str>) -> bool {
    match api_key {
        Some(key) => header_key.map(|h| h == key).unwrap_or(false),
        None => true, // No API key configured = allow all
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn event_history_push_and_snapshot() {
        let history = EventHistory::new(5);
        history.push("event1".into()).await;
        history.push("event2".into()).await;
        let snap = history.snapshot().await;
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0], "event1");
        assert_eq!(snap[1], "event2");
    }

    #[tokio::test]
    async fn event_history_ring_buffer() {
        let history = EventHistory::new(3);
        for i in 0..5 {
            history.push(format!("event{i}")).await;
        }
        let snap = history.snapshot().await;
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0], "event2");
        assert_eq!(snap[2], "event4");
    }

    #[test]
    fn auth_no_key_allows_all() {
        assert!(check_auth(&None, None));
        assert!(check_auth(&None, Some("anything")));
    }

    #[test]
    fn auth_with_key_requires_match() {
        let key = Some("secret".into());
        assert!(!check_auth(&key, None));
        assert!(!check_auth(&key, Some("wrong")));
        assert!(check_auth(&key, Some("secret")));
    }

    #[test]
    fn sse_event_format() {
        let data = serde_json::json!({"hello": "world"});
        let sse = sse_event("test", &data);
        assert!(sse.starts_with("event: test\n"));
        assert!(sse.contains(r#""hello":"world""#));
        assert!(sse.ends_with("\n\n"));
    }
}
