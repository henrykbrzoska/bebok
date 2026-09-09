//! HTTP API (SPEC §5 subset; sessions + permission decisions for M2).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
#[cfg(not(target_os = "android"))]
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
#[cfg(not(target_os = "android"))]
use base64::Engine as _;
use futures::{Stream, stream};
#[cfg(not(target_os = "android"))]
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use bebok_core::agent::run_turn;
use bebok_core::config::{self, provider_from_model, ResolvedConfig};
use bebok_core::error::CoreError;
use bebok_core::event::EventBus;
use bebok_core::permission::{PermissionAnswer, ResolveOutcome};
use bebok_core::{InstanceStore, Instance, Runtimes, check_docker};
use bebok_llm::{AnthropicProvider, OpenAiProvider, Provider, ProviderKind};
#[cfg(not(target_os = "android"))]
use bebok_pty::SpawnOptions;

use crate::AppState;

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

/// `?directory=` query for endpoints that require it (agents/mcp/config).
/// `PUT /config` additionally accepts `?scope=global|project` (which layer to
/// write) and `?replace=true` (replace the whole file instead of merging).
#[derive(Deserialize)]
pub struct DirectoryQuery {
    pub directory: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub replace: Option<bool>,
}

/// `POST /mcp/{name}/toggle` body.
#[derive(Deserialize)]
pub struct ToggleBody {
    pub enabled: Option<bool>,
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

fn err_response(e: &CoreError) -> axum::response::Response {
    let status = match e {
        CoreError::SessionNotFound(_) => StatusCode::NOT_FOUND,
        CoreError::SessionBusy => StatusCode::CONFLICT,
        CoreError::ToolNotFound(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, format!("{e}\n")).into_response()
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
        return Err((
            StatusCode::BAD_REQUEST,
            "missing ?directory= parameter\n".to_string(),
        )
            .into_response());
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
    let session = state
        .store
        .open_session(id)
        .await
        .map_err(|e| err_response(&e))?;

    // One turn per session: synchronously claim the slot -> 409 otherwise.
    if !session.try_begin_turn() {
        return Err((
            StatusCode::CONFLICT,
            "session busy: a turn is already running\n".to_string(),
        )
            .into_response());
    }

    let instance = state
        .store
        .get_or_create_instance(session.directory())
        .await
        .map_err(|e| err_response(&e))?;
    let cfg = instance.config_snapshot();
    let meta = session.meta_snapshot().await;

    // The user may switch the agent mid-chat: an explicit `agent` on the prompt
    // wins and is persisted for subsequent turns.
    if let Some(agent) = body.agent.as_deref().filter(|a| !a.trim().is_empty()) {
        if agent != meta.agent {
            session.set_agent(agent).await;
        }
    }
    let effective_agent = body
        .agent
        .as_deref()
        .filter(|a| !a.trim().is_empty())
        .unwrap_or(&meta.agent);

    // Resolve the agent preset and the effective model (agent override ->
    // session override -> per-agent-type config -> resolved config).
    let mut agent = instance.resolve_agent(effective_agent);
    let model = body
        .model
        .as_deref()
        .filter(|m| !m.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            agent
                .model
                .clone()
                .or_else(|| meta.model.clone())
                .unwrap_or_else(|| cfg.model_for(&agent.name))
        });

    // Assemble AGENTS.md + enabled skills into the system prompt.
    let mut discovered = bebok_core::skills::discover(&instance.root);
    bebok_core::skills::apply_toggles(&mut discovered, Some(&cfg.skills));
    let instructions = bebok_core::skills::assemble_prompt(&discovered);
    if !instructions.is_empty() {
        agent.prompt = format!("{}\n\n{instructions}", agent.prompt);
    }

    // Surface mid-chat environment changes (MCP/skill/yolo toggles) to the model.
    let notes = instance.take_context_notes();
    if !notes.is_empty() {
        let mut block =
            "Recent environment changes during this conversation:\n".to_string();
        for note in &notes {
            block.push_str(&format!("- {note}\n"));
        }
        agent.prompt = format!("{}\n\n{block}", agent.prompt);
    }

    let provider = build_provider(&cfg, &model).map_err(|e| err_response(&e))?;

    // Append the user message, set the title, persist, emit.
    let user_idx = session
        .append_user_message(&body.message)
        .await
        .map_err(|e| err_response(&e))?;
    if session.set_title_if_empty(&body.message).await {
        session.touch().await;
    }

    let bus = state.store.bus();
    let abort = CancellationToken::new();
    session.set_abort(abort.clone()).await;

    // Announce the turn start so clients can show a global "working" indicator
    // even when the chat view is in the background.
    bus.publish(
        bebok_core::event::Event::new("session.updated", session.directory(), &id.to_string())
            .with_properties(serde_json::json!({ "running": true })),
    );

    let task_state = session.clone();
    let store = state.store.clone();
    tokio::spawn(async move {
        // Hold the turn mutex for the whole turn (flag already claimed above).
        let _guard = task_state.turn.lock().await;
        let result = run_turn(
            task_state.clone(),
            agent,
            instance.tools.clone(),
            provider,
            instance.permission.clone(),
            bus.clone(),
            abort.clone(),
            &model,
        )
        .await;
        task_state.clear_abort().await;
        task_state.end_turn();
        if let Err(e) = result {
            if abort.is_cancelled() {
                tracing::info!("turn aborted for session {id}: {e}");
            } else {
                tracing::error!("turn failed for session {id}: {e}");
                // Unstick GUI clients: the normal end-of-turn `session.updated`
                // never fires on this path (SPEC §3.11 fan-out).
                let _ = bus.publish(
                    bebok_core::event::Event::new(
                        "session.updated",
                        task_state.directory(),
                        &id.to_string(),
                    )
                    .with_properties(serde_json::json!({ "error": format!("{e}") })),
                );
            }
        }
        let _ = store; // keep the store alive for the turn
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "sessionID": id.to_string(),
            "messageIndex": user_idx,
            "status": "running",
        })),
    ))
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
            return Err((
                StatusCode::BAD_REQUEST,
                format!("decision must be 'allow' or 'deny', got '{other}'\n"),
            )
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
        ResolveOutcome::NotFound => Err((
            StatusCode::NOT_FOUND,
            format!("no pending permission request {request_id}\n"),
        )
            .into_response()),
    }
}

