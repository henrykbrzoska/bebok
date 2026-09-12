//! Session routes (Facade leaves; thin `extract -> service -> json`).
//! Covers `/session*` incl. prompt/abort/task-abort/permission/export/compact/truncate.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
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
    /// WP-GIT / F6-15: `worktree: { branch, base? }` -> create the session in
    /// a fresh `git worktree` of `directory` at
    /// `<directory>/.bebok/worktrees/<branch>` (additive; absent = unchanged
    /// behaviour). Ignored for `forkOf`/`continueLast`.
    #[serde(default)]
    pub worktree: Option<WorktreeSpec>,
}

/// Worktree request for `POST /session`.
#[derive(Deserialize)]
pub struct WorktreeSpec {
    /// Branch to check out (created from `base` when it does not exist yet).
    /// Doubles as the path below `.bebok/worktrees`, so it must be a safe
    /// relative path (validated in `bebok_core::git`).
    pub branch: String,
    /// Start point for a new branch; default: the current `HEAD`.
    #[serde(default)]
    pub base: Option<String>,
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
    /// Optional image attachments (raw base64, no `data:` URL prefix required).
    #[serde(default)]
    pub images: Vec<ImageInput>,
}

/// One image attached to a prompt: raw base64 payload + MIME type.
#[derive(Debug, Clone, Deserialize)]
pub struct ImageInput {
    pub media_type: String,
    pub data: String,
    #[serde(default)]
    pub name: Option<String>,
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
    if body.continue_last.unwrap_or(false)
        && let Some(last) = state
            .store
            .continue_last_session(&body.directory)
            .await
            .map_err(|e| err_response(&e))?
    {
        return Ok(Json(
            serde_json::json!({ "sessionID": last.id().to_string() }),
        ));
    }
    // No prior session -> fall through and create one.

    // Worktree-backed session (F6-15): the store shells out to `git worktree
    // add` and binds the session to the worktree path.
    if let Some(spec) = body.worktree {
        let base = spec
            .base
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty());
        let (session, path) = state
            .store
            .create_worktree_session(
                &body.directory,
                spec.branch.trim(),
                base,
                body.agent.as_deref().unwrap_or("code"),
                body.model.as_deref(),
            )
            .await
            .map_err(|e| err_response(&e))?;
        return Ok(Json(serde_json::json!({
            "sessionID": session.id().to_string(),
            "directory": session.directory(),
            "worktree": {
                "path": path.to_string_lossy(),
                "branch": spec.branch.trim(),
            },
        })));
    }

    let session = state
        .store
        .create_session(
            &body.directory,
            body.agent.as_deref().unwrap_or("code"),
            body.model.as_deref(),
        )
        .await
        .map_err(|e| err_response(&e))?;
    Ok(Json(
        serde_json::json!({ "sessionID": session.id().to_string() }),
    ))
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
    // Default model for sessions that never ran a turn and carry no override.
    let default_model = state
        .store
        .get_or_create_instance(&directory)
        .await
        .ok()
        .and_then(|instance| instance.config.read().ok().map(|cfg| cfg.model.clone()));
    let sessions: Vec<serde_json::Value> = sessions
        .iter()
        .map(|session| {
            let mut value = serde_json::to_value(session).unwrap_or_default();
            attach_context_window(&mut value, session, default_model.as_deref());
            attach_worktree(&mut value, session);
            value
        })
        .collect();
    Ok(Json(serde_json::json!({ "sessions": sessions })))
}

/// Add the live `context_window` (tokens) for the model that produced the
/// session's last turn (`context_model`), else the session's model override,
/// else the directory default. Resolved from the catalog on every response so
/// it is never persisted and a catalog update needs no migration.
pub fn attach_context_window(
    value: &mut serde_json::Value,
    session: &bebok_core::session::Session,
    default_model: Option<&str>,
) {
    let model = session
        .context_model
        .as_deref()
        .or(session.model.as_deref())
        .or(default_model);
    value["context_window"] = serde_json::json!(bebok_core::context::context_window_for(model));
}

/// WP-GIT: add `worktree_branch` (the branch name, or `null`) when the
/// session's directory is a Bebok worktree (`<root>/.bebok/worktrees/<branch>`).
/// Derived from the directory at response time - nothing is persisted, and
/// the client never has to split paths itself.
pub fn attach_worktree(value: &mut serde_json::Value, session: &bebok_core::session::Session) {
    let branch = bebok_core::git::worktree_info(std::path::Path::new(&session.directory))
        .map(|info| info.branch);
    value["worktree_branch"] = serde_json::json!(branch);
}

