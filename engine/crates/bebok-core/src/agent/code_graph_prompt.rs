//! "Module dependency graph" system-prompt section.
//!
//! When `code_graph.enabled`, the engine injects a compact summary of the
//! project module graph (module count, edge count, build time) plus usage
//! hints for the `code_graph_depends` / `code_graph_dependents` /
//! `code_graph_impact` tools, so the model knows the graph exists and how
//! to query it. Off by default (`None` on the first line — zero I/O).

use std::path::Path;

use crate::code_graph;
use crate::config::ResolvedConfig;

/// The section for the resolved config and project root, or `None` when the
/// feature is off or no cached graph exists.
pub fn code_graph_section(root: &Path, cfg: &ResolvedConfig) -> Option<String> {
    if !cfg.code_graph.enabled {
        return None;
    }
    let g = code_graph::load_graph(root)?;
    Some(format!(
        "Module dependency graph: {} modules, {} edges (built {}).\n\
         Tools (read-only, use before modifying code):\n\
         - `code_graph_depends(module)` — what does `module` import? (outgoing edges)\n\
         - `code_graph_dependents(module)` — what imports `module`? (reverse edges)\n\
         - `code_graph_impact(module, max_depth?)` — blast-radius analysis (BFS over reverse edges, default depth 3, cap 10).\n\
         Modules are project-relative paths (e.g. `engine/crates/bebok-core/src/store`).",
        g.modules.len(),
        g.edges.len(),
        g.meta.built_at,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_returns_none() {
        let cfg = ResolvedConfig::default();
        assert!(!cfg.code_graph.enabled);
        assert!(code_graph_section(Path::new("/nonexistent"), &cfg).is_none());
    }

    #[test]
    fn enabled_without_cache_returns_none() {
        let root = std::env::temp_dir().join(format!("bebok-cg-sec-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let cfg = ResolvedConfig {
            code_graph: crate::config::CodeGraphConfig {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(code_graph_section(&root, &cfg).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn enabled_with_cache_lists_tools_and_paths() {
        let root = std::env::temp_dir().join(format!("bebok-cg-sec-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let cfg = ResolvedConfig {
            code_graph: crate::config::CodeGraphConfig {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        // A minimal cached graph (the section only reads counts + built_at).
        let graph = serde_json::json!({
            "version": 1,
            "modules": [{"id": {"path": "a.rs", "crate_name": "a"}, "lines": 1, "exports": []}],
            "edges": [],
            "meta": {"root": "", "files_scanned": 1, "build_duration_ms": 1,
                     "built_at": "2025-01-01T00:00:00Z"}
        });
        std::fs::write(crate::code_graph::graph_file_path(&root), graph.to_string()).unwrap();

        let section = code_graph_section(&root, &cfg).unwrap();
        assert!(section.contains("1 modules, 0 edges"), "{section}");
        assert!(
            section.contains("`code_graph_depends(module)`"),
            "{section}"
        );
        assert!(
            section.contains("`code_graph_dependents(module)`"),
            "{section}"
        );
        assert!(
            section.contains("`code_graph_impact(module, max_depth?)`"),
            "{section}"
        );
        assert!(section.contains("project-relative paths"), "{section}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