/// `GET /event` -> the single global SSE stream.
pub async fn event_stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let rx = state.store.bus().subscribe();
    let stream = stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let data = serde_json::to_string(&ev)
                        .unwrap_or_else(|_| "{}".to_string());
                    return Some((Ok(SseEvent::default().data(data)), rx));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("event stream lagged, dropped {n} events");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// `GET /agent?directory=` -> agent presets (built-ins + file presets).
pub async fn list_agents(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let agents = instance.agents.read().unwrap().list();
    Ok(Json(serde_json::json!({ "agents": agents })))
}

/// `GET /mcp?directory=` -> MCP servers + status.
pub async fn list_mcp(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let servers = instance.mcp.status();
    Ok(Json(serde_json::json!({ "servers": servers })))
}

/// `POST /mcp/{name}/toggle` -> enable/disable one MCP server.
///
/// Toggle = config edit: writes `mcp.<name>.enabled` to the project config,
/// re-syncs the MCP manager (connect/disconnect) and returns the new status.
pub async fn toggle_mcp(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<DirectoryQuery>,
    Json(body): Json<ToggleBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;

    // Determine the target enabled state (explicit value, else flip).
    let enabled = match body.enabled {
        Some(v) => v,
        None => {
            let current = instance
                .mcp
                .status()
                .into_iter()
                .find(|s| s.name == name)
                .map(|s| s.enabled)
                .unwrap_or(false);
            !current
        }
    };

    // Write the toggle to the project config (`mcp.<name>.enabled`).
    let mut cfg = instance.config_snapshot();
    let mut mcp = cfg.mcp.clone();
    let mcp_map = mcp
        .as_object_mut()
        .ok_or_else(|| err_response(&CoreError::Other("mcp config is not an object".into())))?;
    match mcp_map.get_mut(&name).and_then(|v| v.as_object_mut()) {
        Some(server) => {
            server.insert("enabled".to_string(), serde_json::json!(enabled));
        }
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("unknown mcp server '{name}'\n"),
            )
                .into_response());
        }
    }
    cfg.mcp = mcp;

    config::write_project_delta(&instance.root, &serde_json::json!({ "mcp": cfg.mcp }))
        .map_err(|e| err_response(&CoreError::Other(e)))?;

    // Surface the change to the model in the next prompt's context.
    instance.add_context_note(format!(
        "MCP server '{name}' was {} (its tools are {} available)",
        if enabled { "enabled" } else { "disabled" },
        if enabled { "now" } else { "no longer" }
    ));

    // Reload: re-reads config, re-syncs MCP, emits `config.changed`.
    let instance = state
        .store
        .reload_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let servers = instance.mcp.status();
    Ok(Json(serde_json::json!({ "name": name, "enabled": enabled, "servers": servers })))
}

