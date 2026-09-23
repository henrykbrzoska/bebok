//! Code map routes: `GET /code-map` + `POST /code-map/regenerate`.
//!
//! `GET` answers "what does the engine currently inject?" for debugging /
//! UI: the cached (or lazily generated) map plus the rendered prompt
//! section. `POST /code-map/regenerate` forces a fresh scan and rewrites
//! `<project>/.bebok/code-map.json` (atomic tmp+rename).
//!
//! Both are gated on `code_map.enabled`: a disabled project keeps the
//! "completely off" contract — no generation, no cache file, no I/O.

use std::path::Path;

use axum::extract::{Query, State};
use axum::response::Response;
use axum::Json;

use bebok_core::agent::code_map_gen;
use bebok_core::config::ResolvedConfig;

use crate::error::err_response;
use crate::routes::common::DirectoryQuery;
use crate::state::AppState;

/// Payload for both endpoints: `{ enabled, map, section }`.
fn code_map_payload(root: &Path, cfg: &ResolvedConfig, regenerate: bool) -> Json<serde_json::Value> {
    if !cfg.code_map.enabled {
        return Json(serde_json::json!({ "enabled": false, "map": null, "section": null }));
    }

    let path = code_map_gen::cache_path(root);
    let cache = if regenerate {
        generate_and_write(root, cfg, &path)
    } else {
        code_map_gen::read_cache(&path)
            .filter(|c| !code_map_gen::is_stale(c))
            .or_else(|| generate_and_write(root, cfg, &path))
    };

    match cache {
        Some(cache) => Json(serde_json::json!({
            "enabled": true,
            "map": cache,
            "section": bebok_core::agent::code_map_section(root, cfg),
        })),
        None => Json(serde_json::json!({ "enabled": true, "map": null, "section": null })),
    }
}

/// Scan the project, write the cache (best effort) and return the map.
fn generate_and_write(
    root: &Path,
    cfg: &ResolvedConfig,
    path: &Path,
) -> Option<code_map_gen::CodeMapCache> {
    let cache = code_map_gen::generate_code_map(root, &cfg.code_map)?;
    if let Err(e) = code_map_gen::write_cache(path, &cache) {
        tracing::warn!("code map cache not written: {e}");
    }
    Some(cache)
}

/// `GET /code-map?directory=` -> the current map + rendered section.
pub async fn get_code_map(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let cfg = instance.config_snapshot();
    Ok(code_map_payload(&instance.root, &cfg, false))
}

/// `POST /code-map/regenerate?directory=` -> force a rescan + cache rewrite.
pub async fn regenerate_code_map(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let cfg = instance.config_snapshot();
    Ok(code_map_payload(&instance.root, &cfg, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bebok_core::config::CodeMapConfig;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("bebok-cm-route-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn disabled_payload_is_completely_off() {
        let root = temp_root("disabled");
        let cfg = ResolvedConfig::default();
        assert!(!cfg.code_map.enabled);

        let json = code_map_payload(&root, &cfg, true);
        assert_eq!(json.0["enabled"], serde_json::json!(false));
        assert!(json.0["map"].is_null());
        assert!(json.0["section"].is_null());
        // Disabled => even the regenerate endpoint creates no cache.
        assert!(!code_map_gen::cache_path(&root).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn enabled_regenerate_writes_cache_and_section() {
        let root = temp_root("regenerate");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        let cfg = ResolvedConfig {
            code_map: CodeMapConfig {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };

        let json = code_map_payload(&root, &cfg, true);
        assert_eq!(json.0["enabled"], serde_json::json!(true));
        assert!(json.0["map"]["entries"].is_array());
        let section = json.0["section"].as_str().unwrap_or_default();
        assert!(section.starts_with("Project code map:"), "{section}");
        assert!(code_map_gen::cache_path(&root).is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn get_uses_cache_until_stale() {
        let root = temp_root("get");
        std::fs::create_dir_all(root.join("keep")).unwrap();
        std::fs::write(root.join("keep/f.txt"), "x").unwrap();
        let cfg = ResolvedConfig {
            code_map: CodeMapConfig {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };

        // First GET generates…
        let json = code_map_payload(&root, &cfg, false);
        assert_eq!(json.0["enabled"], serde_json::json!(true));
        // …and writes the cache; a later GET reuses it (same entries).
        let cached = code_map_gen::read_cache(&code_map_gen::cache_path(&root)).unwrap();
        assert_eq!(cached.entries.len(), 1);
        assert_eq!(cached.entries[0].path, "keep/");
        let _ = std::fs::remove_dir_all(&root);
    }
}
