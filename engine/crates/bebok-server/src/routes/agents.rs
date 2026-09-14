//! `GET /session/{id}/agents` (F6-12): live + historical sub-agents of a session.
//!
//! The `task`/`fleet` tools spawn real child sessions and keep the *running*
//! ones in `SessionState::child_tasks` (an in-memory map that was never
//! exposed over HTTP). Finished children are ordinary sessions in the same
//! directory whose `parent.0` points back at the spawner, and since F6-12 the
//! child carries its own `task_status` (persisted when `task.ended` fires).
//! This endpoint merges both views into one authoritative list so the Agents
//! panel can refetch it on every `task.started` / `task.ended` SSE event.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use serde::Serialize;
use uuid::Uuid;

use bebok_core::session::{Role, Session, UsageTotals};
use bebok_core::store::ChildTask;

use crate::error::err_response;
use crate::state::AppState;

/// Lifecycle of one delegated child as seen by the parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    /// WP-DELEGATION: registered but waiting for a `delegation.max_concurrent` slot.
    Queued,
    /// Still in the parent's live task map.
    Running,
    /// `task.ended` with `status: "completed"`.
    Done,
    /// `task.ended` with `status: "error"`.
    Failed,
    /// `task.ended` with `status: "aborted"`.
    Aborted,
    /// Child session with no persisted outcome (spawned before F6-12, or the
    /// engine restarted mid-turn); the transcript may still be inspected.
    Unknown,
}

/// One row of `GET /session/{id}/agents`. Field names follow the existing
/// `task.started` payload (`taskID`, `childSessionID`, `name`, `agent`) so the
/// client can reuse its `ActiveTask` DTO shape.
#[derive(Debug, Clone, Serialize)]
pub struct AgentEntry {
    /// Task id while running (`None` for historical children: the id is not
    /// persisted, only the child session is).
    #[serde(rename = "taskID", skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(rename = "childSessionID")]
    pub child_session_id: String,
    /// Orchestrator-assigned name (`alias` on the child session).
    pub name: String,
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub status: AgentStatus,
    /// First 80 chars of the delegated prompt (from the live map, or the
    /// child's first user message once finished).
    pub description: String,
    #[serde(rename = "startedAt")]
    pub started_at: i64,
    /// `None` while running; the child's `updated_at` afterwards.
    #[serde(rename = "endedAt", skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub usage: UsageTotals,
    /// WP-DELEGATION: live one-line progress (running children only), the
    /// same shape `task.progress` events carry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<bebok_core::agent::TaskProgress>,
    /// WP-DELEGATION: spawned with `background: true`.
    #[serde(default)]
    pub background: bool,
}

/// `?directory=` query of `GET /delegation/models` and `POST /fleet/generate`.
#[derive(Debug, serde::Deserialize)]
pub struct DelegationModelsQuery {
    pub directory: String,
}

/// F9-10: `GET /delegation/models?directory=` -> `{ policy, parent_model,
/// resolved, mappings: [{ provider, model, cheaper }] }` — what the
/// `delegation.model_policy` gives a sub-agent right now, plus the cheaper
/// sibling of the directory's default model, of every `models.<agent>`
/// entry, of each configured provider's models (first three) and of a
/// representative per known provider, so Settings can show the mapping.
pub async fn delegation_models(
    State(state): State<AppState>,
    Query(q): Query<DelegationModelsQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let cfg = instance.config_snapshot();
    let catalog = bebok_llm::ModelCatalog::global();
    let parent_model = cfg.model.clone();
    let resolved =
        bebok_core::agent::resolve_subagent_model(catalog, &cfg.delegation, &parent_model, None);
    let mut models: Vec<String> = vec![parent_model.clone()];
    if let Some(map) = cfg.models.as_object() {
        models.extend(map.values().filter_map(|v| v.as_str().map(str::to_string)));
    }
    for spec in cfg.resolved_providers() {
        for m in spec.models.iter().take(3) {
            if m.contains('/') {
                models.push(m.clone());
            } else {
                models.push(format!("{}/{m}", spec.name));
            }
        }
    }
    let mappings = bebok_core::agent::mappings_for(catalog, &models);
    Ok(Json(serde_json::json!({
        "policy": cfg.delegation.effective_model_policy().as_str(),
        "parent_model": parent_model,
        "resolved": resolved,
        "mappings": mappings,
    })))
}