/// Build the full `GET /config` payload: resolved view + raw config files.
/// `files` carries the untouched layer contents so the GUI can show/edit the
/// *actual* `config.json` text (resolved `config` is merged and unpatchable):
/// `files.global` = `~/.config/bebok/config.json`, `files.project` =
/// `<directory>/.bebok/config.json` (each `{ exists, path, content }`).
fn config_response(
    instance: &Instance,
) -> serde_json::Value {
    let cfg = instance.config_snapshot();
    let mut discovered = bebok_core::skills::discover(&instance.root);
    bebok_core::skills::apply_toggles(&mut discovered, Some(&cfg.skills));
    let servers = instance.mcp.status();
    let agents = instance.agents.read().unwrap().list();

    let global_path = config::global_config_path();
    let project_path = config::project_config_path(&instance.root);
    let layer = |path: &std::path::Path| {
        match std::fs::read_to_string(path) {
            Ok(content) => serde_json::json!({
                "exists": true,
                "path": path.to_string_lossy(),
                "content": content,
            }),
            Err(_) => serde_json::json!({
                "exists": false,
                "path": path.to_string_lossy(),
                "content": "{}",
            }),
        }
    };

    serde_json::json!({
        "config": cfg,
        "providers": cfg
            .resolved_providers()
            .iter()
            .map(|spec| {
                let mut v = serde_json::to_value(spec).unwrap_or(serde_json::json!({}));
                v["has_key"] = serde_json::json!(spec.has_key());
                v
            })
            .collect::<Vec<_>>(),
        "skills": discovered.skills,
        "mcp": servers,
        "agents": agents,
        "runtimes": Runtimes::from_config(&cfg.runtimes),
        "files": {
            "global": layer(&global_path),
            "project": layer(&project_path),
        },
    })
}

/// `GET /config?directory=` -> resolved config view (config + skills + mcp + agents).
pub async fn get_config(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    Ok(Json(config_response(&instance)))
}

/// `PUT /config?directory=` -> write a config delta to the project file and
/// reload. Returns the resolved config view (same shape as `GET /config`).
pub async fn put_config(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
    Json(delta): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    if !delta.is_object() {
        return Err((
            StatusCode::BAD_REQUEST,
            "config delta must be a JSON object\n".to_string(),
        )
            .into_response());
    }

    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;

    // Capture the previous skills/yolo so we can surface what changed.
    let old_cfg = instance.config_snapshot();

    // `?scope=global` writes to `~/.config/bebok/config.json`; default (or
    // `project`) writes to `<directory>/.bebok/config.json`. A full-object
    // body from the raw JSON editor replaces the whole file (keys absent from
    // the body are deleted); partial bodies merge top-level keys.
    let scope_global = q.scope.as_deref() == Some("global");
    let replace = q.replace.unwrap_or(false);
    if replace {
        let write = if scope_global {
            config::write_full_global(&delta)
        } else {
            config::write_full_project(&instance.root, &delta)
        };
        write.map_err(|e| err_response(&CoreError::Other(e)))?;
    } else if scope_global {
        config::write_global_delta(&delta).map_err(|e| err_response(&CoreError::Other(e)))?;
    } else {
        config::write_project_delta(&instance.root, &delta)
            .map_err(|e| err_response(&CoreError::Other(e)))?;
    }

    let instance = state
        .store
        .reload_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;

    let cfg = instance.config_snapshot();

    // Environment-change notes for the next prompt's context.
    if delta.get("skills").is_some() {
        for note in skills_diff_notes(&old_cfg.skills, &cfg.skills) {
            instance.add_context_note(note);
        }
    }
    if delta.get("yolo").is_some() {
        instance.add_context_note(format!(
            "YOLO mode was {} (permission prompts {} bypassed)",
            if cfg.yolo { "enabled" } else { "disabled" },
            if cfg.yolo { "now" } else { "no longer" }
        ));
    }

    Ok(Json(config_response(&instance)))
}

