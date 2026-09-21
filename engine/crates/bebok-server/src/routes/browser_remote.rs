//! Remote browser extension endpoints.
//!
//! Handles registration, heartbeats, and the pull-based command queue for
//! communicating with Chrome extensions that register with the engine.
//!
//! **Protocol (phase 2 — pull model):**
//!
//! 1. Extension registers: `POST /browser/register?port=&session_id=&directory=`
//! 2. Extension sends heartbeats every 5 s: `POST /browser/heartbeat?session_id=`
//! 3. Tool calls `POST /browser/remote/{action}?session_id=&directory=` → engine
//!    queues the command and **waits** (up to 30 s) for the result.
//! 4. Extension long-polls: `GET /browser/remote/pending?session_id=` → returns
//!    queued commands (waits up to 25 s if the queue is empty).
//! 5. Extension posts results: `POST /browser/remote/result` with
//!    `{id, session_id, result?, error?}` → engine wakes the waiting tool call.
//! 6. Pre-flight status check: `GET /browser/remote/status?session_id=&directory=`
//!    → returns all registered extensions and queue depths, allowing clients to
//!    verify an extension is alive before sending a blocking command.
//!
//! The tool-facing endpoint (`POST /browser/remote/{action}`) always returns
//! `{result: ...}` or `{error: "..."}` so that `RemoteClient` works without
//! changes (it extracts the `result` / `error` field from the JSON body).

use std::time::SystemTime;

use axum::extract::{Json, Path, Query, State};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::error::ApiError;
use crate::state::{AppState, CommandRegistry, QueuedCommand, RemoteExtension, WaiterHandle};

/// Maximum age (seconds) of a heartbeat before we consider the extension stale.
const STALE_THRESHOLD_SECS: i64 = 90;

// ── Query / body types ───────────────────────────────────────────────────

/// Query parameters for extension registration.
#[derive(Debug, Deserialize)]
pub struct RegisterQuery {
    /// The port the extension listens on (optional in pull mode — the
    /// extension does not need to expose a TCP listener).
    #[serde(default)]
    pub port: String,
    pub session_id: String,
    #[serde(default)]
    pub directory: String,
}

/// Query parameters for heartbeat.
#[derive(Debug, Deserialize)]
pub struct HeartbeatQuery {
    pub session_id: String,
    /// Optional; when the extension polls `pending` it supplies its session.
    #[allow(dead_code)]
    pub directory: Option<String>,
}

/// Query parameters for remote command forwarding.
#[derive(Debug, Deserialize)]
pub struct RemoteQuery {
    pub session_id: String,
    pub directory: Option<String>,
}

/// Query parameters for the long-poll pending endpoint.
#[derive(Debug, Deserialize)]
pub struct PendingQuery {
    pub session_id: Option<String>,
}

/// Query parameters for status endpoint.
#[derive(Debug, Deserialize)]
pub struct StatusQuery {
    pub session_id: Option<String>,
    pub directory: Option<String>,
}

/// Result payload posted back by the extension.
#[derive(Debug, Deserialize)]
pub struct ResultBody {
    pub id: String,
    pub session_id: Option<String>,
    pub result: Option<Value>,
    pub error: Option<String>,
}

// ── Responses ────────────────────────────────────────────────────────────

/// Response body for register and heartbeat.
#[derive(Debug, Serialize)]
pub struct OkResponse {
    pub ok: bool,
}

impl IntoResponse for OkResponse {
    fn into_response(self) -> axum::response::Response {
        axum::Json(json!({"ok": self.ok})).into_response()
    }
}

/// Extension info for status response.
#[derive(Debug, Serialize)]
pub struct ExtensionInfo {
    pub session_id: String,
    pub directory: String,
    pub last_seen: i64,
    pub last_seen_age_secs: i64,
}

/// Response for status endpoint.
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub ok: bool,
    pub extensions: Vec<ExtensionInfo>,
    pub queue: std::collections::HashMap<String, usize>,
}

// ── Drop guard: ensures a timed-out or cancelled waiter is removed ────────