/// `POST /fleet/generate?directory=` — ask a cheap configured LLM to plan a
/// fleet of sub-agents (min. 3 members per type: code/ask/plan/debug), picking
/// models from the configured provider pool with a bias against expensive
/// ones. A deterministic cheapest-first fallback fills any gap (or the whole
/// fleet when the LLM call fails), so the response always satisfies the
/// minimum — `fallback: true` + `warning` say which path was taken.
///
/// Body (all optional): `{ "minPerType": 3, "types": ["code","ask","plan","debug"] }`.
pub async fn generate_fleet(
    State(state): State<AppState>,
    Query(q): Query<DelegationModelsQuery>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let opts: bebok_core::fleet_gen::FleetGenOptions = match body {
        Some(Json(v)) => serde_json::from_value(v).map_err(|e| {
            crate::error::ApiError::bad_request(format!("invalid fleet options: {e}"))
                .into_response()
        })?,
        None => serde_json::from_value(serde_json::json!({})).expect("empty options are valid"),
    };
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let cfg = instance.config_snapshot();
    let result = bebok_core::fleet_gen::generate_fleet(&cfg, opts)
        .await
        .map_err(|e| err_response(&e))?;
    Ok(Json(
        serde_json::to_value(&result).expect("FleetGenResult serializes"),
    ))
}

/// `GET /session/{id}/agents` -> `{ agents: [...] }`, running first, then by
/// `started_at` descending.
pub async fn list_agents(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let agents = collect_agents(&state.store, id)
        .await
        .map_err(|e| err_response(&e))?;
    Ok(Json(serde_json::json!({ "agents": agents })))
}

/// The merge itself, separated from the handler so it is unit-testable
/// against a bare `InstanceStore` (no HTTP, no LLM).
pub async fn collect_agents(
    store: &bebok_core::InstanceStore,
    id: Uuid,
) -> bebok_core::error::Result<Vec<AgentEntry>> {
    let parent = store.open_session(id).await?;
    let running = parent.child_tasks_snapshot().await;
    let children: Vec<Session> = store
        .list_sessions(parent.directory())
        .await
        .into_iter()
        .filter(|s| s.parent.map(|(p, _)| p == id).unwrap_or(false))
        .collect();
    let by_id: HashMap<String, &Session> = children.iter().map(|s| (s.id.to_string(), s)).collect();

    let mut entries: Vec<AgentEntry> = Vec::with_capacity(children.len() + running.len());

    // (a) live children: authoritative `running`, enriched with the child's
    // usage so far when its session is known.
    for task in &running {
        // The child's live state (usage + transcript for the progress line)
        // when its session is open, else the persisted meta only.
        let (live_session, progress) = match uuid::Uuid::parse_str(&task.child_session_id) {
            Ok(child_id) => match store.open_session(child_id).await {
                Ok(child) => (
                    Some(child.meta_snapshot().await),
                    Some(bebok_core::agent::summarize_progress(
                        &child.messages_snapshot().await,
                    )),
                ),
                Err(_) => (None, None),
            },
            Err(_) => (None, None),
        };
        entries.push(running_entry(
            task,
            live_session
                .as_ref()
                .or_else(|| by_id.get(&task.child_session_id).copied()),
            progress,
        ));
    }

    // (b) finished children, skipping anything still in the live map.
    for child in &children {
        let child_id = child.id.to_string();
        if running.iter().any(|t| t.child_session_id == child_id) {
            continue;
        }
        let description = match store.open_session(child.id).await {
            Ok(session) => first_user_text(&session.messages_snapshot().await),
            Err(_) => String::new(),
        };
        entries.push(finished_entry(child, description));
    }

    entries.sort_by(|a, b| {
        let rank =
            |s: AgentStatus| u8::from(!matches!(s, AgentStatus::Running | AgentStatus::Queued));
        rank(a.status)
            .cmp(&rank(b.status))
            .then_with(|| b.started_at.cmp(&a.started_at))
    });
    Ok(entries)
}

fn running_entry(
    task: &ChildTask,
    session: Option<&Session>,
    progress: Option<bebok_core::agent::TaskProgress>,
) -> AgentEntry {
    AgentEntry {
        task_id: Some(task.task_id.clone()),
        child_session_id: task.child_session_id.clone(),
        name: task.name.clone(),
        agent: task.agent.clone(),
        model: task
            .model
            .clone()
            .or_else(|| session.and_then(|s| s.model.clone())),
        status: if task.status == "queued" {
            AgentStatus::Queued
        } else {
            AgentStatus::Running
        },
        description: task.description.clone(),
        started_at: task.started_at,
        ended_at: None,
        error: None,
        usage: session.map(|s| s.usage.clone()).unwrap_or_default(),
        progress,
        background: task.background,
    }
}

