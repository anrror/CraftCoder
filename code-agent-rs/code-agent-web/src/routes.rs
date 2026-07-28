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
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{
        sse::{Event, Sse},
        IntoResponse, Json,
    },
    routing::{delete, get, post},
    Router,
};
use code_agent_protocol::{Message, Team, TeamMember, TeamRole, ThreadId, TurnInput};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tracing::{error, info};

use crate::{sse_response_event, AppState};

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
    /// Optional model name override (e.g. "gpt-4o", "qwen3.6-27b").
    /// If empty/unset, the server default is used.
    #[serde(default)]
    pub model: Option<String>,
    /// Optional sampling temperature (0.0–2.0).
    /// If unset, the server default is used.
    #[serde(default)]
    pub temperature: Option<f32>,
}

fn default_max_iterations() -> usize {
    20
}

/// Request body for `POST /threads/:id/share`.
#[derive(Debug, Deserialize)]
pub struct ShareThreadRequest {
    /// User ID to share with.
    pub user_id: String,
    /// Permission level: "read", "edit", or "admin" (default: "read").
    #[serde(default)]
    pub permission: String,
}

/// Request body for `POST /threads/:id/turns`.
#[derive(Debug, Deserialize)]
pub struct SubmitTurnRequest {
    /// The user message content.
    pub message: String,
}

/// Request body for `POST /teams`.
#[derive(Debug, Deserialize)]
pub struct CreateTeamRequest {
    pub name: String,
}

/// Request body for `POST /teams/:id/members`.
#[derive(Debug, Deserialize)]
pub struct AddMemberRequest {
    pub user_id: String,
    #[serde(default)]
    pub role: String, // "admin" or "member" (default: "member")
}

/// Request body for `POST /threads/:id/share-team`.
#[derive(Debug, Deserialize)]
pub struct ShareTeamRequest {
    pub team_id: String,
    #[serde(default)]
    pub permission: String, // "read", "edit", or "admin" (default: "read")
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
    let user_id = match state.authenticate(extract_api_key(&headers)) {
        Some(uid) => uid,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Unauthorized"})),
            );
        }
    };

    let thread_id = ThreadId::from(
        body.thread_id
            .unwrap_or_else(|| format!("thread-{}", uuid::Uuid::new_v4())),
    );

    match state
        .create_thread(
                    thread_id.clone(),
                    body.system_instructions,
                    body.max_iterations,
                    body.model,
                    body.temperature,
                    user_id,
                )
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
    let user_id = match state.authenticate(extract_api_key(&headers)) {
        Some(uid) => uid,
        None => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Unauthorized"})),
            ));
        }
    };

    let thread_id = ThreadId::from(thread_id_str.as_str());

    // Remove the session from the manager (to avoid holding lock across await)
    let mut session = {
        let mut tm = state.thread_manager.lock().await;
        // C1: Enforce thread ownership before access
        if !tm.check_ownership(&thread_id, &user_id) {
            return Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "Forbidden: thread does not belong to this user"})),
            ));
        }
        match tm.remove_thread(&thread_id) {
            Some(s) => s,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error": "Thread not found".to_string()})),
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

        // Re-insert the session into the thread manager (C9: preserve user_id)
        let mut tm = state_clone.thread_manager.lock().await;
        if let Err(e) = tm.create_thread(tid.clone(), session, user_id) {
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
    let user_id = match state.authenticate(extract_api_key(&headers)) {
        Some(uid) => uid,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Unauthorized"})),
            );
        }
    };

    let thread_id = ThreadId::from(thread_id_str.as_str());

    // C1: Check thread exists first, then enforce ownership
    {
        let tm = state.thread_manager.lock().await;
        if !tm.thread_exists(&thread_id) {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Thread not found".to_string()})),
            );
        }
        if !tm.check_ownership(&thread_id, &user_id) {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "Forbidden: thread does not belong to this user"})),
            );
        }
    }

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
            Json(serde_json::json!({"error": "Thread not found".to_string()})),
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
    let user_id = match state.authenticate(extract_api_key(&headers)) {
        Some(uid) => uid,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Unauthorized"})),
            );
        }
    };

    let thread_id = ThreadId::from(thread_id_str.as_str());

    // C1: Check thread exists first, then enforce ownership
    {
        let tm = state.thread_manager.lock().await;
        if !tm.thread_exists(&thread_id) {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Thread not found".to_string()})),
            );
        }
        if !tm.check_ownership(&thread_id, &user_id) {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "Forbidden: thread does not belong to this user"})),
            );
        }
    }

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