struct WaiterGuard {
    command_id: String,
    queue: std::sync::Arc<tokio::sync::Mutex<CommandRegistry>>,
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        // We are in a sync context (tokio drop); spawning is fine because
        // MutexGuard is Send across threads. The lock is only held for the
        // map removal.
        let queue = self.queue.clone();
        let cmd_id = self.command_id.clone();
        tokio::spawn(async move {
            queue.lock().await.waiters.remove(&cmd_id);
        });
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Current unix timestamp in seconds, or error.
fn now_secs() -> Result<i64, ApiError> {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| ApiError::internal(format!("get timestamp: {e}")))
}

// ── Handlers ─────────────────────────────────────────────────────────────

/// Register a remote browser extension.
pub async fn register(
    State(state): State<AppState>,
    Query(query): Query<RegisterQuery>,
) -> Result<OkResponse, ApiError> {
    let now = now_secs()?;

    let ext = RemoteExtension {
        port: query.port,
        session_id: query.session_id.clone(),
        directory: query.directory,
        last_seen: now,
    };

    state
        .remote_extensions
        .lock()
        .await
        .insert(query.session_id, ext);

    Ok(OkResponse { ok: true })
}

/// Refresh the last_seen timestamp for a registered extension.
///
/// If the `session_id` is unknown (not in the registry), this handler
/// **creates** the entry rather than returning 404. This is intentional:
/// the MV3 service worker dies after ~30 s of inactivity, taking the
/// heartbeat loop with it. When the user wakes the worker, it resumes
/// heartbeating — but the engine may have restarted in the meantime and
/// lost the in-memory registry. Treating the first post-restart heartbeat
/// as a re-registration prevents a 404 → 503 cascade that would confuse
/// the user.
pub async fn heartbeat(
    State(state): State<AppState>,
    Query(query): Query<HeartbeatQuery>,
) -> Result<OkResponse, ApiError> {
    let now = now_secs()?;

    let mut registry = state.remote_extensions.lock().await;
    if let Some(ext) = registry.get_mut(&query.session_id) {
        ext.last_seen = now;
        // Self-heal: older extension builds heartbeat without `directory`;
        // adopt it from the query when provided so `/status?directory=`
        // filtering keeps matching.
        if let Some(dir) = query.directory.clone()
            && !dir.is_empty()
        {
            ext.directory = dir;
        }
    } else {
        // Entry missing — likely a post-restart heartbeat from a worker
        // that is still alive. Re-create the entry; preserve any existing
        // directory from the query or default to empty.
        let ext = RemoteExtension {
            port: String::new(),
            session_id: query.session_id.clone(),
            directory: query.directory.unwrap_or_default(),
            last_seen: now,
        };
        registry.insert(query.session_id, ext);
    }

    Ok(OkResponse { ok: true })
}

/// Get status of all registered extensions and queue depths.
pub async fn status(
    State(state): State<AppState>,
    Query(query): Query<StatusQuery>,
) -> Result<Json<StatusResponse>, ApiError> {
    let now = now_secs()?;

    // Build extensions list with optional filtering.
    let mut extensions: Vec<ExtensionInfo> = Vec::new();
    let registry = state.remote_extensions.lock().await;
    for ext in registry.values() {
        // Apply filters if provided.
        if let Some(ref sid) = query.session_id
            && ext.session_id != *sid
        {
            continue;
        }
        if let Some(ref dir) = query.directory
            && ext.directory != *dir
        {
            continue;
        }
        extensions.push(ExtensionInfo {
            session_id: ext.session_id.clone(),
            directory: ext.directory.clone(),
            last_seen: ext.last_seen,
            last_seen_age_secs: now.saturating_sub(ext.last_seen),
        });
    }
    drop(registry);

    // Build queue depth map (only sessions with depth > 0).
    let mut queue: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    {
        let q = state.command_queue.lock().await;
        for (sid, cmds) in q.queue.iter() {
            if !cmds.is_empty() {
                queue.insert(sid.clone(), cmds.len());
            }
        }
    }

    Ok(Json(StatusResponse {
        ok: true,
        extensions,
        queue,
    }))
}

