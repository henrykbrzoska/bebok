//! Background processes (F9-14): `GET /session/{id}/processes`,
//! `GET /processes/{id}/log?tail=`, `POST /processes/{id}/kill`, plus the
//! bridge that republishes [`ProcessRegistry`] events on the engine
//! [`EventBus`] as `process.output` / `process.exited`.
//!
//! The registry itself lives in `bebok_tools::processes` (global, engine-wide);
//! this module only adds the session view (descendants + agent labels), the
//! port/url sniffing and the SSE throttle.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use bebok_core::InstanceStore;
use bebok_core::event::{Event, EventBus};
use bebok_core::session::Session;
use bebok_tools::processes::{ProcessEvent, ProcessInfo, ProcessRegistry};

use crate::error::{ApiError, err_response};
use crate::state::AppState;

/// Bytes of log inspected for a port / url.
const URL_SNIFF_BYTES: usize = 64 * 1024;
/// Minimum spacing between two `process.output` events of one process
/// (~3 events/s); chunks arriving faster are coalesced.
const OUTPUT_MIN_INTERVAL: Duration = Duration::from_millis(334);

/// One row of `GET /session/{id}/processes`: the registry snapshot plus the
/// owning agent's label and the port/url sniffed from the log.
#[derive(Debug, Clone, Serialize)]
pub struct ProcessEntry {
    #[serde(flatten)]
    pub info: ProcessInfo,
    /// Child alias, or the session's agent name for the main session.
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// `?tail=<bytes>` for the log endpoint.
#[derive(Deserialize)]
pub struct TailQuery {
    pub tail: Option<usize>,
}

/// `GET /session/{id}/processes` -> `{ processes: ProcessEntry[] }` for the
/// session and all its descendants (main session first, then children in
/// spawn order; within a session oldest process first).
pub async fn list_processes(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let rows = collect_processes(&state.store, id)
        .await
        .map_err(|e| err_response(&e))?;
    Ok(Json(serde_json::json!({ "processes": rows })))
}

/// `GET /processes/{id}/log?tail=` -> `{ id, log, size }`.
pub async fn process_log(
    Path(id): Path<String>,
    Query(q): Query<TailQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let registry = ProcessRegistry::global();
    if registry.get(&id).is_none() {
        return Err(ApiError::not_found(format!("unknown process {id}")).into_response());
    }
    let tail = q.tail;
    let read_id = id.clone();
    let (log, size) =
        tokio::task::spawn_blocking(move || registry.read_log_with_size(&read_id, tail))
            .await
            .map_err(|e| ApiError::internal(format!("log read task failed: {e}")).into_response())?
            .map_err(|e| ApiError::internal(format!("failed to read log: {e}")).into_response())?;
    Ok(Json(
        serde_json::json!({ "id": id, "log": log, "size": size }),
    ))
}

/// `POST /processes/{id}/kill` -> the `ProcessInfo` after the kill.
pub async fn kill_process(
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let registry = ProcessRegistry::global();
    if registry.get(&id).is_none() {
        return Err(ApiError::not_found(format!("unknown process {id}")).into_response());
    }
    let info = registry
        .kill(&id)
        .await
        .map_err(|e| ApiError::internal(format!("failed to kill process: {e}")).into_response())?;
    Ok(Json(serde_json::to_value(info).unwrap_or_default()))
}

/// The session view: `id` plus every descendant (sessions in the same
/// directory whose `parent` chain leads to `id`), labelled and url-sniffed.
/// Separated from the handler so it is testable against a bare store.
pub async fn collect_processes(
    store: &InstanceStore,
    id: Uuid,
) -> bebok_core::error::Result<Vec<ProcessEntry>> {
    let root = store.open_session(id).await?;
    let root_meta = root.meta_snapshot().await;
    let all: Vec<Session> = store.list_sessions(root.directory()).await;

    // parent -> children (spawn order).
    let mut children: HashMap<Uuid, Vec<&Session>> = HashMap::new();
    for s in &all {
        if let Some((parent, _)) = s.parent {
            children.entry(parent).or_default().push(s);
        }
    }
    for list in children.values_mut() {
        list.sort_by_key(|s| s.created_at);
    }

    // BFS: (session id, agent label), main first. The label is the alias when
    // the session is a named child, else its agent name (a main session has
    // no alias, so it reads e.g. "code"; a child rooted view keeps its alias).
    let label_of = |s: &Session| s.alias.clone().unwrap_or_else(|| s.agent.clone());
    let mut order: Vec<(String, String)> = vec![(id.to_string(), label_of(&root_meta))];
    let mut seen: HashSet<Uuid> = HashSet::from([id]);
    let mut queue: VecDeque<Uuid> = VecDeque::from([id]);
    while let Some(current) = queue.pop_front() {
        if let Some(kids) = children.get(&current) {
            for kid in kids {
                if seen.insert(kid.id) {
                    order.push((kid.id.to_string(), label_of(kid)));
                    queue.push_back(kid.id);
                }
            }
        }
    }

    let registry = ProcessRegistry::global();
    let mut rows = Vec::new();
    for (session_id, agent) in order {
        for info in registry.list(&session_id) {
            let sniffed = registry
                .read_log(&info.id, Some(URL_SNIFF_BYTES))
                .ok()
                .and_then(|text| detect_url(&text));
            rows.push(ProcessEntry {
                info,
                agent: agent.clone(),
                port: sniffed.as_ref().map(|(port, _)| *port),
                url: sniffed.map(|(_, url)| url),
            });
        }
    }
    Ok(rows)
}

/// Sniff the port a server announced in its log: the first
/// `http(s)://localhost:<port>` / `http(s)://127.0.0.1:<port>` /
/// `http(s)://0.0.0.0:<port>` url wins; otherwise the first `port <n>`
/// mention (e.g. "listening on port 3000") maps to `http://localhost:<n>`.
pub fn detect_url(log_text: &str) -> Option<(u16, String)> {
    const HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "0.0.0.0"];
    let mut best: Option<(usize, u16, String)> = None;
    for scheme in ["http://", "https://"] {
        for host in HOSTS {
            let needle = format!("{scheme}{host}:");
            let mut from = 0;
            while let Some(pos) = log_text[from..].find(&needle) {
                let start = from + pos;
                let after = start + needle.len();
                if let Some(port) = leading_port(&log_text[after..]) {
                    if best.as_ref().is_none_or(|(p, _, _)| start < *p) {
                        best = Some((start, port, format!("{scheme}{host}:{port}")));
                    }
                    break;
                }
                from = after;
            }
        }
    }
    if let Some((_, port, url)) = best {
        return Some((port, url));
    }