fn finished_entry(child: &Session, description: String) -> AgentEntry {
    let status = match child.task_status.as_deref() {
        Some("completed") => AgentStatus::Done,
        Some("error") => AgentStatus::Failed,
        Some("aborted") => AgentStatus::Aborted,
        _ => AgentStatus::Unknown,
    };
    AgentEntry {
        task_id: None,
        child_session_id: child.id.to_string(),
        name: child
            .alias
            .clone()
            .or_else(|| child.title.clone())
            .unwrap_or_default(),
        agent: child.agent.clone(),
        model: child.model.clone(),
        status,
        description,
        started_at: child.created_at,
        ended_at: Some(child.updated_at),
        error: child.task_error.clone(),
        usage: child.usage.clone(),
        progress: None,
        background: false,
    }
}

/// First 80 chars of the first user message (the delegated prompt).
fn first_user_text(messages: &[bebok_core::session::Message]) -> String {
    messages
        .iter()
        .find(|m| m.role == Role::User)
        .map(|m| m.text_content().chars().take(80).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn temp_base(tag: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("bebok-agents-{tag}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    /// A `task`-style child goes running -> done through the merged view
    /// without a live LLM: register it in the parent's live map, then do what
    /// the tool does on `task.ended` (persist the status, unregister).
    #[tokio::test]
    async fn merged_view_reflects_running_then_done() {
        let base = temp_base("merge");
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let store = bebok_core::InstanceStore::with_data_dir(base.join("data"));
        let parent = store
            .create_session(project.to_str().unwrap(), "orchestrator", None)
            .await
            .unwrap();
        let child = store
            .create_subagent_session(&parent, "code", Some("test-model"), Some("fix-ci"))
            .await
            .unwrap();
        child
            .append_user_message("please fix the CI")
            .await
            .unwrap();

        // Nothing registered yet: the child is a finished-looking session
        // with no persisted outcome -> `unknown`.
        let before = collect_agents(&store, parent.id()).await.unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].status, AgentStatus::Unknown);
        assert_eq!(before[0].child_session_id, child.id().to_string());

        parent
            .register_child_task_with_model(
                "task-1",
                "please fix the CI",
                &child.id().to_string(),
                "fix-ci",
                "code",
                Some("test-model"),
                CancellationToken::new(),
            )
            .await;
        let running = collect_agents(&store, parent.id()).await.unwrap();
        assert_eq!(running.len(), 1, "live + historical must not duplicate");
        assert_eq!(running[0].status, AgentStatus::Running);
        assert_eq!(running[0].task_id.as_deref(), Some("task-1"));
        assert_eq!(running[0].model.as_deref(), Some("test-model"));
        assert_eq!(running[0].name, "fix-ci");
        assert!(running[0].ended_at.is_none());

        child.set_task_status("completed", None).await;
        parent.unregister_child_task("task-1").await;
        let done = collect_agents(&store, parent.id()).await.unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].status, AgentStatus::Done);
        assert!(done[0].task_id.is_none());
        assert!(done[0].ended_at.is_some());
        assert_eq!(done[0].description, "please fix the CI");
        assert_eq!(done[0].agent, "code");

        // The transcript stays retrievable through the ordinary session API.
        let transcript = store.open_session(child.id()).await.unwrap();
        assert_eq!(transcript.messages_snapshot().await.len(), 1);

        // Failure + abort map to their own statuses.
        child.set_task_status("error", Some("boom")).await;
        let failed = collect_agents(&store, parent.id()).await.unwrap();
        assert_eq!(failed[0].status, AgentStatus::Failed);
        assert_eq!(failed[0].error.as_deref(), Some("boom"));
        child.set_task_status("aborted", None).await;
        let aborted = collect_agents(&store, parent.id()).await.unwrap();
        assert_eq!(aborted[0].status, AgentStatus::Aborted);

        // Wire shape: camelCase ids, lowercase status.
        let json = serde_json::to_value(&aborted[0]).unwrap();
        assert_eq!(json["status"], "aborted");
        assert!(json.get("childSessionID").is_some());
        assert!(json.get("startedAt").is_some());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn running_children_sort_first() {
        let base = temp_base("sort");
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let store = bebok_core::InstanceStore::with_data_dir(base.join("data"));
        let parent = store
            .create_session(project.to_str().unwrap(), "orchestrator", None)
            .await
            .unwrap();
        let older = store
            .create_subagent_session(&parent, "code", None, Some("older"))
            .await
            .unwrap();
        older.set_task_status("completed", None).await;
        let newer = store
            .create_subagent_session(&parent, "code", None, Some("newer"))
            .await
            .unwrap();
        parent
            .register_child_task(
                "t-newer",
                "d",
                &newer.id().to_string(),
                "newer",
                "code",
                CancellationToken::new(),
            )
            .await;
        let list = collect_agents(&store, parent.id()).await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "newer");
        assert_eq!(list[0].status, AgentStatus::Running);
        assert_eq!(list[1].name, "older");
        assert_eq!(list[1].status, AgentStatus::Done);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Same raw-socket pattern as `routes::fs` / `routes::projects`: the real
    /// router with the capability-token layer, over a loopback socket.
    async fn serve() -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<()>,
        std::path::PathBuf,
    ) {
        let base = temp_base("http");
        let state = crate::state::AppState {
            store: bebok_core::InstanceStore::with_data_dir(base.join("data")),
            #[cfg(not(target_os = "android"))]
            ptys: Arc::new(bebok_pty::PtyManager::new()),
            debug: Arc::new(bebok_core::DebugLog::new(base.join("debug.log"))),
            llm_trace: Arc::new(bebok_core::LlmTrace::new(2)),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                crate::routes::build_api_router().with_state(state),
            )
            .await
            .unwrap();
        });
        (address, server, base)
    }

    async fn raw(address: std::net::SocketAddr, request: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    }

    #[tokio::test]
    async fn agents_route_requires_the_capability_token() {
        let (address, server, base) = serve().await;
        let id = Uuid::new_v4();
        let response = raw(
            address,
            &format!(
                "GET /session/{id}/agents HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
            ),
        )
        .await;
        // `BEBOK_NO_AUTH` is never set in the test process, so the layer is live.
        assert!(
            response.starts_with("HTTP/1.1 401") || response.starts_with("HTTP/1.1 403"),
            "{response}"
        );
        server.abort();
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn agents_route_returns_404_for_an_unknown_session() {
        let (address, server, base) = serve().await;
        let auth = crate::auth::token();
        let id = Uuid::new_v4();
        let response = raw(
            address,
            &format!(
                "GET /session/{id}/agents HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {auth}\r\nConnection: close\r\n\r\n"
            ),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 404"), "{response}");
        server.abort();
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn fleet_generate_responds_with_valid_shape() {
        let (address, server, base) = serve().await;
        let auth = crate::auth::token();
        let dir = base.join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        let directory = dir
            .to_string_lossy()
            .replace('\\', "%5C")
            .replace(':', "%3A")
            .replace('/', "%2F");
        let body = "{}";
        let response = raw(
            address,
            &format!(
                "POST /fleet/generate?directory={directory} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        )
        .await;
        // Either 400 (no providers/keys in this environment) or 200 with a
        // valid FleetGenResult — but never a hang and never HTML/garbage.
        if response.starts_with("HTTP/1.1 200") {
            let json_start = response.find('{').expect("JSON body");
            let parsed: serde_json::Value =
                serde_json::from_str(&response[json_start..]).expect("valid JSON");
            let members = parsed["members"].as_array().expect("members array");
            assert!(members.len() >= 12, "min 3 per type x 4 types: {parsed}");
            assert!(parsed.get("generationModel").is_some(), "{parsed}");
            assert!(parsed.get("fallback").is_some(), "{parsed}");
        } else {
            assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        }
        server.abort();
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn fleet_generate_rejects_orchestrator_type() {
        let (address, server, base) = serve().await;
        let auth = crate::auth::token();
        let dir = base.join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        let directory = dir
            .to_string_lossy()
            .replace('\\', "%5C")
            .replace(':', "%3A")
            .replace('/', "%2F");
        let body = r#"{"types":["code","orchestrator"]}"#;
        let response = raw(
            address,
            &format!(
                "POST /fleet/generate?directory={directory} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        assert!(
            response.contains("orchestrator") || response.contains("invalid agent type"),
            "{response}"
        );
        server.abort();
        let _ = std::fs::remove_dir_all(base);
    }
}
