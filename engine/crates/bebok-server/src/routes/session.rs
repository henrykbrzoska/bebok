//! Session routes (Facade leaves; thin `extract -> service -> json`).
//! Covers `/session*` incl. prompt/abort/permission/export/compact/truncate.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;

use bebok_core::permission::{PermissionAnswer, ResolveOutcome};

use crate::error::{ApiError, err_response};
use crate::services::turn::prompt_turn;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateSession {
    pub directory: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    /// `continueLast: true` -> return the most recent session for the directory.
    #[serde(rename = "continueLast", default)]
    pub continue_last: Option<bool>,
    /// `forkOf: { sessionID, messageIndex }` -> fork that session.
    #[serde(rename = "forkOf", default)]
    pub fork_of: Option<ForkSpec>,
}

#[derive(Deserialize)]
pub struct ForkSpec {
    #[serde(rename = "sessionID")]
    pub session_id: Uuid,
    #[serde(rename = "messageIndex")]
    pub message_index: usize,
}

#[derive(Deserialize)]
pub struct PromptBody {
    pub message: String,
    /// Optional agent override for this turn (lets the user switch agents
    /// mid-chat; persists to the session when provided).
    #[serde(default)]
    pub agent: Option<String>,
    /// Optional model override for this turn (switch model mid-chat).
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub directory: Option<String>,
}

/// `POST /session/{id}/permission/{requestID}` body.
#[derive(Deserialize)]
pub struct PermissionBody {
    /// `allow` or `deny`.
    pub decision: String,
    /// Persist an `ask -> allow` project rule (only meaningful for `allow`).
    #[serde(default)]
    pub always: bool,
}

/// `POST /session/{id}/compact` body.
#[derive(Deserialize)]
pub struct CompactBody {
    pub budget: Option<usize>,
}

/// `POST /session/{id}/truncate` body: rewind in place, keep `keep` leading messages.
#[derive(Deserialize)]
pub struct TruncateBody {
    pub keep: usize,
}

/// `POST /session` -> `{ sessionID }` (also `continueLast` / `forkOf`).
pub async fn create_session(
    State(state): State<AppState>,
    Json(body): Json<CreateSession>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    // Fork: materialize an independent session copying messages up to the fork point.
    if let Some(fork) = body.fork_of {
        let session = state
            .store
            .fork_session(fork.session_id, fork.message_index)
            .await
            .map_err(|e| err_response(&e))?;
        return Ok(Json(serde_json::json!({
            "sessionID": session.id().to_string(),
            "parent": serde_json::json!([fork.session_id.to_string(), fork.message_index]),
        })));
    }

    // Continue: resume the most recent session for the directory.
    if body.continue_last.unwrap_or(false) {
        if let Some(last) = state
            .store
            .continue_last_session(&body.directory)
            .await
            .map_err(|e| err_response(&e))?
        {
            return Ok(Json(serde_json::json!({ "sessionID": last.id().to_string() })));
        }
        // No prior session -> fall through and create one.
    }

    let session = state
        .store
        .create_session(&body.directory, body.agent.as_deref().unwrap_or("code"), body.model.as_deref())
        .await
        .map_err(|e| err_response(&e))?;
    Ok(Json(serde_json::json!({ "sessionID": session.id().to_string() })))
}

/// `GET /session?directory=` -> session metadata list
pub async fn list_sessions(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let Some(directory) = q.directory else {
        return Err(ApiError::bad_request("missing ?directory= parameter").into_response());
    };
    let sessions = state.store.list_sessions(&directory).await;
    Ok(Json(serde_json::json!({ "sessions": sessions })))
}

/// `GET /session/{id}` -> metadata + usage totals
pub async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let meta = state
        .store
        .session_meta(id)
        .await
        .map_err(|e| err_response(&e))?;
    Ok(Json(serde_json::to_value(&meta).unwrap_or_default()))
}

/// `GET /session/{id}/message` -> full parts transcript
pub async fn get_messages(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let session = state
        .store
        .open_session(id)
        .await
        .map_err(|e| err_response(&e))?;
    let messages = session.messages_snapshot().await;
    Ok(Json(serde_json::json!({ "messages": messages })))
}

/// `POST /session/{id}/prompt` -> append user message, start the turn (202).
/// A second prompt during a running turn -> `409 Conflict`.
pub async fn prompt(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<PromptBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), axum::response::Response> {
    prompt_turn(&state, id, body)
        .await
        .map_err(|e| e.into_response())
}

