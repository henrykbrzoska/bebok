//! MCP routes: `GET /mcp` + `POST /mcp/{name}/toggle`.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;

use bebok_core::config;
use bebok_core::error::CoreError;

use crate::error::{ApiError, err_response};
use crate::routes::common::DirectoryQuery;
use crate::state::AppState;

/// `POST /mcp/{name}/toggle` body.
#[derive(Deserialize)]
pub struct ToggleBody {
    pub enabled: Option<bool>,
}

/// `GET /mcp?directory=` -> MCP servers + status.
pub async fn list_mcp(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let servers = instance.mcp.status();
    Ok(Json(serde_json::json!({ "servers": servers })))
}

/// `POST /mcp/{name}/toggle` -> enable/disable one MCP server.
///
/// Toggle = config edit: writes `mcp.<name>.enabled` to the project config,
/// re-syncs the MCP manager (connect/disconnect) and returns the new status.
pub async fn toggle_mcp(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<DirectoryQuery>,
    Json(body): Json<ToggleBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;

    // Determine the target enabled state (explicit value, else flip).
    let enabled = match body.enabled {
        Some(v) => v,
        None => {
            let current = instance
                .mcp
                .status()
                .into_iter()
                .find(|s| s.name == name)
                .map(|s| s.enabled)
                .unwrap_or(false);
            !current
        }
    };

    // Write the toggle to the project config (`mcp.<name>.enabled`).
    let mut cfg = instance.config_snapshot();
    let mut mcp = cfg.mcp.clone();
    let mcp_map = mcp
        .as_object_mut()
        .ok_or_else(|| err_response(&CoreError::Other("mcp config is not an object".into())))?;
    match mcp_map.get_mut(&name).and_then(|v| v.as_object_mut()) {
        Some(server) => {
            server.insert("enabled".to_string(), serde_json::json!(enabled));
        }
        None => {
            return Err(ApiError::not_found(format!("unknown mcp server '{name}'")).into_response());
        }
    }
    cfg.mcp = mcp;

    config::write_project_delta(&instance.root, &serde_json::json!({ "mcp": cfg.mcp }))
        .map_err(|e| err_response(&CoreError::Other(e)))?;

    // Surface the change to the model in the next prompt's context.
    instance.add_context_note(format!(
        "MCP server '{name}' was {} (its tools are {} available)",
        if enabled { "enabled" } else { "disabled" },
        if enabled { "now" } else { "no longer" }
    ));

    // Reload: re-reads config, re-syncs MCP, emits `config.changed`.
    let instance = state
        .store
        .reload_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let servers = instance.mcp.status();
    Ok(Json(
        serde_json::json!({ "name": name, "enabled": enabled, "servers": servers }),
    ))
}