/// `GET /docker?directory=` -> Docker access probe (resolves `runtimes.docker`).
pub async fn check_docker_endpoint(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let runtimes = Runtimes::from_config(&instance.config_snapshot().runtimes);
    let status = check_docker(&runtimes.docker).await;
    Ok(Json(serde_json::json!({ "docker": status })))
}

// ---------------------------------------------------------------------------
// M6: explorer (/fs/*), providers (/models), session lifecycle (export/compact)
// ---------------------------------------------------------------------------

/// `GET /fs/tree?directory=&path=` and `GET /fs/file?directory=&path=` query.
#[derive(Deserialize)]
pub struct FsQuery {
    pub directory: String,
    #[serde(default)]
    pub path: Option<String>,
}

/// `GET /models?directory=&provider=` query.
#[derive(Deserialize)]
pub struct ModelsQuery {
    pub directory: String,
    pub provider: String,
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

/// `GET /debug/log` -> the debug log entries (LLM + HTTP requests/responses).
pub async fn debug_log(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "entries": state.debug.entries(),
        "maxChars": bebok_core::debug::DEBUG_LOG_MAX_CHARS,
    }))
}

/// `DELETE /debug/log` -> clear the debug log.
pub async fn debug_clear(State(state): State<AppState>) -> Json<serde_json::Value> {
    state.debug.clear();
    Json(serde_json::json!({ "ok": true }))
}

/// `GET /fs/tree?directory=&path=` -> immediate children of `path` (lazy,
/// gitignore-aware). The client expands a directory by requesting its path.
pub async fn fs_tree(
    State(state): State<AppState>,
    Query(q): Query<FsQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let rel = q.path.as_deref().unwrap_or(".");
    let entries = bebok_core::explorer::list_children(&instance.root, rel);
    Ok(Json(serde_json::json!({ "path": rel, "entries": entries })))
}

/// `GET /fs/file?directory=&path=` -> file content (for the viewer/diffs).
pub async fn fs_file(
    State(state): State<AppState>,
    Query(q): Query<FsQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let rel = q.path.as_deref().unwrap_or(".");
    match bebok_core::explorer::read_file_text(&instance.root, rel) {
        Ok(text) => Ok(Json(serde_json::json!({ "path": rel, "content": text }))),
        Err(e) => Err((StatusCode::BAD_REQUEST, format!("{e}\n")).into_response()),
    }
}

