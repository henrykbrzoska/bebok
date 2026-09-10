//! Config routes: `GET /config` + `PUT /config` (resolved view + deltas).

use axum::extract::{Query, State};
use axum::Json;
use axum::response::IntoResponse;

use bebok_core::error::CoreError;
use bebok_core::{Instance, Runtimes};

use crate::error::{ApiError, err_response};
use crate::routes::common::DirectoryQuery;
use crate::state::AppState;

/// Build the full `GET /config` payload: resolved view + raw config files.
/// `files` carries the untouched layer contents so the GUI can show/edit the
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
            Ok(content) => serde_json::json!({
                "exists": true,
                "path": path.to_string_lossy(),
                "content": content,
            }),
            Err(_) => serde_json::json!({
                "exists": false,
                "path": path.to_string_lossy(),
                "content": "{}",
            }),
        }
    };

    serde_json::json!({
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
    })
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
    Json(delta): Json<serde_json::Value>,
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
                if after == Some(true) { "enabled" } else { "disabled" },
                if after == Some(true) { "are now" } else { "are no longer" }
            ));
        }
    }
    notes
}
