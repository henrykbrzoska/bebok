//! Tool routes (WP-CHAT4 / F7-7): `GET /tools/safety` + `PUT /tools/safety`.
//!
//! The safety category of a tool is *informational* (it colours the dots in
//! the chat transcript and the "Tool safety" table in Settings); it never
//! changes an allow/ask/deny verdict. Categories live in the `tool_safety`
//! config map (`{ "<tool name or glob>": "safe" | "caution" | "dangerous" |
//! "uncategorized" }`), global by default with a per-project override layer.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;

use bebok_core::error::CoreError;
use bebok_core::{Instance, SafetyCategory, SafetyOverrides, config, tool_safety};

use crate::error::{ApiError, err_response};
use crate::routes::common::DirectoryQuery;
use crate::state::AppState;

/// `PUT /tools/safety` body: `{ "overrides": { "<tool>": "<category>" | null } }`.
/// `null` removes the override from the written layer ("reset to default").
/// A bare map without the `overrides` wrapper is accepted too.
#[derive(Deserialize)]
pub struct SafetyBody {
    #[serde(default)]
    pub overrides: Option<BTreeMap<String, Option<String>>>,
    #[serde(flatten, default)]
    pub bare: BTreeMap<String, serde_json::Value>,
}

/// The `GET /tools/safety` payload: every tool the engine knows right now
/// plus the raw override maps of both layers (so the UI can tell a project
/// override from a global one) and the category list for selects.
fn safety_response(instance: &Instance) -> serde_json::Value {
    let cfg = instance.config_snapshot();
    let overrides = SafetyOverrides::parse(&cfg.tool_safety);
    let tools = tool_safety::list_entries(&instance.tools, &overrides);
    let uncategorized = tools
        .iter()
        .filter(|t| t.category == SafetyCategory::Uncategorized)
        .count();
    let layer = |path: &std::path::Path| {
        config::read_layer_json(path)
            .and_then(|v| v.get("tool_safety").cloned())
            .unwrap_or_else(|| serde_json::json!({}))
    };
    serde_json::json!({
        "tools": tools,
        "uncategorized": uncategorized,
        "categories": SafetyCategory::ALL.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
        "overrides": {
            "global": layer(&config::global_config_path()),
            "project": layer(&config::project_config_path(&instance.root)),
        },
    })
}

/// `GET /tools/safety?directory=` -> every known tool with its category.
pub async fn get_tool_safety(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    Ok(Json(safety_response(&instance)))
}

/// `PUT /tools/safety?directory=[&scope=global|project]` -> merge the given
/// overrides into the layer's `tool_safety` map (default: **global**, since
/// a category is a user-level judgement; `scope=project` writes the project
/// file), reload the instance and return the same payload as `GET`.
pub async fn put_tool_safety(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
    Json(body): Json<SafetyBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;

    // Normalize both accepted body shapes into `name -> Some(category) | None`.
    let mut patch: BTreeMap<String, Option<SafetyCategory>> = BTreeMap::new();
    let entries: Vec<(String, serde_json::Value)> = match body.overrides {
        Some(map) => map
            .into_iter()
            .map(|(k, v)| (k, v.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null)))
            .collect(),
        None => body.bare.into_iter().collect(),
    };
    for (name, value) in entries {
        let name = name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        let category = match value {
            serde_json::Value::Null => None,
            serde_json::Value::String(s) => match SafetyCategory::parse(&s) {
                Some(c) => Some(c),
                None => {
                    return Err(ApiError::bad_request(format!(
                        "unknown safety category {s:?} for {name:?}"
                    ))
                    .into_response());
                }
            },
            other => {
                return Err(ApiError::bad_request(format!(
                    "safety category for {name:?} must be a string or null, got {other}"
                ))
                .into_response());
            }
        };
        patch.insert(name, category);
    }

    let scope_project = q.scope.as_deref() == Some("project");
    let layer_path = if scope_project {
        config::project_config_path(&instance.root)
    } else {
        config::global_config_path()
    };
    let mut section = config::read_layer_json(&layer_path)
        .and_then(|v| v.get("tool_safety").cloned())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    for (name, category) in patch {
        match category {
            Some(c) => {
                section.insert(name, serde_json::Value::String(c.as_str().to_string()));
            }
            None => {
                section.remove(&name);
            }
        }
    }
    config::write_delta_to(
        &layer_path,
        &serde_json::json!({ "tool_safety": serde_json::Value::Object(section) }),
    )
    .map_err(|e| err_response(&CoreError::Other(e)))?;

    let instance = state
        .store
        .reload_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    Ok(Json(safety_response(&instance)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_accepts_wrapped_and_bare_maps() {
        let wrapped: SafetyBody =
            serde_json::from_str(r#"{ "overrides": { "bash": "caution", "rm": null } }"#).unwrap();
        let map = wrapped.overrides.unwrap();
        assert_eq!(map.get("bash").cloned().flatten().as_deref(), Some("caution"));
        assert_eq!(map.get("rm").cloned().flatten(), None);

        let bare: SafetyBody = serde_json::from_str(r#"{ "bash": "caution" }"#).unwrap();
        assert!(bare.overrides.is_none());
        assert_eq!(bare.bare.get("bash").and_then(|v| v.as_str()), Some("caution"));
    }
}