/// `PUT /fs/file?directory=&path=` -> save file content (explorer edit mode).
pub async fn fs_file_write(
    State(state): State<AppState>,
    Query(q): Query<FsQuery>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let rel = q.path.as_deref().unwrap_or(".");
    let content = body
        .get("content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "missing 'content'\n".to_string()).into_response())?;
    match bebok_core::explorer::write_file_text(&instance.root, rel, content) {
        Ok(()) => Ok(Json(serde_json::json!({ "path": rel, "saved": true }))),
        Err(e) => Err((StatusCode::BAD_REQUEST, format!("{e}\n")).into_response()),
    }
}

/// `GET /models?directory=&provider=` -> list a provider's available models and
/// persist them into the **project** config `.bebok/config.json` for the
/// instance (`<directory>`), so `GET /config` and the GUI's model selects see
/// them (the project layer is authoritative over the global config). This is
/// the GUI's "check available models" button.
pub async fn list_models(
    State(state): State<AppState>,
    Query(q): Query<ModelsQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let cfg = instance.config_snapshot();
    let spec = cfg.provider_spec(&q.provider).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("unknown provider '{}'\n", q.provider),
        )
            .into_response()
    })?;

    let models = bebok_llm::list_models(&spec).await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("failed to list models for '{}': {e}\n", q.provider),
        )
            .into_response()
    })?;

    // Persist the fetched models into the instance's project config providers
    // list, then reload so `GET /config` reflects them for the model selects.
    config::save_provider_models(&instance.root, &q.provider, spec.kind, &models)
        .map_err(|e| err_response(&CoreError::Other(e)))?;
    state
        .store
        .reload_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))
        .map(|_| ())?;

    Ok(Json(serde_json::json!({ "provider": q.provider, "models": models })))
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
        return Err((
            StatusCode::BAD_REQUEST,
            "session too short to compact\n".to_string(),
        )
            .into_response());
    }

    let budget = body.budget.unwrap_or(source.config_snapshot().context_budget);
    let cutoff = compact_cutoff(&messages, budget);
    if cutoff == 0 {
        return Err((
            StatusCode::BAD_REQUEST,
            "session fits the budget; nothing to compact\n".to_string(),
        )
            .into_response());
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

/// Diff two `skills` config sections and produce "skill X enabled/disabled"
/// notes for every skill whose toggle state changed.
fn skills_diff_notes(old: &serde_json::Value, new: &serde_json::Value) -> Vec<String> {
    use std::collections::BTreeSet;
    let old_map = old.as_object();
    let new_map = new.as_object();
    let keys: BTreeSet<String> = old_map
        .into_iter()
        .chain(new_map.into_iter())
        .flat_map(|m| m.keys().cloned())
        .collect();
    let mut notes = Vec::new();
    for key in keys {
        let before = old_map.and_then(|m| m.get(&key)).and_then(|v| v.as_bool());
        let after = new_map.and_then(|m| m.get(&key)).and_then(|v| v.as_bool());
        if before != after {
            notes.push(format!(
                "skill '{key}' was {} (its instructions {} in context)",
                if after == Some(true) { "enabled" } else { "disabled" },
                if after == Some(true) { "are now" } else { "are no longer" }
            ));
        }
    }
    notes
}

/// Choose a compaction cutoff: the number of leading messages to summarize such
/// that the remaining tail fits comfortably within half the budget (leaving
/// room for the model's reply), always keeping the latest exchange.
fn compact_cutoff(messages: &[bebok_core::session::Message], budget: usize) -> usize {
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

// ---------------------------------------------------------------------------
// M5: terminal (PTY)
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "android"))]
/// `POST /pty` body.
#[derive(Deserialize)]
pub struct CreatePtyBody {
    /// Working directory of the shell (project root).
    pub directory: Option<String>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub title: Option<String>,
}

#[cfg(not(target_os = "android"))]
/// `GET /pty/{id}/connect` query.
#[derive(Deserialize)]
pub struct ConnectQuery {
    pub ticket: String,
}

#[cfg(not(target_os = "android"))]
/// `POST /pty` -> spawn a terminal session, return `{ ptyId }`.
pub async fn create_pty(
    State(state): State<AppState>,
    Json(body): Json<CreatePtyBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let opts = SpawnOptions {
        cwd: body.directory.map(std::path::PathBuf::from),
        rows: body.rows.unwrap_or(bebok_pty::DEFAULT_ROWS),
        cols: body.cols.unwrap_or(bebok_pty::DEFAULT_COLS),
        title: body.title,
        shell: None,
        command: None,
    };
    let session = state.ptys.spawn(opts).map_err(pty_err_response)?;
    let pty_id = session.id().to_string();

    // Publish `pty.exited` on the global event bus when the session terminates.
    let bus = state.store.bus();
    let mut exit_rx = session.exit_rx();
    let pty_for_event = pty_id.clone();
    tokio::spawn(async move {
        loop {
            if exit_rx.changed().await.is_err() {
                return;
            }
            if let Some(code) = *exit_rx.borrow() {
                let _ = bus.publish(
                    bebok_core::event::Event::new("pty.exited", "", &pty_for_event)
                        .with_properties(serde_json::json!({
                            "ptyId": pty_for_event,
                            "exitCode": code,
                        })),
                );
                return;
            }
        }
    });

    Ok(Json(serde_json::json!({ "ptyId": pty_id })))
}

