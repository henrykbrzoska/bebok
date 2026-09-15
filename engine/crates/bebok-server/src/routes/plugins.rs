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

/// Lazily register a declared, enabled, installed dynamic plugin on the
/// global [`PluginHost`](bebok_core::PluginHost) if it is not already
/// registered. This is idempotent: a plugin that is already known is
/// skipped, and an undeclared / disabled / missing plugin is silently
/// left unregistered (the caller will still get a 404).
async fn ensure_plugin_registered(root: &std::path::Path, name: &str) {
    let host = bebok_core::PluginHost::global();

    // Fast path: already registered.
    if host.names().await.iter().any(|n| n == name) {
        return;
    }

    // Read the declaration file.
    let decl_path = bebok_core::plugin_decl::decl_path(root, name);
    let decl = match bebok_core::plugin_decl::read_decl(&decl_path) {
        Ok(d) => d,
        Err(_) => return, // no declaration → caller returns 404
    };
    if !decl.enabled {
        return; // disabled → caller returns 404
    }

    // Check that the slot directory exists.
    let slot_dir = bebok_core::plugin_decl::install_dir(root, name);
    if !slot_dir.is_dir() {
        return;
    }

    // Load the dynamic plugin from the slot dir.
    match bebok_core::load_dynamic_plugin(&slot_dir) {
        Ok(dyn_plugin) => {
            tracing::info!(
                "lazy-registering plugin '{name}' from {}",
                slot_dir.display()
            );
            host.register(std::sync::Arc::new(dyn_plugin)).await;
        }
        Err(e) => {
            tracing::warn!(
                "failed to load plugin '{name}' from {}: {e}",
                slot_dir.display()
            );
        }
    }
}

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

/// `GET /plugins/{name}/status?directory=` — query the status of a dynamic
/// plugin subprocess. Returns `{"ok":true,...}` from the plugin's `status`
/// action, or 404 when no plugin with that name is registered.
pub async fn plugin_status(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    ensure_plugin_registered(&instance.root, &name).await;
    let host = bebok_core::PluginHost::global();
    let input = serde_json::json!({});
    match host.invoke(&name, "status", &input).await {
        Some(resp) => Ok(Json(resp)),
        None => Err(
            ApiError::not_found(format!("no plugin registered with name '{name}'")).into_response(),
        ),
    }
}

