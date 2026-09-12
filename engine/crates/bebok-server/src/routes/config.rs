//! Config routes: `GET /config` + `PUT /config` (resolved view + deltas).

use axum::Json;
use axum::extract::{Query, State};
use axum::response::IntoResponse;

use bebok_core::error::CoreError;
use bebok_core::{Instance, Runtimes};

use crate::error::{ApiError, err_response};
use crate::routes::common::DirectoryQuery;
use crate::state::AppState;

/// Build the full `GET /config` payload: resolved view + redacted config files.
/// `files` carries layer contents with credentials removed so the GUI can edit
/// *actual* `config.json` text (resolved `config` is merged and unpatchable):
/// `files.global` = `~/.config/bebok/config.json`, `files.project` =
/// `<directory>/.bebok/config.json` (each `{ exists, path, content }`).
fn config_response(instance: &Instance) -> serde_json::Value {
    use bebok_core::config;
    let cfg = instance.config_snapshot();
    let mut discovered = bebok_core::skills::discover(&instance.root);
    bebok_core::skills::apply_toggles(&mut discovered, Some(&cfg.skills));
    let servers = instance.mcp.status();
    let agents = instance.agent_infos();

    let global_path = config::global_config_path();
    let project_path = config::project_config_path(&instance.root);
    let layer = |path: &std::path::Path| {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                // Do not return unparsed text: secrets may also occur in JSONC
                // comments, which the editor must not receive.
                let mut parsed =
                    config::jsonc::parse(&content).unwrap_or_else(|_| serde_json::json!({}));
                redact_secrets(&mut parsed);
                let content = serde_json::to_string_pretty(&parsed).unwrap_or_else(|_| "{}".into());
                serde_json::json!({
                    "exists": true,
                    "path": path.to_string_lossy(),
                    "content": content,
                })
            }
            Err(_) => serde_json::json!({
                "exists": false,
                "path": path.to_string_lossy(),
                "content": "{}",
            }),
        }
    };

    let mut response = serde_json::json!({
        "config": cfg,
        "providers": cfg
            .resolved_providers()
            .iter()
            .map(|spec| {
                let mut v = serde_json::to_value(spec).unwrap_or(serde_json::json!({}));
                v["has_key"] = serde_json::json!(spec.has_key());
                v
            })
            .collect::<Vec<_>>(),
        "skills": discovered.skills,
        "mcp": servers,
        "agents": agents,
        "runtimes": Runtimes::from_config(&cfg.runtimes),
        "files": {
            "global": layer(&global_path),
            "project": layer(&project_path),
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

    // `?scope=global` writes to `~/.config/bebok/config.json`; default (or
    // `project`) writes to `<directory>/.bebok/config.json`. A full-object
    // body from the raw JSON editor replaces the whole file (keys absent from
    // the body are deleted); partial bodies merge top-level keys.
    let scope_global = q.scope.as_deref() == Some("global");
    let replace = q.replace.unwrap_or(false);
    let layer_path = if scope_global {
        config::global_config_path()
    } else {
        config::project_config_path(&instance.root)
    };
    if let Ok(raw) = std::fs::read_to_string(&layer_path) {
        let previous = config::jsonc::parse(&raw)
            .map_err(|e| ApiError::bad_request(format!("invalid config: {e}")).into_response())?;
        restore_secrets(&mut delta, &previous);
    }
    if replace {
        let write = if scope_global {
            config::write_full_global(&delta)
        } else {
            config::write_full_project(&instance.root, &delta)
        };
        write.map_err(|e| err_response(&CoreError::Other(e)))?;
    } else if scope_global {
        config::write_global_delta(&delta).map_err(|e| err_response(&CoreError::Other(e)))?;
    } else {
        config::write_project_delta(&instance.root, &delta)
            .map_err(|e| err_response(&CoreError::Other(e)))?;
    }

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
        .chain(new_map.into_iter())
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
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!(
            "GET /config?directory={directory} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
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
            "GET /fs/file?directory={directory}&path=.bebok%2Fconfig.json HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert!(!response.contains("root-secret-443"));
        server.abort();
        std::fs::remove_dir_all(base).unwrap();
    }
}