#[cfg(not(target_os = "android"))]
/// `GET /pty` -> all terminal sessions (for listing + reattach).
pub async fn list_ptys(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let ptys = state.ptys.list();
    Json(serde_json::json!({ "ptys": ptys }))
}

#[cfg(not(target_os = "android"))]
/// `POST /pty/{id}/ticket` -> a one-time, short-lived, scope-bound ticket.
pub async fn pty_ticket(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let ticket = state.ptys.issue_ticket(&id).map_err(pty_err_response)?;
    Ok(Json(serde_json::json!({ "ptyId": id, "ticket": ticket })))
}

#[cfg(not(target_os = "android"))]
/// `GET /pty/{id}/connect?ticket=...` -> WebSocket upgrade; the ticket is
/// consumed atomically (a second connect with the same ticket is rejected).
pub async fn pty_connect(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<ConnectQuery>,
) -> axum::response::Response {
    let Some(ticket_pty) = state.ptys.consume_ticket(&q.ticket) else {
        return (StatusCode::FORBIDDEN, "invalid or expired ticket\n").into_response();
    };
    if ticket_pty != id {
        return (StatusCode::FORBIDDEN, "ticket is not bound to this pty\n").into_response();
    }
    let Some(session) = state.ptys.get(&id) else {
        return (StatusCode::NOT_FOUND, "unknown pty\n").into_response();
    };
    ws.on_upgrade(move |socket| handle_pty_socket(socket, session))
        .into_response()
}

#[cfg(not(target_os = "android"))]
fn pty_err_response(e: bebok_pty::PtyError) -> axum::response::Response {
    let status = match &e {
        bebok_pty::PtyError::NotFound(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, format!("{e}\n")).into_response()
}

/// Stream the scrollback dump (binary frames), then live bytes; parse control
/// frames (JSON text) for `resize` / `input`.
#[cfg(not(target_os = "android"))]
async fn handle_pty_socket(socket: WebSocket, session: Arc<bebok_pty::PtySession>) {
    let (mut tx, mut rx) = socket.split();
    let mut client = session.connect();
    let pty = session; // separate Arc for control (avoids borrow conflicts)

    // 1. Dump scrollback first, chunked so a ~1 MB history stays bounded.
    let scrollback = std::mem::take(&mut client.scrollback);
    for chunk in scrollback.chunks(65536) {
        if tx.send(Message::Binary(chunk.to_vec().into())).await.is_err() {
            return;
        }
    }

    // 2. Live stream + control frames.
    loop {
        tokio::select! {
            msg = rx.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => handle_control_frame(&pty, &text).await,
                    Some(Ok(Message::Close(_))) | None => return,
                    Some(Ok(_)) => {} // binary / ping / pong ignored
                    Some(Err(_)) => return,
                }
            }
            chunk = client.recv() => {
                match chunk {
                    Some(bytes) => {
                        if tx.send(Message::Binary(bytes.to_vec().into())).await.is_err() {
                            return;
                        }
                    }
                    None => return, // pty exited
                }
            }
        }
    }
}

