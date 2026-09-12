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

/// `?path=` query for the diff endpoint.
#[derive(Deserialize)]
pub struct PathQuery {
    pub path: String,
}

/// `POST /session/{id}/changes/revert` body.
#[derive(Deserialize)]
pub struct RevertBody {
    pub path: String,
}

async fn tracker_for(state: &AppState, id: Uuid) -> Result<Tracker, axum::response::Response> {
    let session = state
        .store
        .open_session(id)
        .await
        .map_err(|e| err_response(&e))?;
    Ok(Tracker::new(session.directory(), session.disk_dir()))
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

/// `GET /session/{id}/changes` -> `{ changes: [ { path, added, removed, baseline, exists } ] }`.
pub async fn list_changes(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let tracker = tracker_for(&state, id).await?;
    let changes = blocking(move || Ok(tracker.list())).await?;
    Ok(Json(serde_json::json!({ "changes": changes })))
}

/// `GET /session/{id}/changes/diff?path=` -> `{ path, diff, baseline, added, removed }`.
pub async fn change_diff(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let tracker = tracker_for(&state, id).await?;
    let diff = blocking(move || tracker.diff(&q.path)).await?;
    Ok(Json(serde_json::to_value(diff).unwrap_or_default()))
}

/// `POST /session/{id}/changes/revert` body `{ path }` -> `{ path, baseline, exists }`.
pub async fn revert_change(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<RevertBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let tracker = tracker_for(&state, id).await?;
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
}