/// `GET /health` — health check endpoint.
pub async fn health_check() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "service": "code-agent-web",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// `GET /threads` — list active threads.
pub async fn list_threads(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Auth check
    let user_id = match state.authenticate(extract_api_key(&headers)) {
        Some(uid) => uid,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Unauthorized"})),
            );
        }
    };

    // C10: Only return threads belonging to the authenticated user
    let tm = state.thread_manager.lock().await;
    let thread_ids: Vec<String> = tm
        .list_user_threads(&user_id)
        .iter()
        .map(|id| id.0.clone())
        .collect();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "threads": thread_ids,
            "count": thread_ids.len(),
        })),
    )
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// `POST /threads/:id/share` — share a thread with another user.
pub async fn share_thread(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(thread_id_str): Path<String>,
    Json(body): Json<ShareThreadRequest>,
) -> impl IntoResponse {
    let user_id = match state.authenticate(extract_api_key(&headers)) {
        Some(uid) => uid,
        None => return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Unauthorized"}))),
    };

    let thread_id = ThreadId::from(thread_id_str.as_str());
    let permission = match body.permission.as_str() {
        "edit" => code_agent_protocol::SharePermission::Edit,
        "admin" => code_agent_protocol::SharePermission::Admin,
        _ => code_agent_protocol::SharePermission::Read,
    };

    let target_user = body.user_id;
    let mut tm = state.thread_manager.lock().await;
    match tm.share_thread(thread_id, &user_id, target_user.clone(), permission) {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({"shared": true, "thread_id": thread_id_str, "with": target_user})),
        ),
        Err(e) => {
            let code = if e.contains("not the owner") { StatusCode::FORBIDDEN }
                else if e.contains("not found") { StatusCode::NOT_FOUND }
                else { StatusCode::BAD_REQUEST };
            (code, Json(serde_json::json!({"error": e})))
        }
    }
}

/// `DELETE /threads/:id/share/:user_id` — unshare a thread.
pub async fn unshare_thread(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((thread_id_str, target_user)): Path<(String, String)>,
) -> impl IntoResponse {
    let user_id = match state.authenticate(extract_api_key(&headers)) {
        Some(uid) => uid,
        None => return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Unauthorized"}))),
    };

    let thread_id = ThreadId::from(thread_id_str.as_str());
    let mut tm = state.thread_manager.lock().await;
    match tm.unshare_thread(thread_id, &user_id, &target_user) {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({"unshared": true, "thread_id": thread_id_str, "user": target_user})),
        ),
        Err(e) => {
            let code = if e.contains("not the owner") { StatusCode::FORBIDDEN }
                else if e.contains("not found") { StatusCode::NOT_FOUND }
                else { StatusCode::BAD_REQUEST };
            (code, Json(serde_json::json!({"error": e})))
        }
    }
}

/// `GET /shared-threads` — list threads shared with the current user.
pub async fn list_shared_threads(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let user_id = match state.authenticate(extract_api_key(&headers)) {
        Some(uid) => uid,
        None => return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Unauthorized"}))),
    };

    let tm = state.thread_manager.lock().await;
    let threads: Vec<String> = tm
        .list_shared_threads(&user_id)
        .iter()
        .map(|id| id.0.clone())
        .collect();

    (StatusCode::OK, Json(serde_json::json!({"threads": threads, "count": threads.len()})))
}

