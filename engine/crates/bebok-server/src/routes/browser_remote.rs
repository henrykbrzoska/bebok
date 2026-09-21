//! Remote browser extension endpoints.
//!
//! Handles registration and communication with external browser extensions
//! that register with the engine to receive remote commands.
//!
//! Endpoints:
//! * POST /browser/register?port=&session_id=&directory=  -> register extension
//! * POST /browser/heartbeat?session_id=                  -> refresh last_seen
//! * POST /browser/remote/{action}?directory=&session_id=  -> forward command to extension

use std::time::SystemTime;

use axum::extract::{Json, Path, Query, State};
use axum::response::IntoResponse;
use reqwest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::ApiError;
use crate::state::{AppState, RemoteExtension};

/// Query parameters for extension registration.
#[derive(Debug, Deserialize)]
pub struct RegisterQuery {
    pub port: String,
    pub session_id: String,
    pub directory: String,
}

/// Query parameters for heartbeat.
#[derive(Debug, Deserialize)]
pub struct HeartbeatQuery {
    pub session_id: String,
}

/// Query parameters for remote command forwarding.
#[derive(Debug, Deserialize)]
pub struct RemoteQuery {
    pub session_id: String,
    pub directory: Option<String>,
}

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

/// Register a remote browser extension.
pub async fn register(
    State(state): State<AppState>,
    Query(query): Query<RegisterQuery>,
) -> Result<OkResponse, ApiError> {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|e| ApiError::internal(format!("get timestamp: {e}")))?
        .as_secs() as i64;

    let ext = RemoteExtension {
        port: query.port,
        session_id: query.session_id,
        directory: query.directory,
        last_seen: now,
    };

    state
        .remote_extensions
        .lock()
        .await
        .insert(ext.session_id.clone(), ext);

    Ok(OkResponse { ok: true })
}

/// Refresh the last_seen timestamp for a registered extension.
pub async fn heartbeat(
    State(state): State<AppState>,
    Query(query): Query<HeartbeatQuery>,
) -> Result<OkResponse, ApiError> {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|e| ApiError::internal(format!("get timestamp: {e}")))?
        .as_secs() as i64;

    let mut registry = state.remote_extensions.lock().await;
    if let Some(ext) = registry.get_mut(&query.session_id) {
        ext.last_seen = now;
        Ok(OkResponse { ok: true })
    } else {
        Err(ApiError::not_found(format!(
            "extension with session_id {} not registered",
            query.session_id
        )))
    }
}

/// Forward a command to a remote browser extension.
pub async fn remote(
    State(state): State<AppState>,
    Path(action): Path<String>,
    Query(query): Query<RemoteQuery>,
    body: Option<axum::extract::Json<Value>>,
) -> Result<axum::extract::Json<Value>, ApiError> {
    let registry = state.remote_extensions.lock().await;

    // Lookup by session_id primarily; fall back to directory if session_id not found
    let ext = registry
        .get(&query.session_id)
        .or_else(|| {
            query
                .directory
                .as_ref()
                .and_then(|dir| registry.values().find(|e| e.directory == *dir))
        })
        .cloned();

    let ext = ext.ok_or_else(|| {
        ApiError::service_unavailable(
            "no registered extension for the given session_id or directory".to_string(),
        )
    })?;

    // Build the target URL: http://127.0.0.1:{port}/{action}
    let url = format!("http://127.0.0.1:{}/{}", ext.port, action);

    let body = body.map(|Json(v)| v).unwrap_or(json!({}));

    // Forward the request with 30s timeout
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| ApiError::internal(format!("create client: {e}")))?;

    let response = client.post(&url).json(&body).send().await.map_err(|e| {
        if e.is_timeout() {
            ApiError::service_unavailable(format!("request to extension timed out: {e}"))
        } else {
            ApiError::internal(format!("forward request to extension: {e}"))
        }
    })?;

    let status = response.status();
    let response_body = response.json::<Value>().await.unwrap_or(Value::Null);

    if status.is_success() {
        Ok(axum::extract::Json(response_body))
    } else {
        Err(ApiError::internal(format!(
            "extension returned {} status",
            status.as_u16()
        )))
    }
}