    // "port 3000" / "PORT: 3000" / "port=3000".
    let lower = log_text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find("port") {
        let start = from + pos;
        let after = start + 4;
        // Whole word: not "export", "report", "portal", ...
        let prev_ok = start == 0 || !lower.as_bytes()[start - 1].is_ascii_alphanumeric();
        let rest = &lower[after..];
        let trimmed = rest.trim_start_matches([' ', '\t', ':', '=']);
        if prev_ok
            && trimmed.len() != rest.len()
            && let Some(port) = leading_port(trimmed)
        {
            return Some((port, format!("http://localhost:{port}")));
        }
        from = after;
    }
    None
}

/// Parse the decimal port at the start of `s` (1..=65535, at most 5 digits,
/// not followed by another digit).
fn leading_port(s: &str) -> Option<u16> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).take(6).collect();
    if digits.is_empty() || digits.len() > 5 {
        return None;
    }
    let port: u32 = digits.parse().ok()?;
    if port == 0 || port > 65535 {
        return None;
    }
    Some(port as u16)
}

/// Republish registry events on the engine bus as `process.output`
/// `{ id, sessionID, chunk, at }` and `process.exited` `{ id, sessionID, code }`
/// (envelope `sessionID` = owning session, `directory` = its directory when
/// the session is known, else `""`). Output is throttled to at most ~3 events
/// per second per process by coalescing chunks; a pending chunk is always
/// flushed before the matching `process.exited`.
pub fn spawn_bridge(store: Arc<InstanceStore>) -> tokio::task::JoinHandle<()> {
    let bus = store.bus();
    let mut rx = ProcessRegistry::global().subscribe();
    tokio::spawn(async move {
        let mut bridge = Bridge {
            bus,
            store,
            dirs: HashMap::new(),
            pending: HashMap::new(),
            last_emit: HashMap::new(),
        };
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Ok(ProcessEvent::Output(o)) => bridge.on_output(o.id, o.session_id, o.chunk, o.at).await,
                    Ok(ProcessEvent::Exited(e)) => bridge.on_exited(e.id, e.session_id, e.code).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("process event bridge lagged, skipped {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                _ = tick.tick() => bridge.flush_due().await,
            }
        }
    })
}