/// `POST /plugins/{name}/{action}?directory=` — invoke an action on a
/// dynamic plugin subprocess. The request body is forwarded as the action
/// input. Returns the plugin's JSON response, or 404 when no plugin with
/// that name is registered.
pub async fn plugin_invoke(
    State(state): State<AppState>,
    Path((name, action)): Path<(String, String)>,
    Query(q): Query<DirectoryQuery>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    ensure_plugin_registered(&instance.root, &name).await;
    let host = bebok_core::PluginHost::global();
    match host.invoke(&name, &action, &body).await {
        Some(resp) => Ok(Json(resp)),
        None => Err(ApiError::not_found(format!(
            "no plugin registered with name '{name}' (or action '{action}' not handled)"
        ))
        .into_response()),
    }
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

    /// Unregistered plugin name -> 404 on both status and invoke routes.
    #[tokio::test]
    async fn plugin_status_and_invoke_404_for_unregistered() {
        let base = temp_base("unregistered");
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let dir = project.to_str().unwrap().to_string();
        let state = state_for(&base);

        let err = plugin_status(
            State(state.clone()),
            Path("no-such-plugin".to_string()),
            Query(query(&dir)),
        )
        .await
        .expect_err("unregistered plugin must 404");
        let (parts, body) = err.into_parts();
        let text = axum::body::to_bytes(body, 64 * 1024).await.unwrap();
        assert_eq!(parts.status, axum::http::StatusCode::NOT_FOUND);
        assert!(String::from_utf8_lossy(&text).contains("no-such-plugin"));

        let err = plugin_invoke(
            State(state.clone()),
            Path(("no-such-plugin".to_string(), "search".to_string())),
            Query(query(&dir)),
            Json(serde_json::json!({"query": "test"})),
        )
        .await
        .expect_err("invoke on unregistered plugin must 404");
        let (parts, body) = err.into_parts();
        let text = axum::body::to_bytes(body, 64 * 1024).await.unwrap();
        assert_eq!(parts.status, axum::http::StatusCode::NOT_FOUND);
        assert!(String::from_utf8_lossy(&text).contains("no-such-plugin"));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// DynamicPlugin roundtrip via PluginHost::invoke with a stub process.
    #[tokio::test]
    async fn dynamic_plugin_invoke_roundtrip() {
        let base = temp_base("dynamic");
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();

        // Create a stub plugin binary.
        let slot_dir = project.join(".bebok").join("plugins").join("dyn-test");
        std::fs::create_dir_all(&slot_dir).unwrap();
        let stub = slot_dir.join("dyn-stub.sh");
        std::fs::write(
            &stub,
            r#"#!/bin/sh
while IFS= read -r line; do
  action=$(echo "$line" | awk -F'"action"' '{split($2,a,"\""); print a[2]}')
  [ -z "$action" ] && action="unknown"
  printf '{"ok":true,"action":"%s","plugin":"dyn-test"}\n' "$action"
done
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Create and register a DynamicPlugin.
        let dyn_plugin = bebok_core::DynamicPlugin::new(
            "dyn-test",
            slot_dir.clone(),
            Some(format!("sh {} --plugin-server", stub.display())),
        );
        let host = bebok_core::PluginHost::global();
        host.register(std::sync::Arc::new(dyn_plugin)).await;

        // Invoke "status" via the host.
        let resp = host
            .invoke("dyn-test", "status", &serde_json::json!({}))
            .await
            .expect("expected response from dynamic plugin");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["action"], "status");
        assert_eq!(resp["plugin"], "dyn-test");

        // Invoke "search".
        let resp = host
            .invoke(
                "dyn-test",
                "search",
                &serde_json::json!({"query": "fn main"}),
            )
            .await
            .expect("expected response from dynamic plugin");
        assert_eq!(resp["action"], "search");

        // Cleanup.
        host.unregister("dyn-test").await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Lazy registration: `ensure_plugin_registered` picks up a declared,
    /// enabled, installed plugin from disk and registers it on the host so
    /// that subsequent `plugin_status` / `plugin_invoke` calls succeed
    /// instead of returning 404.
    #[tokio::test]
    async fn lazy_registration_ensures_plugin_available() {
        let base = temp_base("lazy");
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let dir = project.to_str().unwrap().to_string();
        let state = state_for(&base);
        let plugin_name = "lazy-test";

        // 1. Create declaration (enabled = true).
        let decl = bebok_core::PluginDecl {
            name: plugin_name.to_string(),
            repo: "test/repo".to_string(),
            url: "https://example.com/test/repo".to_string(),
            enabled: true,
        };
        let plugins_dir = project.join(".bebok").join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        std::fs::write(
            plugins_dir.join(format!("{plugin_name}.json")),
            serde_json::to_string_pretty(&decl).unwrap(),
        )
        .unwrap();

        // 2. Create slot dir with manifest + stub binary.
        let slot_dir = plugins_dir.join(plugin_name);
        std::fs::create_dir_all(&slot_dir).unwrap();
        let stub = slot_dir.join("stub.sh");
        std::fs::write(
            &stub,
            r#"#!/bin/sh
while IFS= read -r line; do
  action=$(echo "$line" | awk -F'"action"' '{split($2,a,"\""); print a[2]}')
  [ -z "$action" ] && action="unknown"
  printf '{"ok":true,"action":"%s","plugin":"lazy-test"}\n' "$action"
done
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let manifest = serde_json::json!({
            "name": plugin_name,
            "entrypoint": format!("sh {} --plugin-server", stub.display())
        });
        std::fs::write(
            slot_dir.join("bebok-plugin.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        // 3. Verify plugin is NOT registered yet.
        let host = bebok_core::PluginHost::global();
        assert!(
            !host.names().await.iter().any(|n| n == plugin_name),
            "plugin must not be registered before lazy ensure"
        );

        // 4. Call ensure_plugin_registered — it should discover and register.
        let root = project.clone();
        let name = plugin_name.to_string();
        ensure_plugin_registered(&root, &name).await;
        assert!(
            host.names().await.iter().any(|n| n == plugin_name),
            "plugin should now be registered after lazy ensure"
        );

        // 5. plugin_status via the handler should return ok (not 404).
        let resp = plugin_status(
            State(state.clone()),
            Path(plugin_name.to_string()),
            Query(query(&dir)),
        )
        .await
        .unwrap();
        assert_eq!(resp.0["ok"], true);
        assert_eq!(resp.0["plugin"], "lazy-test");

        // 6. plugin_invoke via the handler should also succeed.
        let resp = plugin_invoke(
            State(state.clone()),
            Path((plugin_name.to_string(), "search".to_string())),
            Query(query(&dir)),
            Json(serde_json::json!({"query": "fn main"})),
        )
        .await
        .unwrap();
        assert_eq!(resp.0["ok"], true);
        assert_eq!(resp.0["action"], "search");

        // Cleanup.
        host.unregister(plugin_name).await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Lazy registration does NOT register a disabled plugin (still 404).
    #[tokio::test]
    async fn lazy_registration_skips_disabled_plugin() {
        let base = temp_base("lazy-disabled");
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let dir = project.to_str().unwrap().to_string();
        let state = state_for(&base);
        let plugin_name = "lazy-disabled";

        // Declaration with enabled = false.
        let decl = bebok_core::PluginDecl {
            name: plugin_name.to_string(),
            repo: "test/repo".to_string(),
            url: "https://example.com/test/repo".to_string(),
            enabled: false,
        };
        let plugins_dir = project.join(".bebok").join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        std::fs::write(
            plugins_dir.join(format!("{plugin_name}.json")),
            serde_json::to_string_pretty(&decl).unwrap(),
        )
        .unwrap();

        // Slot dir exists but enabled = false — must stay 404.
        let slot_dir = plugins_dir.join(plugin_name);
        std::fs::create_dir_all(&slot_dir).unwrap();
        let manifest = serde_json::json!({"name": plugin_name});
        std::fs::write(
            slot_dir.join("bebok-plugin.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let root = project.clone();
        let name = plugin_name.to_string();
        ensure_plugin_registered(&root, &name).await;

        let host = bebok_core::PluginHost::global();
        assert!(
            !host.names().await.iter().any(|n| n == plugin_name),
            "disabled plugin must not be registered"
        );

        // Handler still returns 404.
        let err = plugin_status(
            State(state.clone()),
            Path(plugin_name.to_string()),
            Query(query(&dir)),
        )
        .await
        .expect_err("disabled plugin must 404");
        let (parts, _) = err.into_parts();
        assert_eq!(parts.status, axum::http::StatusCode::NOT_FOUND);

        let _ = std::fs::remove_dir_all(&base);
    }
}