#[cfg(not(target_os = "android"))]
/// Parse one JSON text control frame: `resize` or `input` (base64 payload).
async fn handle_control_frame(pty: &Arc<bebok_pty::PtySession>, text: &str) {
    #[derive(Deserialize)]
    struct ControlFrame {
        #[serde(rename = "type")]
        kind: String,
        cols: Option<u16>,
        rows: Option<u16>,
        data: Option<String>,
    }
    let Ok(frame) = serde_json::from_str::<ControlFrame>(text) else {
        return;
    };
    match frame.kind.as_str() {
        "resize" => {
            if let (Some(rows), Some(cols)) = (frame.rows, frame.cols) {
                let _ = pty.resize(rows, cols);
            }
        }
        "input" => {
            if let Some(data) = frame.data {
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                    let _ = pty.send_input(bytes).await;
                }
            }
        }
        _ => {}
    }
}

/// Build the provider for a model. Selection is by model prefix (`zai/...`,
/// `openai/...`, `anthropic/...`, `xai/...`, `deepseek/...`, `google/...`,
/// `openrouter/...`, `ollama/...` or any provider name from config). The API
/// key comes from the provider spec (`api_key` field or the provider env var)
/// with a final fallback to the resolved `config.api_key`.
fn build_provider(config: &ResolvedConfig, model: &str) -> Result<Arc<dyn Provider>, CoreError> {
    let name = provider_from_model(model);
    let spec = config
        .provider_spec(&name)
        .ok_or_else(|| CoreError::Other(format!("unknown provider '{name}'")))?;

    let mut key = bebok_llm::resolve_api_key(&spec);
    if key.is_none() {
        key = config.api_key.clone().filter(|s| !s.trim().is_empty());
    }

    match spec.kind {
        ProviderKind::Anthropic => {
            let key = key.ok_or_else(|| {
                CoreError::Other(format!(
                    "no API key for provider '{name}': set its api_key or the {} env var",
                    spec.env_var()
                ))
            })?;
            Ok(Arc::new(
                AnthropicProvider::new(key).with_base_url(spec.chat_url()),
            ))
        }
        ProviderKind::Openai => Ok(Arc::new(OpenAiProvider::new(key, spec.chat_url()))),
    }
}

// Re-used by the prompt handler to keep the instance (and its tool registry)
// alive conceptually; the Arc itself is moved into the turn task.
#[allow(dead_code)]
fn _instance_alive(_bus: &EventBus, _instance: &Instance, _store: &InstanceStore) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    #[tokio::test]
    async fn decision_endpoint_resolves_pending_ask() {
        let base = std::env::temp_dir().join(format!("bebok-api-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let store = StdArc::new(InstanceStore::with_data_dir(base.join("data")));
        let state = AppState {
            store: store.clone(),
            #[cfg(not(target_os = "android"))]
            ptys: StdArc::new(bebok_pty::PtyManager::new()),
            debug: StdArc::new(bebok_core::DebugLog::new(base.join("debug.log"))),
        };
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();

        // Register a pending ask the same way the agent loop does.
        let request_id = "req-1".to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        session
            .register_permission_request(&request_id, tx)
            .await;

        let response = permission_decision(
            State(state.clone()),
            Path((session.id(), request_id.clone())),
            Json(PermissionBody {
                decision: "allow".to_string(),
                always: true,
            }),
        )
        .await
        .expect("first resolution succeeds");
        assert_eq!(response.0["resolved"], true);
        assert_eq!(response.0["decision"], "allow");

        // The agent loop's receiver got the answer with `always` intact.
        let answer = rx.await.expect("ask receiver got the decision");
        assert!(answer.allow);
        assert!(answer.always);

        // A second resolution for the same id -> 404 (first resolve wins).
        let again = permission_decision(
            State(state),
            Path((session.id(), request_id)),
            Json(PermissionBody {
                decision: "deny".to_string(),
                always: false,
            }),
        )
        .await;
        assert!(again.is_err(), "second resolution must not resolve");

        let _ = std::fs::remove_dir_all(&base);
    }
}


/// `GET /plugins` -> registered plugins + exposed hook points (introspection
/// for the plugin system: shows what is plugged in and where it can hook).
pub async fn list_plugins() -> Json<serde_json::Value> {
    let host = bebok_core::PluginHost::global();
    Json(serde_json::json!({
        "plugins": host.names().await,
        "hooks": bebok_core::hook_names(),
        "attached": true,
    }))
}
