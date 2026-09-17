//! Plugin declaration routes (TOR B — silnik): `GET /plugins` lists declared
//! plugins from `<project>/.bebok/plugins/*.json` plus the registered
//! in-process host names; `POST /plugins/{name}/install` creates the
//! declaration + slot dir (for any name listed in the plugin registry —
//! the bundled offline fallback only contains `bebok-index`); `POST
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
///
/// Per-project limitation: the global host is keyed by plugin name only,
/// not by `(root, name)` — the host API exposes just `names()`, so there
/// is no way to compare the registered plugin's `slot_dir`
/// ([`DynamicPlugin::slot_dir`](bebok_core::DynamicPlugin::slot_dir))
/// against `root` here. Two projects using the same plugin name therefore
/// share one registration (first one wins).
/// TODO: key registrations per `(root, name)` (or store the owning root
/// alongside the plugin) so per-project enable/disable can't leak across
/// projects; until then callers must not assume the registered slot
/// belongs to their project.
async fn ensure_plugin_registered(root: &std::path::Path, name: &str) {
    let host = bebok_core::PluginHost::global();

    // Read the declaration file FIRST — a disabled or missing declaration
    // must block registration even when the plugin is already present on
    // the host (cross-project safety).
    let decl_path = bebok_core::plugin_decl::decl_path(root, name);
    let decl = match bebok_core::plugin_decl::read_decl(&decl_path) {
        Ok(d) => d,
        Err(_) => return, // no declaration → caller returns 404
    };
    if !decl.enabled {
        return; // disabled → caller returns 404
    }

    // Fast path: already registered.
    if host.names().await.iter().any(|n| n == name) {
        return;
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
/// offline): `{ plugins: [{name, repo, url, description}], cached: bool }`
/// (`cached` is true when served from the fresh on-disk cache, false when
/// fetched from the network, read from a stale cache, or bundled).
pub async fn plugin_registry(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let data_dir = state.store.data_dir().to_path_buf();
    let cached = bebok_core::plugin_registry::cache_is_fresh(&data_dir);
    let registry = bebok_core::load_registry_or_fallback(&data_dir, bebok_core::REGISTRY_URL).await;
    Ok(Json(
        serde_json::json!({ "plugins": registry.plugins, "cached": cached }),
    ))
}

/// `POST /plugins/{name}/install?directory=` -> clone the plugin repo
/// (latest tag) into the slot dir `<root>/.bebok/plugins/<name>/`, validate
/// its `bebok-plugin.json` manifest and create the declaration file.
/// The name must be listed in the plugin registry; anything else is a 400
/// (no arbitrary URLs, only public repos). Any registry-listed plugin is
/// accepted — the registry is just a catalogue (currently its only entry
/// is `bebok-index`, and the offline bundled fallback contains only it).
/// Idempotent: keeps an existing declaration.
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

/// `POST /plugins/{name}/update?directory=` -> remove the existing slot and
/// re-install from the registry entry. Idempotent. Fails with 409 when the
/// plugin's subprocess is running (responds to `status`), 404 when
/// no declaration exists, and 503 when the slot exists but the binary is
/// missing.
pub async fn update_plugin(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;

    // 404: no declaration.
    let decl_path = bebok_core::plugin_decl::decl_path(&instance.root, &name);
    if !decl_path.is_file() {
        return Err(
            ApiError::not_found(format!("plugin '{name}' is not declared")).into_response(),
        );
    }

    // 409: plugin subprocess is running.
    let host = bebok_core::PluginHost::global();
    if host.names().await.iter().any(|n| n == &name) {
        // Try to invoke "status" to see if the plugin is alive; if it
        // responds, it's running — reject update.
        if host
            .invoke(&name, "status", &serde_json::json!({}))
            .await
            .is_some()
        {
            return Err(ApiError::conflict(format!(
                "plugin '{name}' is running — stop it before updating"
            ))
            .into_response());
        }
    }

    // 503: slot is a dir but no binary resolves for this platform.
    // Shared logic with the plugin listing (`plugin_decl::slot_state`):
    // a missing slot manifest, a manifest without an entrypoint, or a
    // missing slot binary all mean `binary == "missing"` (per the
    // documented `DeclaredPlugin.binary` contract).
    let slot = bebok_core::plugin_decl::install_dir(&instance.root, &name);
    if slot.is_dir() {
        let (_, binary) = bebok_core::plugin_decl::slot_state(&instance.root, &name, true);
        if binary == "missing" {
            // Machine-readable code embedded in the text body (ApiError
            // serializes as text): the client maps `binary_missing`
            // onto `settings.pluginBinaryMissing`.
            let body = serde_json::json!({
                "error": "binary_missing",
                "message": format!(
                    "plugin '{name}' binary is missing — run Update in Settings"
                ),
            });
            return Err(ApiError::service_unavailable(body.to_string()).into_response());
        }
    }

    // Unregister the plugin from the host before re-installing.
    host.unregister(&name).await;

    let plugin = state
        .store
        .update_plugin(&instance, &name)
        .await
        .map_err(|e| ApiError::from_core(&e).into_response())?;

    // Re-register after install.
    ensure_plugin_registered(&instance.root, &name).await;

    Ok(Json(serde_json::json!({ "plugin": plugin })))
}

/// `POST /plugins/{name}/toggle?directory=` -> flip the `enabled` switch of
/// a declared plugin (body `{ enabled: bool }`; missing = flip the current
/// value). Unknown names are a 400. Disabling a plugin also unregisters it
/// from the global [`PluginHost`] so that backend tools stop immediately.
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

    // When disabling, immediately unregister from the global host so that
    // backend tools (code_index_*, plugin invoke, etc.) stop working for
    // this plugin without waiting for a restart.
    // NOTE: the global host is keyed by name only (see the per-project
    // TODO on `ensure_plugin_registered`) — another project may still be
    // using this plugin, but we unregister anyway to fail closed locally.
    // TODO: keep per-(root, name) registrations so toggle OFF only affects
    // this project.
    if !enabled {
        tracing::warn!(
            "unregistering plugin '{name}' globally on toggle OFF for {}",
            instance.root.display()
        );
        bebok_core::PluginHost::global().unregister(&name).await;
    } else {
        // Toggle ON: the plugin may have been unregistered by a previous
        // toggle OFF — register it back (no-op when already registered).
        ensure_plugin_registered(&instance.root, &name).await;
    }

    Ok(Json(serde_json::json!({ "plugin": plugin })))
}

/// `GET /plugins/{name}/status?directory=` — query the status of a dynamic
/// plugin subprocess. The plugin receives `{"directory": <project dir>}` as
/// the input. Returns `{"ok":true,...}` from the plugin's `status` action,
/// or 404 when no plugin with that name is registered or the plugin is
/// disabled for this project.
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

    // Cross-project safety: a disabled declaration in THIS project blocks
    // access even if the plugin is registered globally (for another project).
    if bebok_core::plugin_decl::is_disabled(&instance.root, &name) {
        return Err(
            ApiError::not_found(format!("no plugin registered with name '{name}'")).into_response(),
        );
    }

    ensure_plugin_registered(&instance.root, &name).await;
    let host = bebok_core::PluginHost::global();
    // Normalised project root (the instance root), not the raw
    // `?directory=` query value (which may be relative / unnormalised).
    let input = serde_json::json!({ "directory": instance.root.to_string_lossy() });
    match host.invoke(&name, "status", &input).await {
        Some(resp) => Ok(Json(resp)),
        None => Err(
            ApiError::not_found(format!("no plugin registered with name '{name}'")).into_response(),
        ),
    }
}