struct PendingChunk {
    session_id: String,
    chunk: String,
    at: i64,
}

struct Bridge {
    bus: EventBus,
    store: Arc<InstanceStore>,
    /// session id -> directory (cached; `""` when unknown).
    dirs: HashMap<String, String>,
    /// process id -> coalesced chunk waiting for its slot.
    pending: HashMap<String, PendingChunk>,
    /// process id -> when its last `process.output` went out.
    last_emit: HashMap<String, tokio::time::Instant>,
}

impl Bridge {
    async fn directory_for(&mut self, session_id: &str) -> String {
        if let Some(dir) = self.dirs.get(session_id) {
            return dir.clone();
        }
        let dir = match Uuid::parse_str(session_id) {
            Ok(uuid) => self
                .store
                .session_meta(uuid)
                .await
                .map(|s| s.directory)
                .unwrap_or_default(),
            Err(_) => String::new(),
        };
        self.dirs.insert(session_id.to_string(), dir.clone());
        dir
    }

    async fn on_output(&mut self, id: String, session_id: String, chunk: String, at: i64) {
        if let Some(p) = self.pending.get_mut(&id) {
            p.chunk.push_str(&chunk);
            p.at = at;
            return;
        }
        let due = self
            .last_emit
            .get(&id)
            .is_none_or(|t| t.elapsed() >= OUTPUT_MIN_INTERVAL);
        if due {
            self.emit_output(&id, &session_id, chunk, at).await;
        } else {
            self.pending.insert(
                id,
                PendingChunk {
                    session_id,
                    chunk,
                    at,
                },
            );
        }
    }

    async fn on_exited(&mut self, id: String, session_id: String, code: Option<i32>) {
        if let Some(p) = self.pending.remove(&id) {
            self.emit_output(&id, &p.session_id, p.chunk, p.at).await;
        }
        self.last_emit.remove(&id);
        let directory = self.directory_for(&session_id).await;
        self.bus.publish(
            Event::new("process.exited", &directory, &session_id).with_properties(
                serde_json::json!({ "id": id, "sessionID": session_id, "code": code }),
            ),
        );
    }

    async fn flush_due(&mut self) {
        let due: Vec<String> = self
            .pending
            .keys()
            .filter(|id| {
                self.last_emit
                    .get(*id)
                    .is_none_or(|t| t.elapsed() >= OUTPUT_MIN_INTERVAL)
            })
            .cloned()
            .collect();
        for id in due {
            if let Some(p) = self.pending.remove(&id) {
                self.emit_output(&id, &p.session_id, p.chunk, p.at).await;
            }
        }
    }

