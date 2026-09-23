use serde::{Deserialize, Serialize};

/// Identifies a Rust module by its file path (relative to workspace root)
/// and the crate it belongs to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ModuleId {
    pub path: String,
    pub crate_name: String,
}

/// The kind of dependency edge between two modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DepKind {
    /// `use crate::foo;` or `use super::bar;` — same-crate reference.
    Use,
    /// `mod foo;` — file-level mod declaration (child module).
    ModDecl,
    /// `mod foo { ... }` — inline mod block inside a file.
    ModInline,
    /// `use other_crate::foo;` — cross-crate or external dependency.
    UseExternal,
}

/// A directed edge from one module to another.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepEdge {
    pub from: ModuleId,
    pub to: ModuleId,
    pub kind: DepKind,
}

/// A node in the dependency graph representing a single Rust source file / module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleNode {
    pub id: ModuleId,
    pub lines: usize,
    pub exports: Vec<String>,
}

/// Top-level metadata about the graph build.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphMeta {
    /// Absolute path to the workspace root the graph was built from.
    pub root: String,
    /// Total number of `.rs` files scanned.
    pub files_scanned: usize,
    /// Wall-clock time for the build in milliseconds.
    pub build_duration_ms: u64,
    /// ISO-8601 timestamp of when the graph was built.
    pub built_at: String,
}

/// The complete dependency graph for a Rust workspace.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DependencyGraph {
    pub version: u32,
    pub modules: Vec<ModuleNode>,
    pub edges: Vec<DepEdge>,
    pub meta: GraphMeta,
}

/// Result of an impact analysis: what is affected if `target` changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactResult {
    /// The root module that was changed.
    pub target: String,
    /// Total number of modules affected (across all depths).
    pub total_affected: usize,
    /// `by_depth[d]` = list of module paths at distance `d` from the target.
    pub by_depth: Vec<Vec<String>>,
    /// Shortest-path critical chain from target to the most-remote affected module.
    pub critical_path: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_module(path: &str, crate_name: &str) -> ModuleNode {
        ModuleNode {
            id: ModuleId {
                path: path.to_string(),
                crate_name: crate_name.to_string(),
            },
            lines: 100,
            exports: vec![],
        }
    }

    #[test]
    fn module_id_round_trip() {
        let id = ModuleId {
            path: "src/lib.rs".to_string(),
            crate_name: "bebok_core".to_string(),
        };
        let json = serde_json::to_string(&id).unwrap();
        let back: ModuleId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn dep_kind_copy_and_eq() {
        let a = DepKind::Use;
        let b = a; // Copy
        assert_eq!(a, b);
        assert_ne!(DepKind::Use, DepKind::ModDecl);
    }

    #[test]
    fn dep_edge_round_trip() {
        let edge = DepEdge {
            from: ModuleId {
                path: "a.rs".into(),
                crate_name: "c".into(),
            },
            to: ModuleId {
                path: "b.rs".into(),
                crate_name: "c".into(),
            },
            kind: DepKind::Use,
        };
        let json = serde_json::to_string(&edge).unwrap();
        let back: DepEdge = serde_json::from_str(&json).unwrap();
        assert_eq!(edge.from, back.from);
        assert_eq!(edge.to, back.to);
        assert_eq!(edge.kind, back.kind);
    }

    #[test]
    fn dependency_graph_default() {
        let g = DependencyGraph::default();
        assert_eq!(g.version, 0);
        assert!(g.modules.is_empty());
        assert!(g.edges.is_empty());
        assert_eq!(g.meta.root, "");
    }

    #[test]
    fn graph_meta_default() {
        let m = GraphMeta::default();
        assert_eq!(m.files_scanned, 0);
        assert_eq!(m.build_duration_ms, 0);
        assert!(m.built_at.is_empty());
    }

    #[test]
    fn impact_result_fields() {
        let r = ImpactResult {
            target: "foo.rs".into(),
            total_affected: 3,
            by_depth: vec![vec!["bar.rs".into()], vec!["baz.rs".into()]],
            critical_path: vec!["foo.rs".into(), "bar.rs".into(), "baz.rs".into()],
        };
        assert_eq!(r.total_affected, 3);
        assert_eq!(r.by_depth.len(), 2);
        assert_eq!(r.critical_path.len(), 3);
        let _ = make_module("x.rs", "x");
    }
}
