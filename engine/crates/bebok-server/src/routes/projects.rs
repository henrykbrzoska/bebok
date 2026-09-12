//! Projects registry routes (F5-1): `GET/POST /projects`,
//! `PATCH/DELETE /projects/{id}`, `POST /projects/{id}/open`.
//!
//! The registry itself lives in `bebok_core::config::projects` (the
//! `"projects"` key of the global config file). These handlers are thin:
//! extract -> registry -> JSON, mapping `ProjectsError` onto the project's
//! usual status codes (400 invalid input, 404 unknown id, 500 write failure).
//!
//! All of them inherit the capability-token layer applied around the whole
//! router in `routes::build_api_router`, so no per-handler auth check is
//! needed.

use axum::Json;
use axum::extract::Path;
use axum::response::IntoResponse;
use serde::Deserialize;

use bebok_core::config::projects::{self, ProjectPatch, ProjectsError};

use crate::error::ApiError;

fn map_err(e: ProjectsError) -> axum::response::Response {
    match e {
        ProjectsError::Invalid(m) => ApiError::bad_request(m),
        ProjectsError::NotFound => ApiError::not_found("unknown project"),
        ProjectsError::Io(m) => ApiError::internal(m),
    }
    .into_response()
}

/// `POST /projects` body.
#[derive(Deserialize)]
pub struct AddProjectBody {
    pub path: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// `GET /projects` -> the registry in display order (pinned, then most
/// recently opened, then name).
pub async fn list_projects() -> Json<serde_json::Value> {
    let projects = projects::list_from(&bebok_core::config::global_config_path());
    Json(serde_json::json!({ "projects": projects }))
}

/// `POST /projects` -> register a directory (or return the existing entry when
/// the path normalises onto one already registered).
pub async fn add_project(
    Json(body): Json<AddProjectBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let (entry, _created) = projects::add_in(
        &bebok_core::config::global_config_path(),
        &body.path,
        body.name,
    )
    .map_err(map_err)?;
    // 200 in both cases: a duplicate add is idempotent, not an error.
    Ok(Json(serde_json::to_value(entry).unwrap_or_default()))
}

/// `PATCH /projects/{id}` -> partial update (name and/or pinned).
pub async fn patch_project(
    Path(id): Path<String>,
    Json(patch): Json<ProjectPatch>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = projects::patch_in(&bebok_core::config::global_config_path(), &id, &patch)
        .map_err(map_err)?;
    Ok(Json(serde_json::to_value(entry).unwrap_or_default()))
}

/// `DELETE /projects/{id}` -> forget the project. Never touches disk.
pub async fn delete_project(
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    projects::remove_in(&bebok_core::config::global_config_path(), &id).map_err(map_err)?;
    Ok(Json(serde_json::json!({ "removed": true, "id": id })))
}

/// `POST /projects/{id}/open` -> stamp `last_opened_at` and return the entry
/// with its normalised path (what the client switches the active directory to).
pub async fn open_project(
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = projects::mark_opened_in(&bebok_core::config::global_config_path(), &id)
        .map_err(map_err)?;
    Ok(Json(serde_json::to_value(entry).unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Same raw-socket pattern as `routes::fs`'s HTTP test: build the real
    /// router (capability-token layer included) and talk to it over a socket.
    async fn serve() -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<()>,
        std::path::PathBuf,
    ) {
        let base =
            std::env::temp_dir().join(format!("bebok-projects-http-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
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
    async fn projects_route_requires_the_capability_token() {
        let (address, server, base) = serve().await;
        let response = raw(
            address,
            "GET /projects HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
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
    async fn projects_route_rejects_a_non_directory_path() {
        let (address, server, base) = serve().await;
        let auth = crate::auth::token();
        let file = base.join("not-a-dir.txt");
        std::fs::write(&file, "x").unwrap();
        let body = serde_json::json!({ "path": file.to_string_lossy() }).to_string();
        let response = raw(
            address,
            &format!(
                "POST /projects HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        server.abort();
        let _ = std::fs::remove_dir_all(base);
    }
}