// ---------------------------------------------------------------------------
// Team management handlers
// ---------------------------------------------------------------------------

/// `POST /teams` — create a new team.
pub async fn create_team(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreateTeamRequest>,
) -> impl IntoResponse {
    let user_id = match state.authenticate(extract_api_key(&headers)) {
        Some(uid) => uid,
        None => return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Unauthorized"}))),
    };

    let team_id = format!("team-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();

    let team = Team {
        id: team_id.clone(),
        name: body.name.clone(),
        created_by: user_id.clone(),
        created_at: now.clone(),
    };

    let member = TeamMember {
        team_id: team_id.clone(),
        user_id: user_id.clone(),
        role: TeamRole::Admin,
        joined_at: now,
    };

    // Store team
    {
        let mut teams = state.teams.write().await;
        teams.insert(team_id.clone(), team);
    }

    // Store member
    {
        let mut members = state.team_members.write().await;
        members.insert(team_id.clone(), vec![member]);
    }

    // Track user membership
    {
        let mut uts = state.user_teams.write().await;
        uts.entry(user_id.clone()).or_default().push(team_id.clone());
    }

    info!(team_id = %team_id, user_id = %user_id, name = %body.name, "Team created");
    (StatusCode::CREATED, Json(serde_json::json!({"team_id": team_id, "name": body.name})))
}

/// `GET /teams` — list teams the current user belongs to.
pub async fn list_teams(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let user_id = match state.authenticate(extract_api_key(&headers)) {
        Some(uid) => uid,
        None => return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Unauthorized"}))),
    };

    let uts = state.user_teams.read().await;
    let team_ids = uts.get(&user_id).cloned().unwrap_or_default();

    let teams_map = state.teams.read().await;
    let teams: Vec<&Team> = team_ids.iter().filter_map(|id| teams_map.get(id)).collect();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "teams": teams,
            "count": teams.len(),
        })),
    )
}

/// `GET /teams/:id/members` — list team members.
pub async fn list_team_members(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(team_id): Path<String>,
) -> impl IntoResponse {
    let user_id = match state.authenticate(extract_api_key(&headers)) {
        Some(uid) => uid,
        None => return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Unauthorized"}))),
    };

    // Check team exists
    let teams = state.teams.read().await;
    if !teams.contains_key(&team_id) {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "Team not found"})));
    }
    drop(teams);

    // Verify user is a team member
    let uts = state.user_teams.read().await;
    let user_team_ids = uts.get(&user_id).cloned().unwrap_or_default();
    if !user_team_ids.contains(&team_id) {
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({"error": "Forbidden: not a team member"})));
    }
    drop(uts);

    let members_map = state.team_members.read().await;
    let members = members_map.get(&team_id).cloned().unwrap_or_default();

    (StatusCode::OK, Json(serde_json::json!({"members": members, "count": members.len()})))
}

/// `POST /teams/:id/members` — add member to team (admin only).
pub async fn add_team_member(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(team_id): Path<String>,
    Json(body): Json<AddMemberRequest>,
) -> impl IntoResponse {
    let user_id = match state.authenticate(extract_api_key(&headers)) {
        Some(uid) => uid,
        None => return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Unauthorized"}))),
    };

    // Verify requester is a team admin
    let members_map = state.team_members.read().await;
    let members = match members_map.get(&team_id) {
        Some(m) => m.clone(),
        None => return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "Team not found"}))),
    };
    drop(members_map);

    let is_admin = members.iter().any(|m| m.user_id == user_id && matches!(m.role, TeamRole::Admin));
    if !is_admin {
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({"error": "Forbidden: admin role required"})));
    }

    // Check if user already a member
    if members.iter().any(|m| m.user_id == body.user_id) {
        return (StatusCode::CONFLICT, Json(serde_json::json!({"error": "User already a team member"})));
    }

    let role = match body.role.as_str() {
        "admin" => TeamRole::Admin,
        _ => TeamRole::Member,
    };

    let member = TeamMember {
        team_id: team_id.clone(),
        user_id: body.user_id.clone(),
        role,
        joined_at: chrono::Utc::now().to_rfc3339(),
    };

    // Add to team members
    {
        let mut members_map = state.team_members.write().await;
        if let Some(mlist) = members_map.get_mut(&team_id) {
            mlist.push(member);
        }
    }

    // Track user membership
    {
        let mut uts = state.user_teams.write().await;
        uts.entry(body.user_id.clone()).or_default().push(team_id.clone());
    }

    info!(team_id = %team_id, added_user = %body.user_id, by = %user_id, "Member added to team");
    (StatusCode::OK, Json(serde_json::json!({"added": true})))
}

