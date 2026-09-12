//! Explorer routes: `GET /fs/tree`, `GET /fs/file`, `PUT /fs/file`.

use axum::Json;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::error::{ApiError, err_response};
use crate::state::AppState;

/// `GET /fs/tree?directory=&path=` and `GET /fs/file?directory=&path=` query.
#[derive(Deserialize)]
pub struct FsQuery {
    pub directory: String,
    #[serde(default)]
    pub path: Option<String>,
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
    bebok_core::explorer::validate_rel(&instance.root, rel)
        .map_err(|e| ApiError::bad_request(e).into_response())?;
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
    bebok_core::explorer::validate_rel(&instance.root, rel)
        .map_err(|e| ApiError::bad_request(e).into_response())?;
    // GET /config is the only HTTP surface for config contents; it redacts
    // credentials. The generic file viewer must not expose the raw file.
    let target = instance.root.join(rel.replace('\\', "/"));
    let config_path = bebok_core::config::project_config_path(&instance.root);
    if let (Ok(target), Ok(config_path)) = (
        std::fs::canonicalize(target),
        std::fs::canonicalize(config_path),
    ) {
        if target == config_path {
            return Err(
                ApiError::forbidden("use /config to inspect project configuration").into_response(),
            );
        }
    }
    match bebok_core::explorer::read_file_text(&instance.root, rel) {
        Ok(text) => Ok(Json(serde_json::json!({ "path": rel, "content": text }))),
        Err(e) => Err(ApiError::bad_request(e.to_string()).into_response()),
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
        .ok_or_else(|| ApiError::bad_request("missing 'content'").into_response())?;
    match bebok_core::explorer::write_file_text(&instance.root, rel, content) {
        Ok(()) => Ok(Json(serde_json::json!({ "path": rel, "saved": true }))),
        Err(e) => Err(ApiError::bad_request(e.to_string()).into_response()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn file_route_rejects_windows_traversal_over_http() {
        let base = std::env::temp_dir().join(format!("bebok-fs-http-{}", uuid::Uuid::new_v4()));
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let state = AppState {
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

        let directory = root
            .to_string_lossy()
            .replace('\\', "%5C")
            .replace(':', "%3A")
            .replace('/', "%2F");
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!(
            "GET /fs/file?directory={directory}&path=..%5C..%5CWindows%5Cwin.ini HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        assert!(!response.contains("[fonts]"));
        server.abort();
        std::fs::remove_dir_all(base).unwrap();
    }
}