/// `GET /session/{id}` -> metadata + usage totals
pub async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let session = state
        .store
        .open_session(id)
        .await
        .map_err(|e| err_response(&e))?;
    let snapshot = session.meta_snapshot().await;
    let mut meta = serde_json::to_value(&snapshot).unwrap_or_default();
    meta["running"] = serde_json::json!(session.is_running());
    let default_model = session.config_snapshot().model_for(&snapshot.agent);
    attach_context_window(&mut meta, &snapshot, Some(&default_model));
    attach_worktree(&mut meta, &snapshot);
    Ok(Json(meta))
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

/// `GET /permission?directory=` restores pending asks after an SSE reconnect.
pub async fn pending_permissions(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let Some(directory) = q.directory else {
        return Err(ApiError::bad_request("missing ?directory= parameter").into_response());
    };
    let asks = state.store.pending_permissions(&directory).await;
    Ok(Json(serde_json::json!({ "asks": asks })))
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

    state.store.bus().publish(bebok_core::event::Event::new(
        "session.updated",
        session.directory(),
        &id.to_string(),
    ));

    Ok(Json(serde_json::json!({
        "sessionID": id.to_string(),
        "aborted": cancelled,
    })))
}

/// `POST /session/{id}/task/{taskID}/abort` -> cancel a specific child task.
///
/// When a child task is aborted, the cancellation propagates to the parent's
/// abort token — the entire orchestrator turn is cancelled so the model can
/// decide what to do next.
pub async fn abort_task(
    State(state): State<AppState>,
    Path((id, task_id)): Path<(Uuid, String)>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let session = state
        .store
        .open_session(id)
        .await
        .map_err(|e| err_response(&e))?;

    // Cancel the child token. This triggers the child's run_turn to break out
    // and return, which causes the task tool to emit task.ended with
    // status=aborted. The child's abort token was created as child_token() of
    // the parent, so cancelling it also marks the parent as cancelled — the
    // parent turn loop breaks and persists "[Turn aborted by user]".
    //
    // However, for the orchestrator to ask the user "what to do next?", we
    // need the parent turn to *not* be fully dead. The child_token() of the
    // parent's abort already handles this: when the child is cancelled, the
    // parent sees is_cancelled() = true and breaks out of its loop. The
    // services/turn.rs post-turn handler then sets running = false so the
    // user can send a new prompt.
    let cancelled = session.abort_child_task(&task_id).await;

    if cancelled {
        // Emit a descriptive event so the client knows which task was aborted.
        state.store.bus().publish(
            bebok_core::event::Event::new("task.aborted", session.directory(), &id.to_string())
                .with_properties(serde_json::json!({
                    "taskID": task_id,
                })),
        );
    }

    state.store.bus().publish(bebok_core::event::Event::new(
        "session.updated",
        session.directory(),
        &id.to_string(),
    ));

    Ok(Json(serde_json::json!({
        "sessionID": id.to_string(),
        "taskID": task_id,
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
/// is untouched on disk ("show full history" still available via `export`).
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

    let budget = body
        .budget
        .unwrap_or(source.config_snapshot().context_budget);
    let cutoff = compact_cutoff(&messages, budget);
    if cutoff == 0 {
        return Err(
            ApiError::bad_request("session fits the budget; nothing to compact").into_response(),
        );
    }

    let summary = bebok_core::context::compact_summary(&messages, cutoff);
    let source_meta = source.meta_snapshot().await;
    // "From" is the live gauge when a turn has run (provider-counted), else
    // the chars/4 estimate of the whole transcript; "to" is always estimated
    // because the fork has not been sent to a provider yet.
    let before = source_meta
        .context_used
        .unwrap_or_else(|| bebok_core::context::estimate_transcript(&messages));
    let fork = state
        .store
        .compact_session(id, summary, cutoff)
        .await
        .map_err(|e| err_response(&e))?;
    let after = bebok_core::context::estimate_transcript(&fork.messages_snapshot().await);

    // Visible marker at the end of the forked transcript (F6-4). Appended by
    // the route rather than inside `InstanceStore::compact_session` so the
    // fork mechanics stay untouched; it is a user-role text like the summary
    // itself, so providers see it as a plain note.
    let marker = compaction_marker(before, after, cutoff);
    fork.append_user_message(&marker)
        .await
        .map_err(|e| err_response(&e))?;
    // Seed the fork's gauge with the estimate so the meter reflects the
    // reduction immediately; the first real turn overwrites it.
    let model = source_meta
        .context_model
        .clone()
        .or(source_meta.model.clone())
        .unwrap_or_else(|| source.config_snapshot().model_for(&source_meta.agent));
    fork.set_context_used(after, &model).await;

    Ok(Json(serde_json::json!({
        "sessionID": fork.id().to_string(),
        "parent": serde_json::json!([id.to_string(), cutoff]),
        "before": before,
        "after": after,
    })))
}

/// The human-readable "Context compacted" note appended to a compacted fork.
pub fn compaction_marker(before: u64, after: u64, cutoff: usize) -> String {
    let noun = if cutoff == 1 { "message" } else { "messages" };
    format!(
        "[Context compacted: from {before} to {after} tokens ({cutoff} earlier {noun} summarized)]"
    )
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
    // WP-GIT: tell the client when the directory was a Bebok worktree so it
    // can *offer* removal. The worktree itself is never touched here - that
    // only happens through `POST /projects/{id}/git/worktree/remove`.
    let worktree = bebok_core::git::worktree_info(std::path::Path::new(&meta.directory));
    Ok(Json(serde_json::json!({
        "sessionID": id.to_string(),
        "directory": meta.directory,
        "deleted": true,
        "is_worktree": worktree.is_some(),
        "worktree_path": worktree.as_ref().map(|_| meta.directory.clone()),
        "worktree_branch": worktree.as_ref().map(|w| w.branch.clone()),
        "project_root": worktree.as_ref().map(|w| w.root.to_string_lossy().to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use bebok_core::session::Message;

    /// WP-GIT: the `worktree` field is additive - bodies without it still
    /// parse, bodies with it carry branch + optional base.
    #[test]
    fn create_session_body_accepts_an_optional_worktree_spec() {
        let plain: CreateSession =
            serde_json::from_str(r#"{"directory":"/p","agent":"code"}"#).unwrap();
        assert!(plain.worktree.is_none());
        let with: CreateSession =
            serde_json::from_str(r#"{"directory":"/p","worktree":{"branch":"bebok/session-1"}}"#)
                .unwrap();
        let spec = with.worktree.unwrap();
        assert_eq!(spec.branch, "bebok/session-1");
        assert!(spec.base.is_none());
        let with_base: CreateSession =
            serde_json::from_str(r#"{"directory":"/p","worktree":{"branch":"x","base":"main"}}"#)
                .unwrap();
        assert_eq!(with_base.worktree.unwrap().base.as_deref(), Some("main"));
    }

    #[test]
    fn attach_worktree_derives_the_branch_from_the_directory() {
        let root = std::env::temp_dir().join("proj");
        let wt = bebok_core::git::worktrees_dir(&root)
            .join("bebok")
            .join("feat");
        let session = bebok_core::session::Session::new(wt.to_string_lossy().to_string(), "code");
        let mut value = serde_json::json!({});
        attach_worktree(&mut value, &session);
        assert_eq!(value["worktree_branch"], "bebok/feat");

        let plain = bebok_core::session::Session::new(root.to_string_lossy().to_string(), "code");
        let mut value = serde_json::json!({});
        attach_worktree(&mut value, &plain);
        assert!(value["worktree_branch"].is_null());
    }

    #[test]
    fn compaction_marker_is_human_readable() {
        let text = compaction_marker(120_000, 30_000, 12);
        assert!(text.starts_with("[Context compacted: from 120000 to 30000 tokens"));
        assert!(text.contains("12 earlier messages"));
        assert!(compaction_marker(1, 1, 1).contains("1 earlier message summarized"));
    }

    #[test]
    fn compact_cutoff_keeps_the_latest_exchange_and_fits_half_budget() {
        // Six ~100-token messages (400 chars each) and a budget of 400 tokens:
        // the tail may hold 200 tokens -> the last 2 messages stay, 4 go.
        let messages: Vec<Message> = (0..6)
            .map(|i| Message::user(format!("{i}").repeat(400)))
            .collect();
        assert_eq!(compact_cutoff(&messages, 400), 4);
        // A generous budget keeps everything except the very first message
        // (the loop always leaves at least one message to summarize).
        assert_eq!(compact_cutoff(&messages, 100_000), 1);
    }
}
