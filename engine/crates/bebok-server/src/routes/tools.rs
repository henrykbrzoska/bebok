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
use bebok_core::tool_safety::source_label;
use bebok_core::{Instance, SafetyCategory, SafetyOverrides, config, tool_config, tool_safety};

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
    let db = instance.config_db_path();
    let layer = |scope: String| {
        config::store::read_scope_at(db, &scope)
            .and_then(|v| v.get("tool_safety").cloned())
            .unwrap_or_else(|| serde_json::json!({}))
    };
    serde_json::json!({
        "tools": tools,
        "uncategorized": uncategorized,
        "categories": SafetyCategory::ALL.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
        "overrides": {
            "global": layer(config::GLOBAL_SCOPE.to_string()),
            "project": layer(config::project_scope(&instance.root)),
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
            .map(|(k, v)| {
                (
                    k,
                    v.map(serde_json::Value::String)
                        .unwrap_or(serde_json::Value::Null),
                )
            })
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
    let db = instance.config_db_path().to_path_buf();
    let scope_id = if scope_project {
        config::project_scope(&instance.root)
    } else {
        config::GLOBAL_SCOPE.to_string()
    };
    let mut section = config::store::read_scope_at(&db, &scope_id)
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
    config::store::write_delta_at(
        &db,
        &scope_id,
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

/// One row of `GET /tools/list` / the `PUT /tools/config` response: a tool
/// with its effective enable state, description (custom or built-in) and the
/// metadata the Settings "Tools" tab needs. Also embedded in `GET /config`
/// under `tools`.
fn tool_list_response(instance: &Instance) -> serde_json::Value {
    let db = instance.config_db_path().to_path_buf();
    let layer = |scope: String| {
        config::store::read_scope_at(&db, &scope)
            .and_then(|v| v.get("tool_config").cloned())
            .unwrap_or_else(|| serde_json::json!({}))
    };

    serde_json::json!({
        "tools": tool_entries(instance),
        "categories": SafetyCategory::ALL.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
        "overrides": {
            "global": layer(config::GLOBAL_SCOPE.to_string()),
            "project": layer(config::project_scope(&instance.root)),
        },
    })
}

/// Every known tool (including disabled ones) as `{name, source, enabled,
/// description, default_description, has_description_override, is_read_only,
/// safety}`. Used by `GET /tools/list` and embedded in `GET /config`.
pub fn tool_entries(instance: &Instance) -> Vec<serde_json::Value> {
    let cfg = instance.config_snapshot();
    let overrides = SafetyOverrides::parse(&cfg.tool_safety);
    let tool_cfg = tool_config::ToolConfigMap::parse(&cfg.tool_config);
    instance
        .tools
        .list_all_with_source()
        .into_iter()
        .map(|(source, tool)| {
            let name = tool.name().to_string();
            let enabled = !instance.tools.is_disabled(&name);
            let default_description = instance
                .tools
                .default_description(&name)
                .unwrap_or_default();
            serde_json::json!({
                "name": name,
                "source": source_label(source, tool.name()),
                "enabled": enabled,
                "description": tool.description(),
                "default_description": default_description,
                "has_description_override": tool_cfg.custom_description(&name).is_some(),
                "is_read_only": tool.is_read_only(),
                "safety": tool_safety::categorize(tool.as_ref(), &overrides).as_str(),
            })
        })
        .collect()
}

/// `GET /tools/list?directory=` -> every known tool (including disabled ones)
/// with its effective `enabled` state, description and safety category.
pub async fn get_tool_list(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    Ok(Json(tool_list_response(&instance)))
}

/// `PUT /tools/config` body: `{ "tools": { "<name>": { "enabled": bool,
/// "description": string } | null } }`. `null` removes the per-tool entry
/// ("reset to default"). The bare map without the `tools` wrapper and the
/// `overrides` key are accepted too.
#[derive(Deserialize)]
pub struct ToolConfigBody {
    #[serde(default)]
    pub tools: Option<BTreeMap<String, Option<serde_json::Value>>>,
    #[serde(default)]
    pub overrides: Option<BTreeMap<String, Option<serde_json::Value>>>,
    #[serde(flatten, default)]
    pub bare: BTreeMap<String, serde_json::Value>,
}

/// `PUT /tools/config?directory=[&scope=global|project]` -> deep-merge the
/// given per-tool entries into the layer's `tool_config` map (default:
/// **global**), reload the instance and return the same payload as
/// `GET /tools/list`. A per-tool entry is merged field-by-field, so a patch
/// carrying only `description` keeps the stored `enabled` and vice versa.
pub async fn put_tool_config(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
    Json(body): Json<ToolConfigBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;

    // Normalize the accepted body shapes into `name -> Some(patch) | None`.
    let entries: Vec<(String, serde_json::Value)> = if let Some(map) = body.tools {
        map.into_iter()
            .map(|(k, v)| (k, v.unwrap_or(serde_json::Value::Null)))
            .collect()
    } else if let Some(map) = body.overrides {
        map.into_iter()
            .map(|(k, v)| (k, v.unwrap_or(serde_json::Value::Null)))
            .collect()
    } else {
        body.bare.into_iter().collect()
    };

    let scope_project = q.scope.as_deref() == Some("project");
    let db = instance.config_db_path().to_path_buf();
    let scope_id = if scope_project {
        config::project_scope(&instance.root)
    } else {
        config::GLOBAL_SCOPE.to_string()
    };
    let mut section = config::store::read_scope_at(&db, &scope_id)
        .and_then(|v| v.get("tool_config").cloned())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    for (name, value) in entries {
        let name = name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        if value.is_null() {
            section.remove(&name);
            continue;
        }
        let patch = value.as_object().ok_or_else(|| {
            ApiError::bad_request(format!(
                "tool config for {name:?} must be an object or null, got {value}"
            ))
            .into_response()
        })?;
        // Deep-merge into the existing per-tool entry (field-by-field).
        let mut entry = section
            .get(&name)
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        for (k, v) in patch {
            match k.as_str() {
                "enabled" => {
                    if !v.is_boolean() {
                        return Err(ApiError::bad_request(format!(
                            "tool config {name:?}.enabled must be a boolean, got {v}"
                        ))
                        .into_response());
                    }
                    entry.insert(k.clone(), v.clone());
                }
                "description" => {
                    if !v.is_string() {
                        return Err(ApiError::bad_request(format!(
                            "tool config {name:?}.description must be a string, got {v}"
                        ))
                        .into_response());
                    }
                    entry.insert(k.clone(), v.clone());
                }
                _ => {
                    entry.insert(k.clone(), v.clone());
                }
            }
        }
        section.insert(name, serde_json::Value::Object(entry));
    }

    config::store::write_delta_at(
        &db,
        &scope_id,
        &serde_json::json!({ "tool_config": serde_json::Value::Object(section) }),
    )
    .map_err(|e| err_response(&CoreError::Other(e)))?;

    let instance = state
        .store
        .reload_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    Ok(Json(tool_list_response(&instance)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_accepts_wrapped_and_bare_maps() {
        let wrapped: SafetyBody =
            serde_json::from_str(r#"{ "overrides": { "bash": "caution", "rm": null } }"#).unwrap();
        let map = wrapped.overrides.unwrap();
        assert_eq!(
            map.get("bash").cloned().flatten().as_deref(),
            Some("caution")
        );
        assert_eq!(map.get("rm").cloned().flatten(), None);

        let bare: SafetyBody = serde_json::from_str(r#"{ "bash": "caution" }"#).unwrap();
        assert!(bare.overrides.is_none());
        assert_eq!(
            bare.bare.get("bash").and_then(|v| v.as_str()),
            Some("caution")
        );
    }

    #[test]
    fn tool_config_body_accepts_wrapped_and_bare_shapes() {
        let wrapped: ToolConfigBody =
            serde_json::from_str(r#"{ "tools": { "bash": { "enabled": false }, "rm": null } }"#)
                .unwrap();
        let map = wrapped.tools.unwrap();
        assert!(map.contains_key("bash"));
        assert_eq!(map.get("rm").cloned().flatten(), None);

        let bare: ToolConfigBody =
            serde_json::from_str(r#"{ "grep": { "description": "mine" } }"#).unwrap();
        assert!(bare.tools.is_none() && bare.overrides.is_none());
        assert_eq!(
            bare.bare
                .get("grep")
                .and_then(|v| v.get("description"))
                .and_then(|v| v.as_str()),
            Some("mine")
        );
    }
}