/// Normalize the params payload for the `tabs` action.
///
/// The legacy Chrome extension sometimes sends `{"query": "x"}` (a string
/// value for `query`) which causes `chrome.tabs.query()` to throw
/// `No matching signature`. We normalise by:
/// - Using `{}` when `body` is `None` or not an object.
/// - Stripping the `query` key when it is not an object (e.g. a string).
fn normalize_tabs_params(body: Option<Value>) -> Value {
    match body {
        None => json!({}),
        Some(Value::Object(map)) => {
            let mut m = map;
            if let Some(q) = m.get("query")
                && !q.is_object()
            {
                m.remove("query");
            }
            Value::Object(m)
        }
        Some(_) => json!({}),
    }
}

/// Queue a command and wait for the extension to process it (up to 30 s).
///
/// This is the tool-facing endpoint.  `RemoteClient` calls
/// `POST /browser/remote/{action}` and expects `{result: ...}` or
/// `{error: "..."}`.  The handler enqueues the command, waits for the
/// extension to post a result, and returns it directly.
pub async fn remote(
    State(state): State<AppState>,
    Path(action): Path<String>,
    Query(query): Query<RemoteQuery>,
    body: Option<axum::extract::Json<Value>>,
) -> Result<axum::extract::Json<Value>, ApiError> {
    // 1. Resolve the target extension, checking staleness.
    let registry = state.remote_extensions.lock().await;
    let now = now_secs()?;

    let ext = registry
        .get(&query.session_id)
        .or_else(|| {
            query
                .directory
                .as_ref()
                .and_then(|dir| registry.values().find(|e| e.directory == *dir))
        })
        .cloned();
    drop(registry);

    let ext = ext.ok_or_else(|| {
        ApiError::service_unavailable(
            "no registered extension for the given session_id or directory".to_string(),
        )
    })?;

    // Check freshness: if the best candidate's last heartbeat is older
    // than 90 s, the MV3 service worker is almost certainly asleep.
    let age_secs = now.saturating_sub(ext.last_seen);
    if age_secs > STALE_THRESHOLD_SECS {
        return Err(ApiError::service_unavailable(format!(
            "extension last heartbeat {age_secs}s ago (stale > {STALE_THRESHOLD_SECS}s) \
             — worker is probably asleep; ask the user to open the extension Options page \
             / reload it, then retry"
        )));
    }

    // 2. Build and enqueue the command.
    let cmd_id = uuid::Uuid::new_v4().to_string();

    // Normalize params for the `tabs` action to guard against legacy
    // client bugs sending `{"query": "x"}`.
    let params = if action == "tabs" {
        normalize_tabs_params(body.map(|Json(v)| v))
    } else {
        body.map(|Json(v)| v).unwrap_or(json!({}))
    };

    let cmd = QueuedCommand {
        id: cmd_id.clone(),
        method: action.clone(),
        params,
        session_id: ext.session_id.clone(),
    };

    {
        let mut q = state.command_queue.lock().await;
        q.queue.entry(ext.session_id.clone()).or_default().push(cmd);
    }

    // 3. Create a oneshot channel for the result and register the waiter.
    let (tx, rx) = oneshot::channel::<Value>();
    {
        let mut q = state.command_queue.lock().await;
        q.waiters.insert(cmd_id.clone(), WaiterHandle { tx });
    }

    // Guard: if we are dropped (timeout / client disconnect) remove the
    // waiter so the next result does not crash on a closed channel.
    let _guard = WaiterGuard {
        command_id: cmd_id.clone(),
        queue: state.command_queue.clone(),
    };

    // 4. Wait for the result (30 s timeout). On timeout or client
    // disconnect `_guard` drops and removes the waiter.
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), rx)
        .await
        .map_err(|_| {
            ApiError::service_unavailable(format!(
                "extension did not return a result for '{action}' within 30 s"
            ))
        })?;

    // Channel closed = result was already consumed or extension dropped it.
    let value = result.map_err(|_| {
        ApiError::service_unavailable(format!(
            "extension dropped the result channel for '{action}'"
        ))
    })?;

    // 5. Return the raw result so RemoteClient can extract `result` / `error`.
    Ok(Json(value))
}

