//! Session change-tracking routes (WP-CHANGES / F6-8):
//! `GET /session/{id}/changes`, `GET /session/{id}/changes/diff?path=`,
//! `POST /session/{id}/changes/revert`.
//!
//! Thin facade over `bebok_core::change_tracking::Tracker`; the tracker does
//! blocking filesystem + `git` work, so each handler hops to a blocking task.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;

use bebok_core::change_tracking::Tracker;

use crate::error::{ApiError, err_response};
use crate::state::AppState;

/// `?path=&session=` query for the diff endpoint (`session` optional, F9-6:
/// a child session id from the aggregated listing).
#[derive(Deserialize)]
pub struct PathQuery {
    pub path: String,
    #[serde(default)]
    pub session: Option<String>,
}

/// `POST /session/{id}/changes/revert` body.
#[derive(Deserialize)]
pub struct RevertBody {
    pub path: String,
    #[serde(default)]
    pub session: Option<String>,
}

/// One session of the aggregated tree: its tracker plus the label the
/// Changes panel shows.
struct Tracked {
    session_id: String,
    agent: String,
    is_child: bool,
    tracker: Tracker,
}

/// The listed session first (label `main`), then every descendant session
/// (label = its alias, else its agent name) in spawn order (F9-6).
async fn session_tree(
    state: &AppState,
    id: Uuid,
) -> Result<Vec<Tracked>, axum::response::Response> {
    let session = state
        .store
        .open_session(id)
        .await
        .map_err(|e| err_response(&e))?;
    let mut out = vec![Tracked {
        session_id: id.to_string(),
        agent: "main".to_string(),
        is_child: false,
        tracker: Tracker::new(session.directory(), session.disk_dir()),
    }];
    for child in state.store.descendant_sessions(id).await {
        let Ok(child_state) = state.store.open_session(child.id).await else {
            continue;
        };
        out.push(Tracked {
            session_id: child.id.to_string(),
            agent: child.alias.clone().unwrap_or_else(|| child.agent.clone()),
            is_child: true,
            tracker: Tracker::new(child_state.directory(), child_state.disk_dir()),
        });
    }
    Ok(out)
}

/// Pick the tracker for one `(path, session?)`: the named session, else the
/// first session of the tree (main first) that tracks the path.
async fn tracker_for(
    state: &AppState,
    id: Uuid,
    path: &str,
    session: Option<&str>,
) -> Result<Tracker, axum::response::Response> {
    let tree = session_tree(state, id).await?;
    let path = path.to_string();
    let wanted = session.map(str::to_string);
    blocking(move || {
        if let Some(wanted) = wanted.as_deref() {
            return tree
                .into_iter()
                .find(|t| t.session_id == wanted)
                .map(|t| t.tracker)
                .ok_or_else(|| format!("session {wanted} is not part of this session's tree"));
        }
        let mut fallback: Option<Tracker> = None;
        for t in tree {
            if t.tracker.tracks(&path) {
                return Ok(t.tracker);
            }
            if fallback.is_none() {
                fallback = Some(t.tracker);
            }
        }
        fallback.ok_or_else(|| "session tree is empty".to_string())
    })
    .await
}

async fn blocking<T, F>(f: F) -> Result<T, axum::response::Response>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| {
            ApiError::internal(format!("change tracking task failed: {e}")).into_response()
        })?
        .map_err(|e| ApiError::bad_request(e).into_response())
}

/// `GET /session/{id}/changes` -> `{ changes: [ { path, added, removed, baseline, exists,
/// sessionID, agent, isChild } ] }` — the session's own tracked files first, then
/// those of every sub-agent session it spawned (recursively), each tagged with
/// the agent label that made the change (F9-6).
pub async fn list_changes(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let tree = session_tree(&state, id).await?;
    let changes = blocking(move || {
        let mut out = Vec::new();
        for t in tree {
            let mut entries = t.tracker.list();
            entries.sort_by(|a, b| a.path.cmp(&b.path));
            out.extend(
                entries
                    .into_iter()
                    .map(|e| e.tagged(&t.session_id, &t.agent, t.is_child)),
            );
        }
        Ok(out)
    })
    .await?;
    Ok(Json(serde_json::json!({ "changes": changes })))
}

