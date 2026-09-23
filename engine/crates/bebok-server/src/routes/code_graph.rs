//! Code graph routes: `GET /code-graph` + `POST /code-graph/regenerate`.
//!
//! `GET` returns the cached `<project>/.bebok/code_graph.json` (or builds it
//! on the fly when missing); `POST /code-graph/regenerate` forces a fresh
//! scan via [`bebok_core::code_graph::build_graph`] + `save_graph`.
//!
//! Both are gated on `code_graph.enabled`: a disabled project keeps the
//! "completely off" contract — no scan, no cache file, no I/O beyond the
//! config check.

use axum::Json;
use axum::extract::{Query, State};

use axum::response::Response;
use bebok_core::code_graph;
use bebok_core::config::ResolvedConfig;

use crate::error::err_response;
use crate::routes::common::DirectoryQuery;
use crate::state::AppState;

/// Shared payload builder: `{ enabled, graph }`.
fn code_graph_payload(
    root: &std::path::Path,
    cfg: &ResolvedConfig,
    regenerate: bool,
) -> Json<serde_json::Value> {
    if !cfg.code_graph.enabled {
        return Json(serde_json::json!({ "enabled": false, "graph": null }));
    }
    let graph = if regenerate {
        let g = code_graph::build_graph(root);
        if let Err(e) = code_graph::save_graph(root, &g) {
            tracing::warn!("code graph cache not written: {e}");
        }
        g
    } else {
        match code_graph::load_graph(root) {
            Some(g) => g,
            None => {
                let g = code_graph::build_graph(root);
                if let Err(e) = code_graph::save_graph(root, &g) {
                    tracing::debug!("code graph cache not written: {e}");
                }
                g
            }
        }
    };
    Json(serde_json::json!({ "enabled": true, "graph": graph }))
}

/// `GET /code-graph?directory=` -> cached (or lazily built) graph.
pub async fn get_code_graph(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let cfg = instance.config_snapshot();
    Ok(code_graph_payload(&instance.root, &cfg, false))
}

/// `POST /code-graph/regenerate?directory=` -> force a rescan + cache rewrite.
pub async fn regenerate_code_graph(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let cfg = instance.config_snapshot();
    Ok(code_graph_payload(&instance.root, &cfg, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bebok_core::config::CodeGraphConfig;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let base =
            std::env::temp_dir().join(format!("bebok-cg-route-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn disabled_payload_is_completely_off() {
        let root = temp_root("disabled");
        let cfg = ResolvedConfig::default();
        assert!(!cfg.code_graph.enabled);

        let json = code_graph_payload(&root, &cfg, true);
        assert_eq!(json.0["enabled"], serde_json::json!(false));
        assert!(json.0["graph"].is_null());
        assert!(!code_graph::graph_file_path(&root).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn enabled_regenerate_writes_cache() {
        let root = temp_root("regenerate");
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        let cfg = ResolvedConfig {
            code_graph: CodeGraphConfig {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let json = code_graph_payload(&root, &cfg, true);
        assert_eq!(json.0["enabled"], serde_json::json!(true));
        assert!(json.0["graph"].is_object());
        assert!(code_graph::graph_file_path(&root).exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
