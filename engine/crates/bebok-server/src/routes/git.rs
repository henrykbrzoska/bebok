//! Git routes (WP-GIT / F6-14, F6-15): `GET /projects/{id}/git` and
//! `POST /projects/{id}/git/worktree/remove`.
//!
//! Same shape as `routes::projects`: thin handlers that resolve the project id
//! through `bebok_core::config::projects` (global config file, no `AppState`)
//! and hand the registered path to `bebok_core::git`. Both inherit the
//! capability-token layer from `routes::build_api_router`.
//!
//! Worktree removal is deliberately a separate, explicit call - it is never a
//! side effect of `DELETE /session/{id}` - and only ever touches a path that
//! `bebok_core::git::validate_worktree_removal` confirms sits below the
//! project's own `.bebok/worktrees/`.

use std::path::{Path as FsPath, PathBuf};

use axum::Json;
use axum::extract::Path;
use axum::response::IntoResponse;
use serde::Deserialize;

use bebok_core::config::projects::{self, ProjectEntry, ProjectsError};
use bebok_core::git::{self, GitError};

use crate::error::ApiError;

fn map_projects_err(e: ProjectsError) -> axum::response::Response {
    match e {
        ProjectsError::Invalid(m) => ApiError::bad_request(m),
        ProjectsError::NotFound => ApiError::not_found("unknown project"),
        ProjectsError::Io(m) => ApiError::internal(m),
    }
    .into_response()
}

/// `GitError` -> HTTP: caller mistakes are 400 (bad path/branch, not a
/// repository); a failing or missing `git` is 500 with git's own message.
pub fn map_git_err(e: GitError) -> axum::response::Response {
    match e {
        GitError::InvalidInput(m) | GitError::NotRepo(m) => ApiError::bad_request(m),
        GitError::Unavailable => ApiError::internal(e.to_string()),
        GitError::Failed(_) | GitError::Io(_) => ApiError::internal(e.to_string()),
    }
    .into_response()
}

/// `POST /projects/{id}/git/worktree/remove` body.
#[derive(Deserialize)]
pub struct RemoveWorktreeBody {
    /// Absolute worktree path, as reported by `DELETE /session/{id}`
    /// (`worktree_path`) or the session's `directory`.
    pub path: String,
}

/// `GET /projects/{id}/git` -> repository probe for the registered path.
/// A non-repo (or a host without `git`) answers `{ "is_repo": false, ... }`
/// with the other fields `null`, never an error; only an unknown id is 404.
pub async fn project_git(
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&bebok_core::config::global_config_path(), &id)?;
    let info = git::inspect(FsPath::new(&entry.path)).await;
    let mut value = serde_json::to_value(info).unwrap_or_default();
    value["project_id"] = serde_json::json!(entry.id);
    value["path"] = serde_json::json!(entry.path);
    value["worktrees_dir"] =
        serde_json::json!(git::worktrees_dir(FsPath::new(&entry.path)).to_string_lossy());
    Ok(Json(value))
}

/// `POST /projects/{id}/git/worktree/remove` -> `git worktree remove` for a
/// path strictly below `<project>/.bebok/worktrees/` (anything else is 400).
pub async fn remove_worktree(
    Path(id): Path<String>,
    Json(body): Json<RemoveWorktreeBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let removed =
        remove_worktree_in(&bebok_core::config::global_config_path(), &id, &body.path).await?;
    Ok(Json(serde_json::json!({
        "removed": true,
        "path": removed.to_string_lossy(),
    })))
}

/// Testable core of [`remove_worktree`]: explicit config path instead of the
/// global one.
pub async fn remove_worktree_in(
    config_path: &FsPath,
    id: &str,
    path: &str,
) -> Result<PathBuf, axum::response::Response> {
    let entry = resolve(config_path, id)?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request("path is required").into_response());
    }
    git::remove_worktree(FsPath::new(&entry.path), FsPath::new(trimmed))
        .await
        .map_err(map_git_err)
}

fn resolve(config_path: &FsPath, id: &str) -> Result<ProjectEntry, axum::response::Response> {
    projects::find_in(config_path, id).map_err(map_projects_err)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::StatusCode;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Same raw-socket pattern as `routes::projects`'s HTTP tests.
    async fn serve() -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<()>,
        std::path::PathBuf,
    ) {
        let base = std::env::temp_dir().join(format!("bebok-git-http-{}", uuid::Uuid::new_v4()));
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
    async fn git_routes_require_the_capability_token() {
        let (address, server, base) = serve().await;
        let get = raw(
            address,
            "GET /projects/some-id/git HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(
            get.starts_with("HTTP/1.1 401") || get.starts_with("HTTP/1.1 403"),
            "{get}"
        );
        let body = r#"{"path":"x"}"#;
        let post = raw(
            address,
            &format!(
                "POST /projects/some-id/git/worktree/remove HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        )
        .await;
        assert!(
            post.starts_with("HTTP/1.1 401") || post.starts_with("HTTP/1.1 403"),
            "{post}"
        );
        server.abort();
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn unknown_project_id_is_404_over_http() {
        let (address, server, base) = serve().await;
        let auth = crate::auth::token();
        let response = raw(
            address,
            &format!(
                "GET /projects/no-such-project/git HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {auth}\r\nConnection: close\r\n\r\n"
            ),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 404"), "{response}");
        server.abort();
        let _ = std::fs::remove_dir_all(base);
    }

    /// The removal endpoint refuses anything outside the project's own
    /// `.bebok/worktrees/` before `git` is ever invoked (no git needed).
    #[tokio::test]
    async fn worktree_removal_refuses_paths_outside_the_worktrees_dir() {
        let base = std::env::temp_dir().join(format!("bebok-git-remove-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        let other = base.join("other");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let config = base.join("config.json");
        let (entry, _) =
            bebok_core::config::projects::add_in(&config, project.to_str().unwrap(), None).unwrap();

        for bad in [
            other.to_string_lossy().to_string(),
            project.to_string_lossy().to_string(),
            bebok_core::git::worktrees_dir(&project)
                .to_string_lossy()
                .to_string(),
            "relative/path".to_string(),
            String::new(),
        ] {
            let err = super::remove_worktree_in(&config, &entry.id, &bad)
                .await
                .expect_err(&format!("must refuse {bad:?}"));
            assert_eq!(err.status(), StatusCode::BAD_REQUEST, "{bad:?}");
        }
        assert!(
            other.exists(),
            "nothing outside the worktrees dir was touched"
        );

        // Unknown project -> 404 before any path validation.
        let err = super::remove_worktree_in(&config, "nope", other.to_str().unwrap())
            .await
            .expect_err("unknown project");
        assert_eq!(err.status(), StatusCode::NOT_FOUND);

        let _ = std::fs::remove_dir_all(base);
    }
}
