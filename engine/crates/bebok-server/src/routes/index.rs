//! Code-index routes (Faza 1, PR1-część 2): `GET /index/status` + `POST /index/rebuild`.
//!
//! Both are directory-keyed (`?directory=`): status returns the live
//! backend snapshot (`{ status, files, symbols }`), rebuild enqueues a
//! full rebuild and returns the snapshot taken right after enqueueing.
//! Rebuild on a disabled index is a 409 (the switch in Settings > Appearance
//! owns the on/off state through `PUT /config`).
//!
//! Both handlers program against the [`CodeIndexBackend`](bebok_core::index::CodeIndexBackend)
//! contract only — never the concrete orchestrator.

use axum::Json;
use axum::extract::{Query, State};
use axum::response::IntoResponse;

use bebok_core::error::CoreError;

use crate::error::{ApiError, err_response};
use crate::routes::common::DirectoryQuery;
use crate::state::AppState;

/// `GET /index/status?directory=` -> `{ status, files, symbols }`.
pub async fn get_index_status(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let s = instance.code_index_status_dto();
    Ok(Json(serde_json::json!({
        "status": s.status,
        "files": s.files,
        "symbols": s.symbols,
    })))
}

/// `POST /index/rebuild?directory=` -> enqueue a full rebuild.
/// 409 when the index is disabled for this instance.
pub async fn post_index_rebuild(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let Some(backend) = instance.code_index_backend() else {
        return Err(err_response(&CoreError::Other(
            "code index is not attached".to_string(),
        )));
    };
    if !backend.enabled() {
        return Err(ApiError::Conflict(
            "code index is disabled (enable it in Settings > Appearance)".to_string(),
        )
        .into_response());
    }
    backend.rebuild();
    let s = instance.code_index_status_dto();
    Ok(Json(serde_json::json!({
        "status": s.status,
        "files": s.files,
        "symbols": s.symbols,
        "rebuild": true,
    })))
}

#[cfg(test)]
mod tests {

    #[tokio::test]
    async fn index_status_roundtrip() {
        let base = std::env::temp_dir().join(format!("bebok-idxapi-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let store = bebok_core::store::InstanceStore::with_data_dir(base.join("data"));
        let dir = project.to_str().unwrap();
        let instance = store.get_or_create_instance(dir).await.unwrap();
        // Fresh instances default to disabled (no config file => default
        // `CodeIndexConfig { enabled: true }`? No: default IS enabled; the
        // orchestrator attaches and starts indexing. Status eventually flips
        // to ready; right after creation it is indexing/disabled.
        let s = instance.code_index_status();
        assert!(!s.status.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}