/// `POST /session/{id}/abort` -> cancel the running turn.
pub async fn abort(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let session = state
        .store
        .open_session(id)
        .await
        .map_err(|e| err_response(&e))?;

    let cancelled = match session.abort_token().await {
        Some(token) => {
            token.cancel();
            true
        }
        None => false,
    };

    state
        .store
        .bus()
        .publish(bebok_core::event::Event::new(
            "session.updated",
            session.directory(),
            &id.to_string(),
        ));

    Ok(Json(serde_json::json!({
        "sessionID": id.to_string(),
        "aborted": cancelled,
    })))
}

/// `POST /session/{id}/permission/{requestID}` -> resolve an `ask`.
///
/// Body: `{ "decision": "allow"|"deny", "always": bool }`. The first client to
/// resolve a request wins; later attempts for the same id get `404`.
pub async fn permission_decision(
    State(state): State<AppState>,
    Path((id, request_id)): Path<(Uuid, String)>,
    Json(body): Json<PermissionBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let allow = match body.decision.as_str() {
        "allow" => true,
        "deny" => false,
        other => {
            return Err(ApiError::bad_request(format!(
                "decision must be 'allow' or 'deny', got '{other}'"
            ))
            .into_response());
        }
    };

    let session = state
        .store
        .open_session(id)
        .await
        .map_err(|e| err_response(&e))?;

    let outcome = session
        .resolve_permission_request(
            &request_id,
            PermissionAnswer {
                allow,
                always: body.always && allow,
            },
        )
        .await;

    match outcome {
        ResolveOutcome::Resolved => Ok(Json(serde_json::json!({
            "sessionID": id.to_string(),
            "requestID": request_id,
            "decision": if allow { "allow" } else { "deny" },
            "resolved": true,
        }))),
        ResolveOutcome::NotFound => Err(ApiError::not_found(format!(
            "no pending permission request {request_id}"
        ))
        .into_response()),
    }
}

/// `GET /session/{id}/export` -> full JSON (metadata + transcript).
pub async fn export_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let value = state
        .store
        .export_session(id)
        .await
        .map_err(|e| err_response(&e))?;
    Ok(Json(value))
}

/// `POST /session/{id}/compact` -> an internal fork: a new session whose
/// transcript is `[summary of messages 0..N]` + the tail. The original session
/// is untouched on disk (full history still available via `export`).
pub async fn compact_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CompactBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let source = state
        .store
        .open_session(id)
        .await
        .map_err(|e| err_response(&e))?;
    let messages = source.messages_snapshot().await;
    if messages.len() < 4 {
        return Err(ApiError::bad_request("session too short to compact").into_response());
    }

    let budget = body.budget.unwrap_or(source.config_snapshot().context_budget);
    let cutoff = compact_cutoff(&messages, budget);
    if cutoff == 0 {
        return Err(
            ApiError::bad_request("session fits the budget; nothing to compact").into_response(),
        );
    }

    let summary = bebok_core::context::compact_summary(&messages, cutoff);
    let fork = state
        .store
        .compact_session(id, summary, cutoff)
        .await
        .map_err(|e| err_response(&e))?;

    Ok(Json(serde_json::json!({
        "sessionID": fork.id().to_string(),
        "parent": serde_json::json!([id.to_string(), cutoff]),
    })))
}

/// `POST /session/{id}/truncate` -> rollback in place: erase every message from
/// `keep` onwards so the transcript ends right before the point being rewound.
/// Unlike compact/fork this mutates the *same* session (no new session is made).
pub async fn truncate_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<TruncateBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let session = state
        .store
        .truncate_session(id, body.keep)
        .await
        .map_err(|e| err_response(&e))?;
    Ok(Json(serde_json::json!({
        "sessionID": session.id().to_string(),
        "kept": body.keep,
    })))
}

/// `DELETE /session/{id}` -> remove a session permanently (in-memory state +
/// on-disk transcript). Refused with 409 while a turn is running.
pub async fn delete_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let meta = state
        .store
        .delete_session(id)
        .await
        .map_err(|e| err_response(&e))?;
    Ok(Json(serde_json::json!({
        "sessionID": id.to_string(),
        "directory": meta.directory,
        "deleted": true,
    })))
}

/// Choose a compaction cutoff: the number of leading messages to summarize such
/// that the remaining tail fits comfortably within half the budget (leaving
/// room for the model's reply), always keeping the latest exchange.
pub fn compact_cutoff(messages: &[bebok_core::session::Message], budget: usize) -> usize {
    let n = messages.len();
    let mut keep = 2;
    let mut tokens = bebok_core::context::estimate_message(&messages[n - 1])
        + bebok_core::context::estimate_message(&messages[n - 2]);
    while n - keep > 1 && tokens < budget / 2 {
        tokens += bebok_core::context::estimate_message(&messages[n - keep - 1]);
        keep += 1;
    }
    n.saturating_sub(keep)
}

