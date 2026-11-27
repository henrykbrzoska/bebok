//! Config routes: `GET /config` + `PUT /config` (resolved view + deltas).

use axum::Json;
use axum::extract::{Query, State};
use axum::response::IntoResponse;

use bebok_core::error::CoreError;
use bebok_core::{Instance, Runtimes};

use crate::error::{ApiError, err_response};
use crate::routes::common::DirectoryQuery;
use crate::state::AppState;

/// Build the full `GET /config` payload: the resolved view plus the raw JSON
/// document of each config **scope** (the storage is `bebok.db`, there are no
/// config files any more).
///
/// `scopes` carries each scope's raw JSONC text with credentials removed, so
/// the GUI's raw JSON editor can edit *actual* stored text (the resolved
/// `config` is merged and cannot be written back):
/// `scopes.global` = the `global` scope, `scopes.project` = the
/// `project:<directory>` scope — each `{ scope, exists, content }`.
fn config_response(instance: &Instance) -> serde_json::Value {
    use bebok_core::config;
    let cfg = instance.config_snapshot();
    let mut discovered = bebok_core::skills::discover(&instance.root);
    bebok_core::skills::apply_toggles(&mut discovered, Some(&cfg.skills));
    let servers = instance.mcp.status();
    let agents = instance.agent_infos();
    let tool_entries = crate::routes::tools::tool_entries(instance);

    let db = instance.config_db_path().to_path_buf();
    let scope_doc = |scope: String| {
        let exists = config::store::read_scope_at(&db, &scope)
            .map(|m| !m.is_empty())
            .unwrap_or(false);
        if !exists {
            return serde_json::json!({
                "scope": scope,
                "exists": false,
                "content": "{}",
            });
        }
        let text = bebok_core::db::open_db_at(&db)
            .ok()
            .and_then(|conn| config::export::export_scope(&conn, &scope).ok())
            .unwrap_or_else(|| "{}".to_string());
        // Do not return unparsed text: secrets may also occur in JSONC comments,
        // which the editor must not receive.
        let mut parsed = config::jsonc::parse(&text).unwrap_or_else(|_| serde_json::json!({}));
        redact_secrets(&mut parsed);
        let content = serde_json::to_string_pretty(&parsed).unwrap_or_else(|_| "{}".into());
        serde_json::json!({
            "scope": scope,
            "exists": true,
            "content": content,
        })
    };

    // The top-level `config.api_key` is a fallback for every provider that
    // needs a key (see `bebok_core::provider::build_provider`). Surface this
    // so the GUI can filter providers/models correctly even when only the
    // top-level key is set (no per-provider key or env var).
    let config_api_key = cfg.api_key.clone().filter(|s| !s.trim().is_empty());

    let mut response = serde_json::json!({
        "config": cfg,
        "providers": cfg
            .resolved_providers()
            .iter()
            .map(|spec| {
                let mut v = serde_json::to_value(spec).unwrap_or(serde_json::json!({}));
                v["has_key"] = serde_json::json!(
                    spec.has_key() || config_api_key.is_some()
                );
                v
            })
            .collect::<Vec<_>>(),
        "skills": discovered.skills,
        "tools": tool_entries,
        "mcp": servers,
        "agents": agents,
        "runtimes": Runtimes::from_config(&cfg.runtimes),
        "scopes": {
            "global": scope_doc(config::GLOBAL_SCOPE.to_string()),
            "project": scope_doc(config::project_scope(&instance.root)),
        },
    });
    redact_secrets(&mut response);
    response
}

fn secret_field(name: &str) -> bool {
    let key = name.to_ascii_lowercase().replace('-', "_");
    key == "api_key"
        || key == "x_api_key"
        || key == "authorization"
        || key == "access_token"
        || key == "refresh_token"
        || key == "password"
        || key == "secret"
        || key.ends_with("_secret")
        || key.ends_with("_token")
}