/// Long-poll: returns queued commands for the given session.
///
/// Waits up to 25 s if the queue is empty; returns `{commands: [...]}`
/// either way.  The extension calls this in a loop.
pub async fn pending(
    State(state): State<AppState>,
    Query(query): Query<PendingQuery>,
) -> Result<Json<Value>, ApiError> {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(25);

    loop {
        // Drain whatever is in the queue.
        let drained = {
            let mut q = state.command_queue.lock().await;
            if let Some(ref sid) = query.session_id {
                q.queue.remove(sid.as_str()).unwrap_or_default()
            } else {
                // No session filter — drain all sessions.
                let mut all = Vec::new();
                for cmds in q.queue.values_mut() {
                    all.append(cmds);
                }
                all
            }
        };

        if !drained.is_empty() {
            return Ok(Json(json!({ "commands": drained })));
        }

        // Nothing queued yet — sleep briefly then retry.
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(Json(json!({ "commands": [] })));
        }
        let remaining = deadline.duration_since(now);
        let sleep = std::cmp::min(remaining, tokio::time::Duration::from_millis(200));
        tokio::time::sleep(sleep).await;
    }
}

/// Receive a result from the extension and wake the waiting tool call.
///
/// Body: `{id, session_id, result?, error?}`.  `session_id` is required so
/// the engine knows which session the result belongs to; `id` is the command
/// id used to lookup the waiter.
pub async fn result(
    State(state): State<AppState>,
    Json(body): Json<ResultBody>,
) -> Result<Json<Value>, ApiError> {
    // Build the engine response to forward back to the tool.
    let engine_response = if let Some(error) = &body.error {
        json!({ "error": error })
    } else {
        json!({ "result": body.result.clone().unwrap_or(Value::Null) })
    };

    // session_id is required for logging/tracing (not for waiter lookup).
    let sid = body
        .session_id
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("result body must include session_id".to_string()))?;

    // Resolve waiter by command id (the id field in the body).
    let waiter = {
        let mut q = state.command_queue.lock().await;
        q.waiters.remove(&body.id)
    };

    let waiter = waiter.ok_or_else(|| {
        ApiError::not_found(format!(
            "no pending command with id {} (result session_id={sid})",
            body.id
        ))
    })?;

    // Send the result. If the receiver was dropped (timeout) the send
    // returns Err — that is fine, the waiter already returned an error.
    let _ = waiter.tx.send(engine_response);

    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::CommandRegistry;

    fn make_state() -> AppState {
        let base = std::env::temp_dir().join(format!(
            "bebok-browser-remote-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&base).unwrap();
        AppState {
            store: bebok_core::InstanceStore::with_data_dir(base.join("data")),
            #[cfg(not(target_os = "android"))]
            ptys: std::sync::Arc::new(bebok_pty::PtyManager::new()),
            debug: std::sync::Arc::new(bebok_core::DebugLog::new(base.join("debug.log"))),
            llm_trace: std::sync::Arc::new(bebok_core::LlmTrace::new(2)),
            remote_extensions: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            command_queue: std::sync::Arc::new(tokio::sync::Mutex::new(CommandRegistry::new())),
        }
    }

    #[tokio::test]
    async fn register_creates_extension() {
        let state = make_state();
        let resp = register(
            State(state.clone()),
            Query(RegisterQuery {
                port: "9222".into(),
                session_id: "s1".into(),
                directory: "/tmp".into(),
            }),
        )
        .await
        .expect("register should succeed");
        assert!(resp.ok);

        let exts = state.remote_extensions.lock().await;
        assert!(exts.contains_key("s1"));
        assert_eq!(exts["s1"].port, "9222");
    }

    #[tokio::test]
    async fn heartbeat_creates_entry_for_unknown_session() {
        let state = make_state();
        let resp = heartbeat(
            State(state.clone()),
            Query(HeartbeatQuery {
                session_id: "new-ext".into(),
                directory: Some("/home/user/project".into()),
            }),
        )
        .await
        .expect("heartbeat should succeed (re-register)");
        assert!(resp.ok);

        let exts = state.remote_extensions.lock().await;
        let ext = exts
            .get("new-ext")
            .expect("heartbeat should create the entry");
        assert_eq!(ext.session_id, "new-ext");
        assert_eq!(ext.directory, "/home/user/project");
        assert_eq!(ext.port, ""); // port is empty for heartbeat-created entries
        assert!(ext.last_seen > 0);
    }

    #[tokio::test]
    async fn heartbeat_refreshes_last_seen() {
        let state = make_state();
        let before = now_secs().unwrap();

        // First heartbeat — creates the entry.
        heartbeat(
            State(state.clone()),
            Query(HeartbeatQuery {
                session_id: "s1".into(),
                directory: None,
            }),
        )
        .await
        .unwrap();

        let ts1 = {
            let exts = state.remote_extensions.lock().await;
            exts["s1"].last_seen
        };
        assert!(ts1 >= before);

        // Sleep a tiny bit and send another heartbeat.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        heartbeat(
            State(state.clone()),
            Query(HeartbeatQuery {
                session_id: "s1".into(),
                directory: None,
            }),
        )
        .await
        .unwrap();

        let ts2 = {
            let exts = state.remote_extensions.lock().await;
            exts["s1"].last_seen
        };
        assert!(
            ts2 >= ts1,
            "last_seen should be refreshed (or same if sub-ms)"
        );
    }

    #[tokio::test]
    async fn heartbeat_fails_without_registration() {
        let state = make_state();
        // This is no longer expected to fail — heartbeat now re-registers.
        let result = heartbeat(
            State(state),
            Query(HeartbeatQuery {
                session_id: "unknown".into(),
                directory: None,
            }),
        )
        .await;
        assert!(
            result.is_ok(),
            "heartbeat for unknown session should now succeed"
        );
    }

    #[tokio::test]
    async fn status_empty_registry() {
        let state = make_state();
        let resp = status(
            State(state),
            Query(StatusQuery {
                session_id: None,
                directory: None,
            }),
        )
        .await
        .expect("status should succeed");

        assert!(resp.ok);
        assert!(resp.extensions.is_empty());
        assert!(resp.queue.is_empty());
    }

    #[tokio::test]
    async fn status_after_register() {
        let state = make_state();
        register(
            State(state.clone()),
            Query(RegisterQuery {
                port: "9222".into(),
                session_id: "s1".into(),
                directory: "/tmp".into(),
            }),
        )
        .await
        .expect("register should succeed");

        let resp = status(
            State(state),
            Query(StatusQuery {
                session_id: None,
                directory: None,
            }),
        )
        .await
        .expect("status should succeed");

        assert!(resp.ok);
        assert_eq!(resp.extensions.len(), 1);
        assert_eq!(resp.extensions[0].session_id, "s1");
        assert_eq!(resp.extensions[0].directory, "/tmp");
        assert!(resp.extensions[0].last_seen_age_secs >= 0);
        assert!(resp.extensions[0].last_seen_age_secs < 3600);
    }

    #[tokio::test]
    async fn status_filter_unknown_session() {
        let state = make_state();
        register(
            State(state.clone()),
            Query(RegisterQuery {
                port: "9222".into(),
                session_id: "s1".into(),
                directory: "/tmp".into(),
            }),
        )
        .await
        .expect("register should succeed");

        let resp = status(
            State(state),
            Query(StatusQuery {
                session_id: Some("unknown".into()),
                directory: None,
            }),
        )
        .await
        .expect("status should succeed");

        assert!(resp.ok);
        assert!(resp.extensions.is_empty());
    }

    #[tokio::test]
    async fn remote_fails_without_registration() {
        let state = make_state();
        let result = remote(
            State(state),
            Path("navigate".into()),
            Query(RemoteQuery {
                session_id: "s1".into(),
                directory: None,
            }),
            None,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn remote_rejects_stale_extension() {
        let state = make_state();

        // Register an extension with a "last_seen" far in the past.
        let stale_ts = now_secs().unwrap() - 200; // 200 s ago, well past the 90 s threshold
        state.remote_extensions.lock().await.insert(
            "s1".into(),
            RemoteExtension {
                port: "9222".into(),
                session_id: "s1".into(),
                directory: "/tmp".into(),
                last_seen: stale_ts,
            },
        );

        let err = remote(
            State(state),
            Path("navigate".into()),
            Query(RemoteQuery {
                session_id: "s1".into(),
                directory: None,
            }),
            None,
        )
        .await
        .expect_err("remote should fail for stale extension");

        match &err {
            ApiError::ServiceUnavailable(msg) => {
                assert!(
                    msg.contains("stale"),
                    "error message should mention staleness: {msg}"
                );
                assert!(
                    msg.contains("90"),
                    "error message should mention the threshold: {msg}"
                );
            }
            other => panic!("expected ServiceUnavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn queue_and_deliver_command() {
        let state = make_state();

        // Register extension.
        register(
            State(state.clone()),
            Query(RegisterQuery {
                port: "9222".into(),
                session_id: "s1".into(),
                directory: "/tmp".into(),
            }),
        )
        .await
        .expect("register should succeed");

        // Spawn a task that queues a command and waits for the result.
        let state_clone = state.clone();
        let wait_handle = tokio::spawn(async move {
            remote(
                State(state_clone),
                Path("navigate".into()),
                Query(RemoteQuery {
                    session_id: "s1".into(),
                    directory: None,
                }),
                Some(Json(json!({ "url": "https://example.com" }))),
            )
            .await
        });

        // Give the command time to be queued.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Extension polls pending.
        let pending_resp = pending(
            State(state.clone()),
            Query(PendingQuery {
                session_id: Some("s1".into()),
            }),
        )
        .await
        .expect("pending should succeed");

        let pending_val: Value = pending_resp.0;
        let commands = pending_val["commands"].as_array().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0]["method"], "navigate");

        let cmd_id = commands[0]["id"].as_str().unwrap().to_string();

        // Extension posts the result.
        let res = result(
            State(state.clone()),
            Json(ResultBody {
                id: cmd_id,
                session_id: Some("s1".into()),
                result: Some(json!({ "url": "https://example.com", "title": "Example" })),
                error: None,
            }),
        )
        .await
        .expect("result should succeed");
        assert_eq!(res.0["ok"], true);

        // The remote() call should have returned.
        let tool_response = wait_handle
            .await
            .expect("task should complete")
            .expect("remote should succeed");
        let body: Value = tool_response.0;
        assert_eq!(body["result"]["url"], "https://example.com");
    }

    #[tokio::test]
    async fn tabs_normalize_body_none() {
        let result = normalize_tabs_params(None);
        assert_eq!(result, json!({}));
    }

    #[tokio::test]
    async fn tabs_normalize_query_string() {
        let result = normalize_tabs_params(Some(json!({"query": "x"})));
        assert_eq!(result, json!({}));
    }

    #[tokio::test]
    async fn tabs_normalize_query_object_preserved() {
        let result = normalize_tabs_params(Some(json!({"query": {"active": true}})));
        assert_eq!(result, json!({"query": {"active": true}}));
    }

    #[tokio::test]
    async fn tabs_normalize_non_object_body() {
        let result = normalize_tabs_params(Some(json!("just a string")));
        assert_eq!(result, json!({}));
    }

    #[tokio::test]
    async fn tabs_normalize_object_preserves_valid_keys() {
        let result = normalize_tabs_params(Some(json!({"active": true, "currentWindow": true})));
        assert_eq!(result, json!({"active": true, "currentWindow": true}));
    }

    #[tokio::test]
    async fn pending_returns_empty_after_timeout() {
        let state = make_state();

        // Register extension.
        register(
            State(state.clone()),
            Query(RegisterQuery {
                port: "9222".into(),
                session_id: "s1".into(),
                directory: "/tmp".into(),
            }),
        )
        .await
        .expect("register should succeed");

        // Call pending with a very short effective timeout by checking
        // it returns within a reasonable time.
        let start = tokio::time::Instant::now();
        let resp = pending(
            State(state),
            Query(PendingQuery {
                session_id: Some("s1".into()),
            }),
        )
        .await
        .expect("pending should succeed");

        let elapsed = start.elapsed();
        // Should return empty after ~25 s.
        assert!(elapsed.as_secs() >= 20);
        let val: Value = resp.0;
        let commands = val["commands"].as_array().unwrap();
        assert!(commands.is_empty());
    }
}
