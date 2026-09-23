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
         Use `code_graph_depends(module)` for what a module imports, \
         `code_graph_dependents(module)` for its reverse dependencies, \
         `code_graph_impact(module)` for change blast-radius analysis. \
         Modules are addressed by project-relative path.",
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
}
