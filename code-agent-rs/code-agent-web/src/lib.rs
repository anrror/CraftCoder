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
use code_agent_protocol::{PermissionMode, SessionId, Team, TeamMember, ThreadId};
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

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
    pub model: Option<String>,
    pub temperature: Option<f32>,
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
    /// API key → UserId mapping (empty = require explicit anonymous opt-in).
    pub api_keys: HashMap<String, String>,
    /// Whether to allow anonymous requests when no API keys are configured.
    pub allow_anonymous: bool,
    /// Allowed origins for CORS (default: localhost only).
    pub cors_allowed_origins: Vec<String>,
    /// Teams registry: team_id → Team
    pub teams: Arc<RwLock<HashMap<String, Team>>>,
    /// Team members: team_id → Vec<TeamMember>
    pub team_members: Arc<RwLock<HashMap<String, Vec<TeamMember>>>>,
    /// User's team memberships: user_id → Vec<team_id>
    pub user_teams: Arc<RwLock<HashMap<String, Vec<String>>>>,
}

impl AppState {
    /// Create a new AppState with default configuration.
    pub fn new(api_keys: HashMap<String, String>) -> Self {
        Self {
            thread_manager: Arc::new(Mutex::new(ThreadManager::new(100))),
            model_client: None,
            tool_registry: None,
            thread_configs: Arc::new(RwLock::new(HashMap::new())),
            event_histories: Arc::new(RwLock::new(HashMap::new())),
            api_keys,
            allow_anonymous: false,
            cors_allowed_origins: vec![
                "http://localhost:3000".to_string(),
            ],
            teams: Arc::new(RwLock::new(HashMap::new())),
            team_members: Arc::new(RwLock::new(HashMap::new())),
            user_teams: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Authenticate a request header key.
    /// Returns `Some(user_id)` on success, `None` on failure.
    /// When no keys are configured, anonymous access requires explicit
    /// opt-in via [`with_anonymous`](Self::with_anonymous).
    pub fn authenticate(&self, header_key: Option<&str>) -> Option<String> {
        if self.api_keys.is_empty() {
            if self.allow_anonymous {
                return Some(String::new());
            }
            return None;
        }
        let key = header_key?;
        self.api_keys.get(key).cloned()
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

    /// Enable anonymous access when no API keys are configured.
    ///
    /// # Security
    ///
    /// This logs a warning — anonymous mode should only be used for
    /// local development or when an external auth proxy is in place.
    pub fn with_anonymous(mut self) -> Self {
        self.allow_anonymous = true;
        warn!("Running without API keys - all requests allowed. Set api_keys for production use.");
        self
    }

    /// Set the allowed CORS origins (replaces defaults).
    pub fn with_cors_origins(mut self, origins: Vec<String>) -> Self {
        self.cors_allowed_origins = origins;
        self
    }

    /// Create a new thread with the given parameters.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_thread(
        &self,
        thread_id: ThreadId,
        system_instructions: String,
        max_iterations: usize,
        model: Option<String>,
        temperature: Option<f32>,
        user_id: String,
    ) -> Result<serde_json::Value, String> {
        // P1: system_instructions 输入校验 — 防 LLM prompt 注入
        const MAX_SYSTEM_INSTRUCTIONS_LEN: usize = 4096;
        if system_instructions.len() > MAX_SYSTEM_INSTRUCTIONS_LEN {
            return Err(format!(
                "system_instructions too long: {} bytes exceeds {} bytes limit",
                system_instructions.len(),
                MAX_SYSTEM_INSTRUCTIONS_LEN
            ));
        }
        // P1: 拒绝已知危险模式（base64 大段文本/角色混淆/DAN 激活词）
        let blocked: &[&str] = &[
            "ignore all", "ignore previous", "ignore above",
            "you are dan", "you are now dan",
            "disregard", "pretend you are",
            "system: ", "system\n", "system:\n",
        ];
        if let Some(found) = blocked.iter().find(|p| system_instructions.to_lowercase().contains(*p)) {
            return Err(format!(
                "system_instructions contains blocked pattern: {found}"
            ));
        }

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
            tool_router: None,
            hook_registry: None,
            external_cancel: None,
            max_context_tokens: None,
            temperature,
            knowledge: None,
            quality_gate: None,
        };

        let session = Session::new(config).await;

        // Clone model before moving into ThreadConfig (needed for response)
        let model_clone = model.clone();

        // Store config
        {
            let mut configs = self.thread_configs.write().await;
            configs.insert(
                thread_id.clone(),
                ThreadConfig {
                    session_id: session_id.clone(),
                    system_instructions,
                    max_iterations,
            permission_mode: PermissionMode::Permit,  // H2: 默认 Permit，仅允许 Read 工具，Edit/Exec 需用户确认
                    model,
                    temperature,
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
        match tm.create_thread(thread_id.clone(), session, user_id) {
            Ok(()) => Ok(serde_json::json!({
                "thread_id": thread_id.0,
                "session_id": session_id.0,
                "model": model_clone,
                "temperature": temperature,
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
            None => Err("Thread not found".to_string()),
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
    fn auth_no_key_blocks_when_anonymous_disabled() {
        let state = AppState::new(HashMap::new());
        assert_eq!(state.authenticate(None), None);
        assert_eq!(state.authenticate(Some("anything")), None);
    }

    #[test]
    fn auth_no_key_allows_when_anonymous_enabled() {
        let state = AppState::new(HashMap::new()).with_anonymous();
        assert_eq!(state.authenticate(None), Some(String::new()));
        assert_eq!(state.authenticate(Some("anything")), Some(String::new()));
    }

    #[test]
    fn auth_with_key_requires_match() {
        let mut keys = HashMap::new();
        keys.insert("secret".into(), "alice".into());
        let state = AppState::new(keys);
        assert_eq!(state.authenticate(None), None);
        assert_eq!(state.authenticate(Some("wrong")), None);
        assert_eq!(state.authenticate(Some("secret")), Some("alice".into()));
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