fn redact_secrets(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, field) in object {
                if secret_field(key) {
                    *field = serde_json::Value::Null;
                } else {
                    redact_secrets(field);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_secrets(item);
            }
        }
        _ => {}
    }
}

/// A null secret returned by GET /config is an unchanged placeholder on PUT.
/// Restore it from the selected layer, matching provider entries by name so a
/// reordered form cannot assign one provider's key to another.
fn restore_secrets(incoming: &mut serde_json::Value, previous: &serde_json::Value) {
    match incoming {
        serde_json::Value::Object(object) => {
            for (key, field) in object {
                let Some(old) = previous.get(key) else {
                    continue;
                };
                if secret_field(key) {
                    if field.is_null() {
                        *field = old.clone();
                    }
                } else {
                    restore_secrets(field, old);
                }
            }
        }
        serde_json::Value::Array(items) => {
            let Some(old_items) = previous.as_array() else {
                return;
            };
            for (index, item) in items.iter_mut().enumerate() {
                let old = match item.get("name").and_then(|name| name.as_str()) {
                    Some(name) => old_items
                        .iter()
                        .find(|old| old.get("name").and_then(|n| n.as_str()) == Some(name)),
                    None => old_items.get(index),
                };
                if let Some(old) = old {
                    restore_secrets(item, old);
                }
            }
        }
        _ => {}
    }
}