/// `DELETE /teams/:id/members/:user_id` — remove member from team (admin only).
pub async fn remove_team_member(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((team_id, target_user)): Path<(String, String)>,
) -> impl IntoResponse {
    let user_id = match state.authenticate(extract_api_key(&headers)) {
        Some(uid) => uid,
        None => return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Unauthorized"}))),
    };

    let mut members_map = state.team_members.write().await;
    let members = match members_map.get_mut(&team_id) {
        Some(m) => m,
        None => return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "Team not found"}))),
    };

    // Verify requester is admin (or removing themselves)
    let is_admin = members.iter().any(|m| m.user_id == user_id && matches!(m.role, TeamRole::Admin));
    if !is_admin && user_id != target_user {
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({"error": "Forbidden: admin role required"})));
    }

    let orig_len = members.len();
    members.retain(|m| m.user_id != target_user);
    if members.len() == orig_len {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "Member not found"})));
    }

    drop(members_map);

    // Remove from user_teams
    {
        let mut uts = state.user_teams.write().await;
        if let Some(tids) = uts.get_mut(&target_user) {
            tids.retain(|t| t != &team_id);
        }
    }

    info!(team_id = %team_id, removed_user = %target_user, by = %user_id, "Member removed from team");
    (StatusCode::OK, Json(serde_json::json!({"removed": true})))
}

/// `DELETE /teams/:id` — delete team (admin only).
pub async fn delete_team(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(team_id): Path<String>,
) -> impl IntoResponse {
    let user_id = match state.authenticate(extract_api_key(&headers)) {
        Some(uid) => uid,
        None => return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Unauthorized"}))),
    };

    // Verify requester is a team admin
    let members_map = state.team_members.read().await;
    let members = match members_map.get(&team_id) {
        Some(m) => m,
        None => return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "Team not found"}))),
    };

    let is_admin = members.iter().any(|m| m.user_id == user_id && matches!(m.role, TeamRole::Admin));
    if !is_admin {
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({"error": "Forbidden: admin role required"})));
    }

    // Collect all user IDs to clean up user_teams
    let member_user_ids: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();
    let member_count = members.len();
    drop(members_map);

    // Remove team
    {
        let mut teams = state.teams.write().await;
        teams.remove(&team_id);
    }

    // Remove members
    {
        let mut members_map = state.team_members.write().await;
        members_map.remove(&team_id);
    }

    // Remove from user_teams for all members
    {
        let mut uts = state.user_teams.write().await;
        for uid in &member_user_ids {
            if let Some(tids) = uts.get_mut(uid) {
                tids.retain(|t| t != &team_id);
            }
        }
    }

    info!(team_id = %team_id, by = %user_id, member_count = member_count, "Team deleted");
    (StatusCode::OK, Json(serde_json::json!({"deleted": true})))
}

