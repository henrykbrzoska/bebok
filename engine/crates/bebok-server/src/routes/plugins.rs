//! Plugin declaration routes (TOR B — silnik): `GET /plugins` lists declared
//! plugins from `<project>/.bebok/plugins/*.json` plus the registered
//! in-process host names; `POST /plugins/{name}/install` creates the
//! declaration + slot dir (only `bebok-index`); `POST
//! /plugins/{name}/toggle` flips the `enabled` switch. Thin handlers: all
//! logic lives in `bebok_core::{plugin_decl, InstanceStore}`.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::error::{ApiError, err_response};
use crate::routes::common::DirectoryQuery;
use crate::state::AppState;

/// `POST /plugins/{name}/toggle` body.
#[derive(Deserialize)]
pub struct PluginToggleBody {
    pub enabled: Option<bool>,
}

/// `GET /plugins?directory=` -> declared plugins from
/// `<project>/.bebok/plugins/*.json` (`{ name, repo, url, enabled,
/// installed }`) plus the registered in-process host plugin names and the
/// exposed hook points (the previous `GET /plugins` introspection shape is
/// kept under the `host` key).
pub async fn list_plugins(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let declared = state.store.list_declared_plugins(&instance.root);
    let host = bebok_core::PluginHost::global();
    Ok(Json(serde_json::json!({
        "declared": declared,
        "plugins": host.names().await,
        "hooks": bebok_core::hook_names(),
        "attached": true,
    })))
}

/// `GET /plugins/registry` -> the installable-plugin catalogue from the
/// central registry (fetched + cached; bundled `bebok-index` fallback when
/// offline): `{ plugins: [{name, repo, url, description}], cached: bool }`.
pub async fn plugin_registry(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let data_dir = state.store.data_dir().to_path_buf();
    let registry = bebok_core::load_registry_or_fallback(&data_dir, bebok_core::REGISTRY_URL).await;
    Ok(Json(serde_json::json!({ "plugins": registry.plugins })))
}
/// `POST /plugins/{name}/install?directory=` -> clone the plugin repo
/// (latest tag) into the slot dir `<root>/.bebok/plugins/<name>/`, validate
/// its `bebok-plugin.json` manifest and create the declaration file.
/// The name must be listed in the plugin registry; anything else is a 400
/// (no arbitrary URLs, only public repos). Idempotent: keeps an existing
/// declaration.
pub async fn install_plugin(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let plugin = state
        .store
        .install_plugin(&instance, &name)
        .await
        .map_err(|e| ApiError::from_core(&e).into_response())?;
    Ok(Json(serde_json::json!({ "plugin": plugin })))
}