    async fn emit_output(&mut self, id: &str, session_id: &str, chunk: String, at: i64) {
        let directory = self.directory_for(session_id).await;
        self.last_emit
            .insert(id.to_string(), tokio::time::Instant::now());
        self.bus.publish(
            Event::new("process.output", &directory, session_id).with_properties(
                serde_json::json!({ "id": id, "sessionID": session_id, "chunk": chunk, "at": at }),
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode, header};
    use tower::ServiceExt as _;

    use super::*;
    use crate::state::AppState;

    struct Fixture {
        base: std::path::PathBuf,
        root: std::path::PathBuf,
        state: AppState,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "bebok-processes-http-{tag}-{}",
                uuid::Uuid::new_v4()
            ));
            let root = base.join("root");
            std::fs::create_dir_all(&root).unwrap();
            let state = AppState {
                store: bebok_core::InstanceStore::with_data_dir(base.join("data")),
                #[cfg(not(target_os = "android"))]
                ptys: Arc::new(bebok_pty::PtyManager::new()),
                debug: Arc::new(bebok_core::DebugLog::new(base.join("debug.log"))),
                llm_trace: Arc::new(bebok_core::LlmTrace::new(2)),
            };
            Self { base, root, state }
        }

        fn router(&self) -> axum::Router {
            crate::routes::build_api_router().with_state(self.state.clone())
        }

        async fn session(&self) -> Arc<bebok_core::SessionState> {
            self.state
                .store
                .create_session(&self.root.to_string_lossy(), "code", None)
                .await
                .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    async fn json(router: axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let res = router.oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, value)
    }