/// `POST /threads/:id/share-team` — share a thread with a team.
pub async fn share_thread_with_team(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(thread_id_str): Path<String>,
    Json(body): Json<ShareTeamRequest>,
) -> impl IntoResponse {
    let user_id = match state.authenticate(extract_api_key(&headers)) {
        Some(uid) => uid,
        None => return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Unauthorized"}))),
    };

    let thread_id = ThreadId::from(thread_id_str.as_str());
    let permission = match body.permission.as_str() {
        "edit" => code_agent_protocol::SharePermission::Edit,
        "admin" => code_agent_protocol::SharePermission::Admin,
        _ => code_agent_protocol::SharePermission::Read,
    };

    let mut tm = state.thread_manager.lock().await;
    match tm.share_thread_with_team(thread_id, &user_id, body.team_id.clone(), permission) {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({"shared": true, "thread_id": thread_id_str, "team_id": body.team_id})),
        ),
        Err(e) => {
            let code = if e.contains("not the owner") { StatusCode::FORBIDDEN }
                else if e.contains("not found") { StatusCode::NOT_FOUND }
                else { StatusCode::BAD_REQUEST };
            (code, Json(serde_json::json!({"error": e})))
        }
    }
}

/// `DELETE /threads/:id/share-team/:team_id` — unshare thread from a team.
pub async fn unshare_thread_with_team(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((thread_id_str, team_id)): Path<(String, String)>,
) -> impl IntoResponse {
    let user_id = match state.authenticate(extract_api_key(&headers)) {
        Some(uid) => uid,
        None => return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Unauthorized"}))),
    };

    let thread_id = ThreadId::from(thread_id_str.as_str());
    let mut tm = state.thread_manager.lock().await;
    match tm.unshare_thread_with_team(thread_id, &user_id, &team_id) {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({"unshared": true, "thread_id": thread_id_str, "team_id": team_id})),
        ),
        Err(e) => {
            let code = if e.contains("not the owner") { StatusCode::FORBIDDEN }
                else if e.contains("not found") { StatusCode::NOT_FOUND }
                else { StatusCode::BAD_REQUEST };
            (code, Json(serde_json::json!({"error": e})))
        }
    }
}

/// Build the Axum router for all web API routes.
pub fn build_router(state: Arc<AppState>) -> Router {
    // Build CORS layer from the configured allowed origins
    let origins: Vec<HeaderValue> = state
        .cors_allowed_origins
        .iter()
        .filter_map(|o| o.parse::<HeaderValue>().ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/health", get(health_check))
        .route("/threads", post(create_thread).get(list_threads))
        .route("/threads/:id/turns", post(submit_turn))
        .route("/threads/:id/events", get(get_events))
        .route("/threads/:id/share", post(share_thread))
        .route("/threads/:id/share/:user_id", delete(unshare_thread))
        .route("/threads/:id/share-team", post(share_thread_with_team))
        .route("/threads/:id/share-team/:team_id", delete(unshare_thread_with_team))
        .route("/shared-threads", get(list_shared_threads))
        .route("/threads/:id", delete(delete_thread))
        .route("/teams", post(create_team).get(list_teams))
        .route("/teams/:id/members", get(list_team_members).post(add_team_member))
        .route("/teams/:id/members/:user_id", delete(remove_team_member))
        .route("/teams/:id", delete(delete_team))
        .layer(cors)
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
    use code_agent_core::tools::registry::DefaultToolRegistry;
    use code_agent_protocol::ResponseEvent;
use std::collections::HashMap;
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
            _temperature: Option<f32>,
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
            let tr = Arc::new(DefaultToolRegistry::new());
        Arc::new(
            AppState::new(HashMap::new())
                .with_model_client(mc)
                .with_tool_registry(tr)
                .with_anonymous(),
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
                    model: None,
                    temperature: None,
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
            model: None,
            temperature: None,
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
                    model: None,
                    temperature: None,
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
        let mut keys = HashMap::new();
        keys.insert("secret-key".into(), String::new());
        let state = Arc::new(AppState::new(keys));
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
                    model: None,
                    temperature: None,
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
            let tr = Arc::new(DefaultToolRegistry::new());
        let mut keys = HashMap::new();
        keys.insert("valid-key".into(), String::new());
        let state = Arc::new(
            AppState::new(keys)
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
                    model: None,
                    temperature: None,
                })
                .unwrap(),
            ))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }
}