/// `POST /config/import-to-db?directory=` -> import everything into bebok.db:
/// all sessions of all instances, the global config
/// (`~/.config/bebok/config.json`) and the `.bebok/config.json` of every known
/// project (registry + session directories + `directory`). Idempotent: a
/// second call with unchanged files is a no-op. The response carries `steps`
/// (human-readable log), `sessions` counters and a per-project `projects` list. Project `mcp` is ignored (MCP is global-only); project `skills`
/// contributes only per-skill disables. Each file is retired to `*.bak` once
/// the DB holds its content.
///
/// Response: `{ ok, imported: {scopes, keys, secrets}, deleted_files, backups,
/// unchanged, skipped, mcp_mode, conflicts: {count, items, summary} }`.
/// This is the ONLY endpoint that touches config **files**: the engine reads
/// and writes every scope in `bebok.db`, and after the import a config file is
/// ignored (and retired to `*.bak`, or deleted with `--keep-backup` off).
pub async fn import_config_to_db(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    use bebok_core::config;

    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let current_root = instance.root.clone();
    let global_path = config::global_config_path();
    // The config DB of *this* store, so a test store never writes the real one.
    let db_path = state.store.config_db_path();

    let mut steps: Vec<String> = Vec::new();

    // Claude CLI detection is logged on every run; it only matters for summaries.
    let claude = config::import::detect_claude();
    let claude_note = claude
        .as_ref()
        .map(|p| format!("claude-cli found at {}", p.display()))
        .unwrap_or_else(|| "claude-cli not found (plain code merge)".into());
    tracing::info!("config import: {claude_note}");
    steps.push(claude_note);

    let outcome = tokio::task::spawn_blocking(move || -> Result<_, String> {
        use bebok_core::session::persist;
        let mut steps = Vec::new();
        let db_path = db_path;

        // 1. Sessions: every instance's session dirs -> `sessions`/`messages`.
        let data_root = persist::data_root();
        let sessions = persist::import_all_sessions(&data_root)?;
        let msg = format!(
            "sessions: {} scanned, {} imported, {} skipped, {} failed",
            sessions.scanned, sessions.imported, sessions.skipped, sessions.failed
        );
        tracing::info!("config import: {msg}");
        steps.push(msg);

        // 2. Every known project: registry (read before the global file is
        //    retired) + session directories + the current one.
        let mut dirs: Vec<std::path::PathBuf> = vec![current_root.clone()];
        let registry = config::projects::load_at(&db_path);
        let from_sessions = persist::session_directories(&data_root).unwrap_or_default();
        for d in registry.iter().map(|p| p.path.clone()).chain(from_sessions) {
            let path = std::path::PathBuf::from(&d);
            if path.is_dir() && !dirs.contains(&path) {
                dirs.push(path);
            }
        }
        steps.push(format!(
            "projects: {} found ({} in registry)",
            dirs.len(),
            registry.len()
        ));

        // 3. Global config (first pass) + each project's `.bebok/config.json`.
        let conn = bebok_core::db::open_db_at(&db_path)?;
        let mut merged = config::import::ImportReport::default();
        let mut projects = Vec::new();
        let mut errors = Vec::new();
        for dir in &dirs {
            let project_path = config::project_config_path(dir);
            let had_file = project_path.is_file();
            match config::import::import_to_db(&conn, dir, &global_path, &project_path) {
                Ok(r) => {
                    let status = if !had_file {
                        "no config"
                    } else if r.unchanged.iter().any(|s| s.starts_with("project:")) {
                        "unchanged"
                    } else {
                        "imported"
                    };
                    projects.push(serde_json::json!({
                        "directory": dir.to_string_lossy(),
                        "status": status,
                        "keys": r.keys,
                    }));
                    merged.scopes.extend(r.scopes);
                    merged.keys += r.keys;
                    merged.secrets += r.secrets;
                    merged.unchanged.extend(r.unchanged);
                    merged.deleted_files.extend(r.deleted_files);
                    merged.backups.extend(r.backups);
                    merged.skipped.extend(r.skipped);
                    merged.conflicts.extend(r.conflicts);
                }
                Err(e) => {
                    tracing::warn!("config import: {} failed: {e}", dir.display());
                    projects.push(serde_json::json!({
                        "directory": dir.to_string_lossy(),
                        "status": "error",
                        "error": e,
                    }));
                    errors.push(format!("{}: {e}", dir.display()));
                }
            }
        }
        steps.push(format!(
            "config: {} scope(s), {} key(s), {} secret(s) written; {} file(s) retired to .bak",
            merged.scopes.len(),
            merged.keys,
            merged.secrets,
            merged.deleted_files.len()
        ));

        // 4. The DB is the config: nothing else to switch, the runtime reads
        //    every scope from `bebok.db` and never from a file.
        steps.push("config lives in bebok.db (no file fallback)".into());
        Ok((sessions, projects, merged, errors, steps))
    })
    .await
    .map_err(|e| err_response(&CoreError::Other(e.to_string())))?
    .map_err(|e| err_response(&CoreError::Other(e)))?;
    let (sessions, projects, report, errors, more_steps) = outcome;
    steps.extend(more_steps);

    let summary = config::import::summarize_conflicts(&report.conflicts, claude.as_deref()).await;

    // Always reload: the scopes may have just been created or updated.
    state
        .store
        .reload_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;

    Ok(Json(serde_json::json!({
        "ok": errors.is_empty() && sessions.failed == 0,
        "steps": steps,
        "claude": claude.map(|p| p.to_string_lossy().into_owned()),
        "sessions": sessions,
        "projects": projects,
        "errors": errors,
        "imported": {
            "scopes": report.scopes.len(),
            "keys": report.keys,
            "secrets": report.secrets,
        },
        "scopes": report.scopes,
        "unchanged": report.unchanged,
        "deleted_files": report.deleted_files,
        "backups": report.backups,
        "skipped": report.skipped,
        "mcp_mode": "global_only",
        "conflicts": {
            "count": report.conflicts.len(),
            "items": report.conflicts,
            "summary": summary,
        },
    })))
}

/// `GET /config?directory=` -> resolved config view (config + skills + mcp + agents).
pub async fn get_config(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    Ok(Json(config_response(&instance)))
}