/// `GET /session/{id}/changes/diff?path=[&session=]` -> `{ path, diff, baseline, added, removed }`.
pub async fn change_diff(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let tracker = tracker_for(&state, id, &q.path, q.session.as_deref()).await?;
    let diff = blocking(move || tracker.diff(&q.path)).await?;
    Ok(Json(serde_json::to_value(diff).unwrap_or_default()))
}

/// `POST /session/{id}/changes/revert` body `{ path, session? }` -> `{ path, baseline, exists }`.
pub async fn revert_change(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<RevertBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let tracker = tracker_for(&state, id, &body.path, body.session.as_deref()).await?;
    let result = blocking(move || tracker.revert(&body.path)).await?;
    Ok(Json(serde_json::to_value(result).unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode, header};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tower::ServiceExt as _;

    use crate::state::AppState;

    struct Fixture {
        base: std::path::PathBuf,
        root: std::path::PathBuf,
        state: AppState,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir()
                .join(format!("bebok-changes-http-{tag}-{}", uuid::Uuid::new_v4()));
            let root = base.join("root");
            std::fs::create_dir_all(&root).unwrap();
            let state = AppState {
                store: bebok_core::InstanceStore::with_data_dir(base.join("data")),
                #[cfg(not(target_os = "android"))]
                ptys: Arc::new(bebok_pty::PtyManager::new()),
                debug: Arc::new(bebok_core::DebugLog::new(base.join("debug.log"))),
                llm_trace: Arc::new(bebok_core::LlmTrace::new(2)),
                remote_extensions: Arc::new(tokio::sync::Mutex::new(
                    std::collections::HashMap::new(),
                )),
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

    #[tokio::test]
    async fn changes_routes_require_token_over_raw_socket() {
        let fx = Fixture::new("auth");
        let session = fx.session().await;
        let id = session.id();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = fx.router();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        for (method, path, body) in [
            ("GET", format!("/session/{id}/changes"), ""),
            ("GET", format!("/session/{id}/changes/diff?path=a.txt"), ""),
            (
                "POST",
                format!("/session/{id}/changes/revert"),
                r#"{"path":"a.txt"}"#,
            ),
        ] {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            let request = format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).await.unwrap();
            let response = String::from_utf8(response).unwrap();
            assert!(
                response.starts_with("HTTP/1.1 401"),
                "{method} {path}: {response}"
            );
        }
        server.abort();
    }

    #[tokio::test]
    async fn list_diff_and_revert_round_trip() {
        let fx = Fixture::new("roundtrip");
        let session = fx.session().await;
        let id = session.id();
        let file = fx.root.join("notes.txt");
        std::fs::write(&file, "alpha\nbeta\n").unwrap();

        // Empty until a tracked tool touches something.
        let (status, value) = json(fx.router(), get_auth(&format!("/session/{id}/changes"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["changes"].as_array().unwrap().len(), 0);

        // Simulate the exec.rs hook + the tool's write.
        let tracker =
            bebok_core::change_tracking::Tracker::new(session.directory(), session.disk_dir());
        tracker.snapshot_before_write("notes.txt").unwrap();
        std::fs::write(&file, "alpha\nBETA\ngamma\n").unwrap();

        let (status, value) = json(fx.router(), get_auth(&format!("/session/{id}/changes"))).await;
        assert_eq!(status, StatusCode::OK);
        let changes = value["changes"].as_array().unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0]["path"], "notes.txt");
        assert_eq!(changes[0]["added"], 2);
        assert_eq!(changes[0]["removed"], 1);
        assert_eq!(changes[0]["exists"], true);

        let (status, value) = json(
            fx.router(),
            get_auth(&format!("/session/{id}/changes/diff?path=notes.txt")),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["path"], "notes.txt");
        let diff = value["diff"].as_str().unwrap();
        assert!(diff.contains("-beta\n"), "{diff}");
        assert!(diff.contains("+BETA\n"), "{diff}");
        assert!(diff.contains("+gamma"), "{diff}");
        assert!(matches!(
            value["baseline"].as_str(),
            Some("git" | "snapshot")
        ));

        // Unknown path -> 400.
        let (status, _) = json(
            fx.router(),
            get_auth(&format!("/session/{id}/changes/diff?path=missing.txt")),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, value) = json(
            fx.router(),
            post_auth(
                &format!("/session/{id}/changes/revert"),
                r#"{"path":"notes.txt"}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["path"], "notes.txt");
        assert_eq!(value["exists"], true);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "alpha\nbeta\n");

        // After the revert the entry stays listed with zero deltas.
        let (_, value) = json(fx.router(), get_auth(&format!("/session/{id}/changes"))).await;
        let changes = value["changes"].as_array().unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0]["added"], 0);
        assert_eq!(changes[0]["removed"], 0);
    }

    #[tokio::test]
    async fn unknown_session_is_404_and_traversal_is_400() {
        let fx = Fixture::new("errors");
        let missing = uuid::Uuid::new_v4();
        let (status, _) = json(
            fx.router(),
            get_auth(&format!("/session/{missing}/changes")),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let session = fx.session().await;
        let id = session.id();
        let (status, _) = json(
            fx.router(),
            post_auth(
                &format!("/session/{id}/changes/revert"),
                r#"{"path":"../outside.txt"}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// F9-6: a sub-agent's writes show up in the parent's listing, tagged
    /// with the child's alias; diff and revert reach the child's tracker
    /// (explicitly via `session=` and implicitly by path lookup).
    #[tokio::test]
    async fn parent_listing_aggregates_child_sessions() {
        let fx = Fixture::new("aggregate");
        let parent = fx.session().await;
        let id = parent.id();
        let child = fx
            .state
            .store
            .create_subagent_session(&parent, "code", None, Some("api-orders"))
            .await
            .unwrap();
        let grandchild = fx
            .state
            .store
            .create_subagent_session(&child, "code", None, Some("nested"))
            .await
            .unwrap();

        std::fs::write(fx.root.join("own.txt"), "a\n").unwrap();
        bebok_core::change_tracking::Tracker::new(parent.directory(), parent.disk_dir())
            .snapshot_before_write("own.txt")
            .unwrap();
        std::fs::write(fx.root.join("own.txt"), "a\nb\n").unwrap();

        let child_tracker =
            bebok_core::change_tracking::Tracker::new(child.directory(), child.disk_dir());
        child_tracker.snapshot_before_write("orders.ts").unwrap();
        std::fs::write(fx.root.join("orders.ts"), "export {};\n").unwrap();

        bebok_core::change_tracking::Tracker::new(grandchild.directory(), grandchild.disk_dir())
            .snapshot_before_write("deep.txt")
            .unwrap();
        std::fs::write(fx.root.join("deep.txt"), "x\n").unwrap();

        let (status, value) = json(fx.router(), get_auth(&format!("/session/{id}/changes"))).await;
        assert_eq!(status, StatusCode::OK);
        let changes = value["changes"].as_array().unwrap();
        assert_eq!(changes.len(), 3, "{value}");
        assert_eq!(changes[0]["path"], "own.txt");
        assert_eq!(changes[0]["agent"], "main");
        assert_eq!(changes[0]["isChild"], false);
        assert_eq!(changes[0]["sessionID"], id.to_string());
        assert_eq!(changes[1]["path"], "orders.ts");
        assert_eq!(changes[1]["agent"], "api-orders");
        assert_eq!(changes[1]["isChild"], true);
        assert_eq!(changes[1]["sessionID"], child.id().to_string());
        assert_eq!(changes[2]["path"], "deep.txt");
        assert_eq!(changes[2]["agent"], "nested");

        // Diff by explicit session and by path lookup.
        let (status, value) = json(
            fx.router(),
            get_auth(&format!(
                "/session/{id}/changes/diff?path=orders.ts&session={}",
                child.id()
            )),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(value["diff"].as_str().unwrap().contains("+export"));
        let (status, value) = json(
            fx.router(),
            get_auth(&format!("/session/{id}/changes/diff?path=deep.txt")),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert!(value["diff"].as_str().unwrap().contains("+x"));

        // Revert a child's new file through the parent.
        let (status, value) = json(
            fx.router(),
            post_auth(
                &format!("/session/{id}/changes/revert"),
                &format!(r#"{{"path":"orders.ts","session":"{}"}}"#, child.id()),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["exists"], false);
        assert!(!fx.root.join("orders.ts").exists());

        // An unrelated session id is rejected.
        let (status, _) = json(
            fx.router(),
            get_auth(&format!(
                "/session/{id}/changes/diff?path=orders.ts&session={}",
                uuid::Uuid::new_v4()
            )),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
