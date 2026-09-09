//! Explorer routes: `GET /fs/tree`, `GET /fs/file`, `PUT /fs/file`.

use axum::extract::{Query, State};
use axum::Json;
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