/// `PUT /config?directory=` -> write a config delta to the project file and
/// reload. Returns the resolved config view (same shape as `GET /config`).
pub async fn put_config(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
    Json(mut delta): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    use bebok_core::config;
    if !delta.is_object() {
        return Err(ApiError::bad_request("config delta must be a JSON object").into_response());
    }

    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;

    // Capture the previous skills/yolo so we can surface what changed.
    let old_cfg = instance.config_snapshot();

    // `?scope=global` writes the `global` scope; default (or `project`) writes
    // this directory's `project:<dir>` scope. A full-object body from the raw
    // JSON editor replaces the whole scope (keys absent from the body are
    // deleted); partial bodies merge top-level keys.
    let scope_global = q.scope.as_deref() == Some("global");
    let replace = q.replace.unwrap_or(false);
    let db = instance.config_db_path().to_path_buf();
    let scope_id = if scope_global {
        config::GLOBAL_SCOPE.to_string()
    } else {
        config::project_scope(&instance.root)
    };

    // The previous scope, read from the database (never from a file).
    let previous = config::store::read_scope_at(&db, &scope_id)
        .map(serde_json::Value::Object)
        .unwrap_or_else(|| serde_json::json!({}));
    restore_secrets(&mut delta, &previous);

    // `skills` is a toggle map: merge the incoming keys into the existing
    // section instead of replacing it, so flipping one skill off does not
    // silently re-enable every skill that was previously turned off (the
    // panel sends single-key deltas like `{"skills": {"code-index": false}}`).
    if !replace && let Some(incoming) = delta.get("skills").and_then(|s| s.as_object()) {
        let mut merged = previous
            .get("skills")
            .and_then(|s| s.as_object().cloned())
            .unwrap_or_default();
        for (k, v) in incoming {
            merged.insert(k.clone(), v.clone());
        }
        if let Some(obj) = delta.as_object_mut() {
            obj.insert("skills".to_string(), serde_json::Value::Object(merged));
        }
    }

    // `tool_config` is a per-tool map: deep-merge the incoming entries into
    // the existing section so patching one tool does not wipe the others
    // (each per-tool entry merges field-by-field; a null removes the tool).
    if !replace && let Some(incoming) = delta.get("tool_config").and_then(|s| s.as_object()) {
        let mut merged = previous
            .get("tool_config")
            .and_then(|s| s.as_object().cloned())
            .unwrap_or_default();
        for (name, value) in incoming {
            if value.is_null() {
                merged.remove(name);
                continue;
            }
            let mut entry = merged
                .get(name)
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            if let Some(patch) = value.as_object() {
                for (k, v) in patch {
                    entry.insert(k.clone(), v.clone());
                }
            } else {
                entry.insert("enabled".to_string(), value.clone());
            }
            merged.insert(name.clone(), serde_json::Value::Object(entry));
        }
        if let Some(obj) = delta.as_object_mut() {
            obj.insert("tool_config".to_string(), serde_json::Value::Object(merged));
        }
    }

    let write = if replace {
        if scope_global {
            config::write_full_global_at(&db, &delta)
        } else {
            config::write_full_project_at(&db, &instance.root, &delta)
        }
    } else if scope_global {
        config::write_global_delta_at(&db, &delta)
    } else {
        config::write_project_delta_at(&db, &instance.root, &delta)
    };
    write.map_err(|e| err_response(&CoreError::Other(e)))?;

    let instance = state
        .store
        .reload_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;

    let cfg = instance.config_snapshot();

    // Environment-change notes for the next prompt's context.
    if delta.get("skills").is_some() {
        for note in skills_diff_notes(&old_cfg.skills, &cfg.skills) {
            instance.add_context_note(note);
        }
    }
    if delta.get("verify").is_some() && old_cfg.frontend_verify() != cfg.frontend_verify() {
        instance.add_context_note(format!(
            "Frontend verification policy changed to '{}' (verify.frontend)",
            cfg.frontend_verify().as_str()
        ));
    }
    if delta.get("yolo").is_some() {
        instance.add_context_note(format!(
            "YOLO mode was {} (permission prompts {} bypassed)",
            if cfg.yolo { "enabled" } else { "disabled" },
            if cfg.yolo { "now" } else { "no longer" }
        ));
    }

    Ok(Json(config_response(&instance)))
}