/// `POST /plugins/{name}/{action}?directory=` — invoke an action on a
/// dynamic plugin subprocess. The request body is forwarded as the action
/// input; when the body is a JSON object without a `directory` field, the
/// request's project directory is added (an explicitly provided `directory`
/// is never overwritten). Returns the plugin's JSON response, or 404 when
/// no plugin with that name is registered or the plugin is disabled for
/// this project.
///
/// NOTE: `install`, `update`, `toggle` and `status` are reserved action
/// names (they are dedicated routes) — invoking them through this route
/// returns a 400. Axum matches `/plugins/{name}/update` against the
/// dedicated `update_plugin` route first, so this guard only fires for
/// names where the dedicated routes don't match (defence in depth), but it
/// must stay so the generic `{action}` route never shadows them.
pub async fn plugin_invoke(
    State(state): State<AppState>,
    Path((name, action)): Path<(String, String)>,
    Query(q): Query<DirectoryQuery>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    if matches!(action.as_str(), "install" | "update" | "toggle" | "status") {
        return Err(ApiError::bad_request(format!(
            "action '{action}' is reserved — use POST /plugins/{{name}}/{action} instead"
        ))
        .into_response());
    }
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;

    // Cross-project safety: a disabled declaration in THIS project blocks
    // access even if the plugin is registered globally (for another project).
    if bebok_core::plugin_decl::is_disabled(&instance.root, &name) {
        return Err(ApiError::not_found(format!(
            "no plugin registered with name '{name}' (or action '{action}' not handled)"
        ))
        .into_response());
    }

    ensure_plugin_registered(&instance.root, &name).await;
    let host = bebok_core::PluginHost::global();
    // Ensure the plugin can locate the project: default `directory` to the
    // request's project dir unless the caller supplied one explicitly.
    let input = match body.as_object() {
        Some(obj) if !obj.contains_key("directory") => {
            let mut obj = obj.clone();
            obj.insert("directory".to_string(), serde_json::json!(q.directory));
            serde_json::Value::Object(obj)
        }
        _ => body,
    };
    match host.invoke(&name, &action, &input).await {
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

    /// Write a portable echo-stub plugin script into `slot_dir` and return
    /// the manifest entrypoint command for it. Unix uses `sh` + awk (both
    /// guaranteed present); Windows uses `powershell` (ships with the OS —
    /// neither `sh` nor `awk` exists on a stock Windows runner). The `.ps1`
    /// path is quoted: `%TEMP%` often contains spaces.
    #[cfg(windows)]
    fn write_stub(slot_dir: &std::path::Path, plugin: &str) -> String {
        let stub = slot_dir.join("stub.ps1");
        std::fs::write(
            &stub,
            format!(
                "while (($line = [Console]::In.ReadLine()) -ne $null) {{\r\n  if ($line -match '\"action\"\\s*:\\s*\"([^\"]+)\"') {{ $action = $Matches[1] }} else {{ $action = \"unknown\" }}\r\n  '{{\"ok\":true,\"action\":\"' + $action + '\",\"plugin\":\"{plugin}\"}}'\r\n}}\r\n",
            ),
        )
        .unwrap();
        let ps = if std::process::Command::new("where")
            .arg("pwsh")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            "pwsh"
        } else {
            "powershell"
        };
        format!(
            "{ps} -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"{}\" --plugin-server",
            stub.display()
        )
    }

    #[cfg(not(windows))]
    fn write_stub(slot_dir: &std::path::Path, plugin: &str) -> String {
        let stub = slot_dir.join("stub.sh");
        std::fs::write(
            &stub,
            format!(
                r#"#!/bin/sh
while IFS= read -r line; do
  action=$(echo "$line" | awk -F'"action"' '{{split($2,a,"\""); print a[2]}}')
  [ -z "$action" ] && action="unknown"
  printf '{{"ok":true,"action":"%s","plugin":"{plugin}"}}\n' "$action"
done
"#,
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        format!("sh {} --plugin-server", stub.display())
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
    /// Reserved action names (`install`/`update`/`toggle`/`status`) through
    /// the generic `{action}` route -> 400 (they have dedicated routes).
    #[tokio::test]
    async fn plugin_invoke_rejects_reserved_actions() {
        let base = temp_base("reserved");
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let dir = project.to_str().unwrap().to_string();
        let state = state_for(&base);

        for action in ["install", "update", "toggle", "status"] {
            let err = plugin_invoke(
                State(state.clone()),
                Path(("bebok-index".to_string(), action.to_string())),
                Query(query(&dir)),
                Json(serde_json::json!({})),
            )
            .await
            .expect_err(&format!("reserved action '{action}' must 400"));
            let (parts, body) = err.into_parts();
            let text = axum::body::to_bytes(body, 64 * 1024).await.unwrap();
            assert_eq!(parts.status, axum::http::StatusCode::BAD_REQUEST);
            let text = String::from_utf8_lossy(&text);
            assert!(text.contains("reserved"), "body: {text}");
            assert!(text.contains(action), "body: {text}");
        }

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
        let entrypoint = write_stub(&slot_dir, "dyn-test");

        // Create and register a DynamicPlugin.
        let dyn_plugin =
            bebok_core::DynamicPlugin::new("dyn-test", slot_dir.clone(), Some(entrypoint));
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
            asset_url: None,
            asset_sha256: None,
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
        let entrypoint = write_stub(&slot_dir, plugin_name);
        let manifest = serde_json::json!({
            "name": plugin_name,
            "entrypoint": entrypoint
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
            asset_url: None,
            asset_sha256: None,
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

    /// Toggle-off unregisters a previously registered plugin from the host.
    #[tokio::test]
    async fn toggle_off_unregisters_plugin_from_host() {
        let base = temp_base("toggle-unreg");
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let dir = project.to_str().unwrap().to_string();
        let state = state_for(&base);
        let plugin_name = "toggle-unreg";

        // 1. Create declaration (enabled = true).
        let decl = bebok_core::PluginDecl {
            name: plugin_name.to_string(),
            repo: "test/repo".to_string(),
            url: "https://example.com/test/repo".to_string(),
            enabled: true,
            asset_url: None,
            asset_sha256: None,
        };
        let plugins_dir = project.join(".bebok").join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        std::fs::write(
            plugins_dir.join(format!("{plugin_name}.json")),
            serde_json::to_string_pretty(&decl).unwrap(),
        )
        .unwrap();

        // 2. Manually register a stub plugin on the host (DynamicPlugin
        // construction is lazy — no process is spawned until first invoke).
        let slot_dir = plugins_dir.join(plugin_name);
        std::fs::create_dir_all(&slot_dir).unwrap();
        let dyn_plugin = bebok_core::DynamicPlugin::new(plugin_name, slot_dir, None);
        let host = bebok_core::PluginHost::global();
        host.register(Arc::new(dyn_plugin)).await;
        assert!(
            host.names().await.iter().any(|n| n == plugin_name),
            "stub must be registered before toggle"
        );

        // 3. Toggle off via the handler.
        let resp = toggle_plugin(
            State(state.clone()),
            Path(plugin_name.to_string()),
            Query(query(&dir)),
            Json(PluginToggleBody {
                enabled: Some(false),
            }),
        )
        .await
        .unwrap();
        assert_eq!(resp["plugin"]["enabled"], false);

        // 4. Plugin must no longer be on the host.
        assert!(
            !host.names().await.iter().any(|n| n == plugin_name),
            "toggle-off must unregister the plugin from the host"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Disabled plugin returns 404 on status/invoke even when registered on
    /// the host for a different project (cross-project isolation).
    #[tokio::test]
    async fn disabled_plugin_status_invoke_404_cross_project() {
        let base = temp_base("cross-project");
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let dir = project.to_str().unwrap().to_string();
        let state = state_for(&base);
        let plugin_name = "cross-proj";

        // 1. Declaration with enabled = false.
        let decl = bebok_core::PluginDecl {
            name: plugin_name.to_string(),
            repo: "test/repo".to_string(),
            url: "https://example.com/test/repo".to_string(),
            enabled: false,
            asset_url: None,
            asset_sha256: None,
        };
        let plugins_dir = project.join(".bebok").join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        std::fs::write(
            plugins_dir.join(format!("{plugin_name}.json")),
            serde_json::to_string_pretty(&decl).unwrap(),
        )
        .unwrap();

        // 2. Force-register the plugin on the global host (simulates
        //    another project having enabled it). DynamicPlugin construction
        //    is lazy — no process is spawned until first invoke.
        let slot_dir = plugins_dir.join(plugin_name);
        std::fs::create_dir_all(&slot_dir).unwrap();
        let dyn_plugin = bebok_core::DynamicPlugin::new(plugin_name, slot_dir, None);
        let host = bebok_core::PluginHost::global();
        host.register(Arc::new(dyn_plugin)).await;
        assert!(host.names().await.iter().any(|n| n == plugin_name));

        // 3. status must 404 for THIS project (disabled).
        let err = plugin_status(
            State(state.clone()),
            Path(plugin_name.to_string()),
            Query(query(&dir)),
        )
        .await
        .expect_err("disabled plugin must 404 even if registered globally");
        let (parts, _) = err.into_parts();
        assert_eq!(parts.status, axum::http::StatusCode::NOT_FOUND);

        // 4. invoke must also 404 for THIS project.
        let err = plugin_invoke(
            State(state.clone()),
            Path((plugin_name.to_string(), "search".to_string())),
            Query(query(&dir)),
            Json(serde_json::json!({"query": "test"})),
        )
        .await
        .expect_err("disabled plugin invoke must 404");
        let (parts, _) = err.into_parts();
        assert_eq!(parts.status, axum::http::StatusCode::NOT_FOUND);

        // Cleanup.
        host.unregister(plugin_name).await;
        let _ = std::fs::remove_dir_all(&base);
    }
}