/// `POST /plugins/{name}/toggle?directory=` -> flip the `enabled` switch of
/// a declared plugin (body `{ enabled: bool }`; missing = flip the current
/// value). Unknown names are a 400.
pub async fn toggle_plugin(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<DirectoryQuery>,
    Json(body): Json<PluginToggleBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let enabled = match body.enabled {
        Some(v) => v,
        None => {
            let current = state
                .store
                .list_declared_plugins(&instance.root)
                .into_iter()
                .find(|p| p.name == name)
                .map(|p| p.enabled)
                .unwrap_or(false);
            !current
        }
    };
    let plugin = state
        .store
        .set_plugin_enabled(&instance, &name, enabled)
        .map_err(|e| ApiError::from_core(&e).into_response())?;
    Ok(Json(serde_json::json!({ "plugin": plugin })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn temp_base(tag: &str) -> std::path::PathBuf {
        let base =
            std::env::temp_dir().join(format!("bebok-plugins-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn state_for(base: &std::path::Path) -> AppState {
        AppState {
            store: bebok_core::InstanceStore::with_data_dir(base.join("data")),
            #[cfg(not(target_os = "android"))]
            ptys: Arc::new(bebok_pty::PtyManager::new()),
            debug: Arc::new(bebok_core::DebugLog::new(base.join("debug.log"))),
            llm_trace: Arc::new(bebok_core::LlmTrace::new(2)),
        }
    }

    fn query(dir: &str) -> DirectoryQuery {
        DirectoryQuery {
            directory: dir.to_string(),
            scope: None,
            replace: None,
        }
    }

    #[tokio::test]
    async fn list_install_toggle_roundtrip() {
        let base = temp_base("roundtrip");
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let dir = project.to_str().unwrap().to_string();
        let state = state_for(&base);

        // Empty at first; host introspection shape is kept.
        let listed = list_plugins(State(state.clone()), Query(query(&dir)))
            .await
            .unwrap();
        assert_eq!(listed["declared"].as_array().unwrap().len(), 0);
        assert!(listed.get("plugins").is_some());
        assert!(listed.get("hooks").is_some());

        // Install the known plugin: declaration + slot dir appear.
        let installed = install_plugin(
            State(state.clone()),
            Path("bebok-index".to_string()),
            Query(query(&dir)),
        )
        .await
        .unwrap();
        assert_eq!(installed["plugin"]["name"], "bebok-index");
        assert_eq!(installed["plugin"]["repo"], "henrykbrzoska/bebok-index");
        assert_eq!(installed["plugin"]["enabled"], true);
        assert_eq!(installed["plugin"]["installed"], true);
        assert!(
            project
                .join(".bebok")
                .join("plugins")
                .join("bebok-index.json")
                .is_file()
        );
        assert!(
            project
                .join(".bebok")
                .join("plugins")
                .join("bebok-index")
                .is_dir()
        );

        // Listed now with installed = true.
        let listed = list_plugins(State(state.clone()), Query(query(&dir)))
            .await
            .unwrap();
        let declared = listed["declared"].as_array().unwrap();
        assert_eq!(declared.len(), 1);
        assert_eq!(declared[0]["name"], "bebok-index");
        assert_eq!(declared[0]["installed"], true);

        // Toggle off (explicit) then on (flip).
        let off = toggle_plugin(
            State(state.clone()),
            Path("bebok-index".to_string()),
            Query(query(&dir)),
            Json(PluginToggleBody {
                enabled: Some(false),
            }),
        )
        .await
        .unwrap();
        assert_eq!(off["plugin"]["enabled"], false);
        let on = toggle_plugin(
            State(state.clone()),
            Path("bebok-index".to_string()),
            Query(query(&dir)),
            Json(PluginToggleBody { enabled: None }),
        )
        .await
        .unwrap();
        assert_eq!(on["plugin"]["enabled"], true);

        // Install is idempotent: keeps the enabled switch.
        let _ = toggle_plugin(
            State(state.clone()),
            Path("bebok-index".to_string()),
            Query(query(&dir)),
            Json(PluginToggleBody {
                enabled: Some(false),
            }),
        )
        .await
        .unwrap();
        let again = install_plugin(
            State(state.clone()),
            Path("bebok-index".to_string()),
            Query(query(&dir)),
        )
        .await
        .unwrap();
        assert_eq!(again["plugin"]["enabled"], false);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn install_rejects_unknown_plugins_and_toggle_rejects_undeclared() {
        let base = temp_base("reject");
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let dir = project.to_str().unwrap().to_string();
        let state = state_for(&base);

        let err = install_plugin(
            State(state.clone()),
            Path("evil-plugin".to_string()),
            Query(query(&dir)),
        )
        .await
        .expect_err("unknown plugin must be rejected");
        let (mut parts, body) = err.into_parts();
        let text = axum::body::to_bytes(body, 64 * 1024).await.unwrap();
        assert_eq!(parts.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(String::from_utf8_lossy(&text).contains("evil-plugin"));
        let _ = &mut parts;

        let err = toggle_plugin(
            State(state.clone()),
            Path("nope".to_string()),
            Query(query(&dir)),
            Json(PluginToggleBody {
                enabled: Some(true),
            }),
        )
        .await
        .expect_err("undeclared toggle must fail");
        let (parts, body) = err.into_parts();
        let text = axum::body::to_bytes(body, 64 * 1024).await.unwrap();
        assert_eq!(parts.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(String::from_utf8_lossy(&text).contains("nope"));

        let _ = std::fs::remove_dir_all(&base);
    }
}