/// Diff two `skills` config sections and produce "skill X enabled/disabled"
/// notes for every skill whose toggle state changed.
fn skills_diff_notes(old: &serde_json::Value, new: &serde_json::Value) -> Vec<String> {
    use std::collections::BTreeSet;
    let old_map = old.as_object();
    let new_map = new.as_object();
    let keys: BTreeSet<String> = old_map
        .into_iter()
        .chain(new_map)
        .flat_map(|m| m.keys().cloned())
        .collect();
    let mut notes = Vec::new();
    for key in keys {
        let before = old_map.and_then(|m| m.get(&key)).and_then(|v| v.as_bool());
        let after = new_map.and_then(|m| m.get(&key)).and_then(|v| v.as_bool());
        if before != after {
            notes.push(format!(
                "skill '{key}' was {} (its instructions {} in context)",
                if after == Some(true) {
                    "enabled"
                } else {
                    "disabled"
                },
                if after == Some(true) {
                    "are now"
                } else {
                    "are no longer"
                }
            ));
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::common::remove_test_dir;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn restores_redacted_provider_keys_by_name() {
        let old = serde_json::json!({"providers": [
            {"name": "first", "api_key": "first-secret"},
            {"name": "second", "api_key": "second-secret"}
        ]});
        let mut incoming = serde_json::json!({"providers": [
            {"name": "second", "api_key": null},
            {"name": "first", "api_key": null},
            {"name": "new", "api_key": null}
        ]});
        restore_secrets(&mut incoming, &old);
        assert_eq!(incoming["providers"][0]["api_key"], "second-secret");
        assert_eq!(incoming["providers"][1]["api_key"], "first-secret");
        assert!(incoming["providers"][2]["api_key"].is_null());
    }

    #[tokio::test]
    async fn config_route_never_returns_project_api_key() {
        let base = std::env::temp_dir().join(format!("bebok-config-http-{}", uuid::Uuid::new_v4()));
        let root = base.join("root");
        std::fs::create_dir_all(root.join(".bebok")).unwrap();
        std::fs::write(
            root.join(".bebok/config.json"),
            "{\n // comment-secret-443\n \"api_key\": \"root-secret-443\", \"providers\": [{\"name\":\"openai\",\"api_key\":\"provider-secret-443\"}]\n}",
        ).unwrap();
        let state = AppState {
            store: bebok_core::InstanceStore::with_data_dir(base.join("data")),
            #[cfg(not(target_os = "android"))]
            ptys: Arc::new(bebok_pty::PtyManager::new()),
            debug: Arc::new(bebok_core::DebugLog::new(base.join("debug.log"))),
            llm_trace: Arc::new(bebok_core::LlmTrace::new(2)),
            remote_extensions: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            command_queue: Arc::new(tokio::sync::Mutex::new(crate::state::CommandRegistry::new())),
            batches: Arc::new(bebok_core::BatchStore::new()),
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
        // The API router carries the capability-token layer (F0-5): present the
        // engine token so the assertions below exercise redaction, not auth.
        let auth = crate::auth::token();
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!(
            "GET /config?directory={directory} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {auth}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        for secret in [
            "root-secret-443",
            "provider-secret-443",
            "comment-secret-443",
        ] {
            assert!(!response.contains(secret), "leaked {secret}");
        }
        assert!(response.contains("has_key"));
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!(
            "GET /fs/file?directory={directory}&path=.bebok%2Fconfig.json HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {auth}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert!(!response.contains("root-secret-443"));
        server.abort();
        remove_test_dir(&base);
    }
}