    fn get_auth(uri: &str) -> Request<Body> {
        Request::builder()
            .method(Method::GET)
            .uri(uri)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", crate::auth::token()),
            )
            .body(Body::empty())
            .unwrap()
    }

    fn post_auth(uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", crate::auth::token()),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn long_command() -> &'static str {
        if cfg!(windows) {
            "ping -n 30 127.0.0.1"
        } else {
            "sleep 30"
        }
    }

    async fn wait_exited(id: &str) {
        for _ in 0..200 {
            if ProcessRegistry::global()
                .get(id)
                .is_some_and(|p| !p.is_running())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("process {id} did not exit");
    }

    #[test]
    fn detect_url_prefers_urls_then_port_mentions() {
        assert_eq!(
            detect_url("  ➜  Local:   http://localhost:5173/\n"),
            Some((5173, "http://localhost:5173".to_string()))
        );
        assert_eq!(
            detect_url("Server running at http://127.0.0.1:8080/api\n"),
            Some((8080, "http://127.0.0.1:8080".to_string()))
        );
        assert_eq!(
            detect_url("listening on port 3000\nLocal: http://localhost:4200/"),
            Some((4200, "http://localhost:4200".to_string())),
            "an explicit url wins over a port mention that appears earlier"
        );
        assert_eq!(
            detect_url("Listening on PORT 3000"),
            Some((3000, "http://localhost:3000".to_string()))
        );
        assert_eq!(
            detect_url("PORT=8000 started"),
            Some((8000, "http://localhost:8000".to_string()))
        );
        assert_eq!(
            detect_url("port: 9229"),
            Some((9229, "http://localhost:9229".to_string()))
        );
        assert_eq!(detect_url("compiled 12 modules"), None);
        assert_eq!(detect_url("export PATH=... report generated"), None);
        assert_eq!(detect_url("port 0 is invalid, port 70000 too"), None);
        assert_eq!(detect_url("http://localhost:abc"), None);
        assert_eq!(detect_url(""), None);
    }

    #[tokio::test]
    async fn session_processes_include_descendants_with_agent_labels() {
        let fx = Fixture::new("list");
        let main = fx.session().await;
        let child = fx
            .state
            .store
            .create_subagent_session(&main, "code", None, Some("api-orders"))
            .await
            .unwrap();
        let grandchild = fx
            .state
            .store
            .create_subagent_session(&child, "debug", None, None)
            .await
            .unwrap();
        let other = fx.session().await;

        let registry = ProcessRegistry::global();
        let p_main = registry
            .spawn_background(
                &main.id().to_string(),
                &fx.root,
                "echo Local: http://localhost:4200/",
            )
            .await
            .unwrap();
        let p_child = registry
            .spawn_background(&child.id().to_string(), &fx.root, long_command())
            .await
            .unwrap();
        let p_grand = registry
            .spawn_background(&grandchild.id().to_string(), &fx.root, "echo grand")
            .await
            .unwrap();
        let p_other = registry
            .spawn_background(&other.id().to_string(), &fx.root, "echo other")
            .await
            .unwrap();
        wait_exited(&p_main.id).await;

        let (status, value) = json(
            fx.router(),
            get_auth(&format!("/session/{}/processes", main.id())),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        let rows = value["processes"].as_array().unwrap();
        assert_eq!(rows.len(), 3, "{value}");
        assert_eq!(rows[0]["id"], p_main.id);
        assert_eq!(rows[0]["agent"], "code");
        assert_eq!(rows[0]["session_id"], main.id().to_string());
        assert_eq!(rows[0]["status"], "exited");
        assert_eq!(rows[0]["exit_code"], 0);
        assert_eq!(rows[0]["port"], 4200);
        assert_eq!(rows[0]["url"], "http://localhost:4200");
        assert!(rows[0]["log_path"].as_str().unwrap().ends_with(".log"));
        assert_eq!(rows[1]["id"], p_child.id);
        assert_eq!(rows[1]["agent"], "api-orders");
        assert_eq!(rows[1]["status"], "running");
        assert!(rows[1].get("port").is_none());
        assert!(rows[1].get("url").is_none());
        assert_eq!(rows[2]["id"], p_grand.id);
        assert_eq!(rows[2]["agent"], "debug");
        assert!(
            !rows.iter().any(|r| r["id"] == p_other.id),
            "unrelated session leaks in"
        );

        // The child's own view only lists its subtree.
        let (_, value) = json(
            fx.router(),
            get_auth(&format!("/session/{}/processes", child.id())),
        )
        .await;
        let rows = value["processes"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], p_child.id);
        assert_eq!(rows[0]["agent"], "api-orders");

        // Unknown session -> 404.
        let (status, _) = json(
            fx.router(),
            get_auth(&format!("/session/{}/processes", Uuid::new_v4())),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        registry.kill(&p_child.id).await.unwrap();
    }

    #[tokio::test]
    async fn log_and_kill_endpoints() {
        let fx = Fixture::new("logkill");
        let main = fx.session().await;
        let registry = ProcessRegistry::global();

        let short = registry
            .spawn_background(&main.id().to_string(), &fx.root, "echo hello-log")
            .await
            .unwrap();
        wait_exited(&short.id).await;
        let (status, value) = json(
            fx.router(),
            get_auth(&format!("/processes/{}/log", short.id)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["id"], short.id);
        assert!(
            value["log"].as_str().unwrap().contains("hello-log"),
            "{value}"
        );
        let size = value["size"].as_u64().unwrap();
        assert!(size >= "hello-log".len() as u64);

        let (status, value) = json(
            fx.router(),
            get_auth(&format!("/processes/{}/log?tail=4", short.id)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(value["log"].as_str().unwrap().len() <= 4, "{value}");
        assert_eq!(value["size"].as_u64().unwrap(), size);

        let (status, _) = json(fx.router(), get_auth("/processes/nope/log")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let long = registry
            .spawn_background(&main.id().to_string(), &fx.root, long_command())
            .await
            .unwrap();
        let (status, value) = json(
            fx.router(),
            post_auth(&format!("/processes/{}/kill", long.id), ""),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["id"], long.id);
        assert_eq!(value["status"], "exited");
        assert_eq!(value["pid"], long.pid);
        assert!(value["ended_at"].as_i64().is_some());
        assert_eq!(value["session_id"], main.id().to_string());

        let (status, _) = json(fx.router(), post_auth("/processes/nope/kill", "")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_session_kills_its_processes() {
        let fx = Fixture::new("delete");
        let main = fx.session().await;
        let registry = ProcessRegistry::global();
        let long = registry
            .spawn_background(&main.id().to_string(), &fx.root, long_command())
            .await
            .unwrap();
        assert!(registry.get(&long.id).unwrap().is_running());
        fx.state.store.delete_session(main.id()).await.unwrap();
        assert!(!registry.get(&long.id).unwrap().is_running());
    }

    #[tokio::test]
    async fn bridge_republishes_output_and_exit_on_the_bus() {
        let fx = Fixture::new("bridge");
        let main = fx.session().await;
        let bus = fx.state.store.bus();
        let mut rx = bus.subscribe();
        let bridge = spawn_bridge(fx.state.store.clone());

        let info = ProcessRegistry::global()
            .spawn_background(&main.id().to_string(), &fx.root, "echo bridged-line")
            .await
            .unwrap();

        let mut saw_output = false;
        let mut saw_exit = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        while (!saw_output || !saw_exit) && tokio::time::Instant::now() < deadline {
            let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await else {
                continue;
            };
            if ev.properties.get("id").and_then(|v| v.as_str()) != Some(info.id.as_str()) {
                continue;
            }
            assert_eq!(ev.session_id, main.id().to_string());
            assert_eq!(ev.directory, main.directory());
            assert_eq!(ev.properties["sessionID"], main.id().to_string());
            match ev.kind.as_str() {
                "process.output" => {
                    assert!(ev.properties["at"].as_i64().is_some());
                    if ev.properties["chunk"]
                        .as_str()
                        .unwrap()
                        .contains("bridged-line")
                    {
                        saw_output = true;
                    }
                }
                "process.exited" => {
                    // The waiter publishes the exit as soon as it happens; the
                    // tail keeps draining the log for ~1 s afterwards, so for a
                    // command this short the output may follow the exit.
                    assert_eq!(ev.properties["code"], 0);
                    saw_exit = true;
                }
                other => panic!("unexpected event {other}"),
            }
        }
        assert!(
            saw_output && saw_exit,
            "output={saw_output} exit={saw_exit}"
        );
        bridge.abort();
    }

    #[tokio::test]
    async fn bridge_throttles_output_to_three_per_second_and_coalesces() {
        let fx = Fixture::new("throttle");
        let mut bridge = Bridge {
            bus: fx.state.store.bus(),
            store: fx.state.store.clone(),
            dirs: HashMap::new(),
            pending: HashMap::new(),
            last_emit: HashMap::new(),
        };
        let mut rx = fx.state.store.bus().subscribe();
        for i in 0..10 {
            bridge
                .on_output("p1".into(), "not-a-uuid".into(), format!("l{i}\n"), i)
                .await;
        }
        // Only the first chunk went out immediately; the rest is pending.
        let first = rx.try_recv().unwrap();
        assert_eq!(first.kind, "process.output");
        assert_eq!(first.directory, "");
        assert_eq!(first.properties["chunk"], "l0\n");
        assert!(rx.try_recv().is_err());
        bridge.flush_due().await;
        assert!(rx.try_recv().is_err(), "not due yet");
        tokio::time::sleep(OUTPUT_MIN_INTERVAL + Duration::from_millis(20)).await;
        bridge.flush_due().await;
        let second = rx.try_recv().unwrap();
        assert_eq!(
            second.properties["chunk"],
            "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\n"
        );
        assert_eq!(second.properties["at"], 9);
        assert!(rx.try_recv().is_err());

        // Exit flushes whatever is pending first.
        bridge
            .on_output("p1".into(), "not-a-uuid".into(), "tail\n".into(), 11)
            .await;
        bridge
            .on_exited("p1".into(), "not-a-uuid".into(), Some(3))
            .await;
        let flushed = rx.try_recv().unwrap();
        assert_eq!(flushed.kind, "process.output");
        assert_eq!(flushed.properties["chunk"], "tail\n");
        let exited = rx.try_recv().unwrap();
        assert_eq!(exited.kind, "process.exited");
        assert_eq!(exited.properties["code"], 3);
        assert_eq!(exited.properties["id"], "p1");
        assert_eq!(exited.session_id, "not-a-uuid");
    }
}
