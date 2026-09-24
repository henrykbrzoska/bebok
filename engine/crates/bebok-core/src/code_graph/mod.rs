pub mod graph;
pub mod parser;

pub use graph::*;
pub use parser::{parse_file, parse_file_content};

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Instant;

// ---------------------------------------------------------------------------
// Graph construction (bonus — not spec-mandated but already present)
// ---------------------------------------------------------------------------

/// Maximum number of `.rs` files to scan before stopping.
const MAX_FILES: usize = 5000;

/// Build a dependency graph by scanning `.rs` files under `root`.
///
/// This is a **stub** implementation: it creates a `ModuleNode` for every
/// `.rs` file discovered (up to [`MAX_FILES`]) and a `DepEdge` for each `use`
/// statement whose target maps to a known file.  It is intentionally simple —
/// a proper AST parse can be layered on later.
pub fn build_graph(root: &Path) -> DependencyGraph {
    let start = Instant::now();

    // Collect all .rs files.
    let mut rs_files: Vec<PathBuf> = Vec::new();
    collect_rs_files(root, &mut rs_files, &mut HashSet::new());
    rs_files.sort();
    if rs_files.len() > MAX_FILES {
        rs_files.truncate(MAX_FILES);
    }

    // Map relative-path → ModuleId.
    let mut id_map: HashMap<String, ModuleId> = HashMap::new();
    for f in &rs_files {
        let rel = f
            .strip_prefix(root)
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/");
        let crate_name = rel.split('/').next().unwrap_or("unknown").to_string();
        id_map.insert(
            rel.clone(),
            ModuleId {
                path: rel,
                crate_name,
            },
        );
    }

    // Build nodes.
    let modules: Vec<ModuleNode> = rs_files
        .iter()
        .filter_map(|f| {
            let rel = f
                .strip_prefix(root)
                .unwrap_or(f)
                .to_string_lossy()
                .replace('\\', "/");
            let id = id_map.get(&rel)?.clone();
            let lines = std::fs::read_to_string(f)
                .map(|c| c.lines().count())
                .unwrap_or(0);
            Some(ModuleNode {
                id,
                lines,
                exports: Vec::new(), // stub — full export extraction is future work
            })
        })
        .collect();

    // Build edges by scanning for `use` statements.
    let mut edges: Vec<DepEdge> = Vec::new();
    for f in &rs_files {
        let from_rel = f
            .strip_prefix(root)
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/");
        let from_id = match id_map.get(&from_rel) {
            Some(id) => id.clone(),
            None => continue,
        };
        if let Ok(content) = std::fs::read_to_string(f) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("use ") || trimmed.starts_with("pub use ") {
                    let path_part = trimmed
                        .trim_start_matches("pub ")
                        .trim_start_matches("use ")
                        .trim_end_matches(';')
                        .trim();
                    // Try to resolve the first two segments to a file path.
                    if let Some(target_rel) = resolve_use_to_file(path_part, root, &id_map)
                        && let Some(to_id) = id_map.get(&target_rel)
                    {
                        let kind = if is_external_use(path_part, &from_id.crate_name) {
                            DepKind::UseExternal
                        } else {
                            DepKind::Use
                        };
                        edges.push(DepEdge {
                            from: from_id.clone(),
                            to: to_id.clone(),
                            kind,
                        });
                    }
                }
            }
        }
    }

    let elapsed = start.elapsed();
    let now = chrono::Utc::now().to_rfc3339();

    DependencyGraph {
        version: 1,
        modules,
        edges,
        meta: GraphMeta {
            root: root.to_string_lossy().to_string(),
            files_scanned: rs_files.len(),
            build_duration_ms: elapsed.as_millis() as u64,
            built_at: now,
        },
    }
}

/// Recursively collect `.rs` files, skipping common non-source dirs.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>, visited: &mut HashSet<PathBuf>) {
    let canonical = match std::fs::canonicalize(dir) {
        Ok(p) => p,
        Err(_) => return,
    };
    if !visited.insert(canonical) {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "target" || name == ".git" || name == "node_modules" || name == ".bebok" {
            continue;
        }
        if path.is_dir() {
            collect_rs_files(&path, out, visited);
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

/// Try to map a `use` path (e.g. `crate::foo::bar`) to a relative file path.
fn resolve_use_to_file(
    use_path: &str,
    _root: &Path,
    id_map: &HashMap<String, ModuleId>,
) -> Option<String> {
    let segments: Vec<&str> = use_path.split("::").collect();
    if segments.is_empty() {
        return None;
    }

    // Build candidate file paths.
    let mut candidates: Vec<String> = Vec::new();

    // Absolute crate path: skip the first segment if it's `crate`, `self`, `super`.
    let start_idx = match segments[0] {
        "crate" | "self" | "super" => 1,
        _ => 0,
    };

    // <segments.join("/")>.rs
    let as_file: String = segments[start_idx..].join("/");
    candidates.push(format!("{as_file}.rs"));
    // <segments.join("/")>/mod.rs
    candidates.push(format!("{as_file}/mod.rs"));

    // Also try just the first two segments (common pattern).
    if segments.len() > 2 {
        let partial: String =
            segments[start_idx..std::cmp::min(start_idx + 2, segments.len())].join("/");
        candidates.push(format!("{partial}.rs"));
        candidates.push(format!("{partial}/mod.rs"));
    }

    for c in &candidates {
        if id_map.contains_key(c.as_str()) {
            return Some(c.clone());
        }
        // Also check with crate-name prefix.
        for rel in id_map.keys() {
            if rel == c || rel.ends_with(&format!("/{c}")) {
                return Some(rel.clone());
            }
        }
    }
    None
}

/// Heuristic: if the first segment is not our own crate name, treat as external.
fn is_external_use(use_path: &str, own_crate: &str) -> bool {
    let first = use_path.split("::").next().unwrap_or("");
    first != "crate" && first != "self" && first != "super" && first != own_crate
}

// ---------------------------------------------------------------------------
// Spec-mandated API
// ---------------------------------------------------------------------------

/// Return the canonical file path for the code graph JSON of a given project root.
pub fn graph_file_path(root: &Path) -> PathBuf {
    root.join(".bebok/code_graph.json")
}

/// Save the dependency graph atomically (tmp + rename).
pub fn save_graph(root: &Path, g: &DependencyGraph) -> Result<(), String> {
    let path = graph_file_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(g).map_err(|e| format!("serde: {e}"))?;
    // Atomic write: tmp file + rename.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Load the dependency graph from the canonical path for `root`.
/// Returns `None` if the file does not exist or fails to parse.
pub fn load_graph(root: &Path) -> Option<DependencyGraph> {
    let path = graph_file_path(root);
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Return the edges where `module` is the *source* (modules `module` depends on).
pub fn depends_on<'a>(g: &'a DependencyGraph, module: &str) -> Vec<&'a DepEdge> {
    g.edges.iter().filter(|e| e.from.path == module).collect()
}

/// Return the edges where `module` is the *target* (modules that depend on `module`).
pub fn depended_by<'a>(g: &'a DependencyGraph, module: &str) -> Vec<&'a DepEdge> {
    g.edges.iter().filter(|e| e.to.path == module).collect()
}

/// Maximum depth for impact analysis (hard cap).
const IMPACT_DEPTH_LIMIT: usize = 10;

/// Compute the impact of changing `target`: BFS through reverse edges (incoming)
/// up to `max_depth` hops (capped at 10).  Returns which modules are affected
/// at each depth and a critical-path chain.
pub fn impact_analysis(g: &DependencyGraph, target: &str, max_depth: usize) -> ImpactResult {
    let depth_limit = max_depth.min(IMPACT_DEPTH_LIMIT);

    // Build reverse adjacency: module → list of modules that import it.
    let mut incoming: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &g.edges {
        incoming
            .entry(e.to.path.as_str())
            .or_default()
            .push(e.from.path.as_str());
    }

    let mut visited: HashSet<&str> = HashSet::new();
    let mut by_depth: Vec<Vec<String>> = Vec::new();
    let mut queue: VecDeque<(&str, usize)> = VecDeque::new();

    visited.insert(target);
    queue.push_back((target, 0));

    while let Some((node, depth)) = queue.pop_front() {
        while by_depth.len() <= depth {
            by_depth.push(Vec::new());
        }
        if depth > 0 {
            by_depth[depth].push(node.to_string());
        }

        // Stop expanding beyond max_depth.
        if depth >= depth_limit {
            continue;
        }

        if let Some(parents) = incoming.get(node) {
            for &parent in parents {
                if visited.insert(parent) {
                    queue.push_back((parent, depth + 1));
                }
            }
        }
    }

    let total_affected: usize = by_depth.iter().map(|v| v.len()).sum();

    // Critical path: longest chain from target outward through reverse edges.
    let critical_path = compute_critical_path(g, target, &incoming);

    ImpactResult {
        target: target.to_string(),
        total_affected,
        by_depth,
        critical_path,
    }
}

/// Longest path from `target` outward through incoming edges (max blast-radius chain).
fn compute_critical_path<'a>(
    _g: &'a DependencyGraph,
    target: &str,
    incoming: &HashMap<&'a str, Vec<&'a str>>,
) -> Vec<String> {
    // DFS with memoization (longest path in a DAG, cycles broken by visited set).
    fn longest_path<'a>(
        node: &'a str,
        incoming: &HashMap<&'a str, Vec<&'a str>>,
        memo: &mut HashMap<&'a str, Vec<String>>,
        visited: &mut HashSet<&'a str>,
    ) -> Vec<String> {
        if let Some(cached) = memo.get(node) {
            return cached.clone();
        }
        let mut best: Vec<String> = Vec::new();
        if let Some(parents) = incoming.get(node) {
            for &parent in parents {
                if visited.insert(parent) {
                    let path = longest_path(parent, incoming, memo, visited);
                    if path.len() > best.len() {
                        best = path;
                    }
                    visited.remove(parent);
                }
            }
        }
        let mut result = vec![node.to_string()];
        result.append(&mut best);
        memo.insert(node, result.clone());
        result
    }

    let mut memo = HashMap::new();
    let mut visited = HashSet::new();
    visited.insert(target);
    longest_path(target, incoming, &mut memo, &mut visited)
}

// ---------------------------------------------------------------------------
// Cycle detection (DFS-based SCC — Tarjan's algorithm)
// ---------------------------------------------------------------------------

/// Find all strongly connected components (cycles) in the graph.
/// Returns a list of SCCs, each represented as a sorted list of module paths.
/// SCCs of size > 1 contain actual cycles; size == 1 means a self-loop.
pub fn find_cycles(graph: &DependencyGraph) -> Vec<Vec<String>> {
    // Build outgoing adjacency from the edges list.
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &graph.edges {
        adj.entry(e.from.path.as_str())
            .or_default()
            .push(e.to.path.as_str());
    }

    let all_paths: Vec<String> = graph.modules.iter().map(|m| m.id.path.clone()).collect();

    // Tarjan state bundled so the recursive helper stays under the arg limit.
    struct TarjanState<'a> {
        adj: &'a HashMap<&'a str, Vec<&'a str>>,
        index: u32,
        stack: Vec<String>,
        on_stack: HashSet<String>,
        indices: HashMap<String, u32>,
        lowlinks: HashMap<String, u32>,
        sccs: Vec<Vec<String>>,
    }

    fn strongconnect(v: &str, st: &mut TarjanState<'_>) {
        st.indices.insert(v.to_string(), st.index);
        st.lowlinks.insert(v.to_string(), st.index);
        st.index += 1;
        st.stack.push(v.to_string());
        st.on_stack.insert(v.to_string());

        // Clone the neighbour list: recursion borrows `st` mutably.
        let neighbours: Vec<String> = st
            .adj
            .get(v)
            .map(|n| n.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        for w in &neighbours {
            if !st.indices.contains_key(w.as_str()) {
                strongconnect(w, st);
                let low_v = *st.lowlinks.get(v).unwrap();
                let low_w = *st.lowlinks.get(w.as_str()).unwrap();
                st.lowlinks.insert(v.to_string(), low_v.min(low_w));
            } else if st.on_stack.contains(w.as_str()) {
                let low_v = *st.lowlinks.get(v).unwrap();
                let idx_w = *st.indices.get(w.as_str()).unwrap();
                st.lowlinks.insert(v.to_string(), low_v.min(idx_w));
            }
        }

        // If v is a root node, pop the SCC.
        let low_v = *st.lowlinks.get(v).unwrap();
        let idx_v = *st.indices.get(v).unwrap();
        if low_v == idx_v {
            let mut scc: Vec<String> = Vec::new();
            loop {
                let w = st.stack.pop().unwrap();
                st.on_stack.remove(&w);
                let is_root = w == v;
                scc.push(w);
                if is_root {
                    break;
                }
            }
            scc.sort();
            st.sccs.push(scc);
        }
    }

    let mut st = TarjanState {
        adj: &adj,
        index: 0,
        stack: Vec::new(),
        on_stack: HashSet::new(),
        indices: HashMap::new(),
        lowlinks: HashMap::new(),
        sccs: Vec::new(),
    };

    for path in &all_paths {
        if !st.indices.contains_key(path.as_str()) {
            strongconnect(path, &mut st);
        }
    }
    let sccs = st.sccs;

    // Return SCCs of size > 1 (actual cycles) or size 1 with a self-loop.
    let mut cycles: Vec<Vec<String>> = Vec::new();
    for scc in sccs {
        if scc.len() > 1 {
            cycles.push(scc);
        } else if scc.len() == 1 {
            let node = &scc[0];
            if let Some(neighbours) = adj.get(node.as_str())
                && neighbours.contains(&node.as_str())
            {
                cycles.push(scc);
            }
        }
    }
    cycles
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_graph() -> DependencyGraph {
        // a -> b -> c   (linear chain)
        // a -> d        (branch)
        // d -> b        (cross edge, no cycle)
        let modules = vec![
            ModuleNode {
                id: ModuleId {
                    path: "a.rs".into(),
                    crate_name: "test".into(),
                },
                lines: 10,
                exports: vec![],
            },
            ModuleNode {
                id: ModuleId {
                    path: "b.rs".into(),
                    crate_name: "test".into(),
                },
                lines: 20,
                exports: vec![],
            },
            ModuleNode {
                id: ModuleId {
                    path: "c.rs".into(),
                    crate_name: "test".into(),
                },
                lines: 30,
                exports: vec![],
            },
            ModuleNode {
                id: ModuleId {
                    path: "d.rs".into(),
                    crate_name: "test".into(),
                },
                lines: 15,
                exports: vec![],
            },
        ];
        let edges = vec![
            DepEdge {
                from: ModuleId {
                    path: "a.rs".into(),
                    crate_name: "test".into(),
                },
                to: ModuleId {
                    path: "b.rs".into(),
                    crate_name: "test".into(),
                },
                kind: DepKind::Use,
            },
            DepEdge {
                from: ModuleId {
                    path: "a.rs".into(),
                    crate_name: "test".into(),
                },
                to: ModuleId {
                    path: "d.rs".into(),
                    crate_name: "test".into(),
                },
                kind: DepKind::Use,
            },
            DepEdge {
                from: ModuleId {
                    path: "b.rs".into(),
                    crate_name: "test".into(),
                },
                to: ModuleId {
                    path: "c.rs".into(),
                    crate_name: "test".into(),
                },
                kind: DepKind::Use,
            },
            DepEdge {
                from: ModuleId {
                    path: "d.rs".into(),
                    crate_name: "test".into(),
                },
                to: ModuleId {
                    path: "b.rs".into(),
                    crate_name: "test".into(),
                },
                kind: DepKind::Use,
            },
        ];
        DependencyGraph {
            version: 1,
            modules,
            edges,
            meta: GraphMeta::default(),
        }
    }

    #[test]
    fn depends_on_returns_edges() {
        let g = sample_graph();
        let edges = depends_on(&g, "a.rs");
        assert_eq!(edges.len(), 2);
        let targets: Vec<&str> = edges.iter().map(|e| e.to.path.as_str()).collect();
        assert!(targets.contains(&"b.rs"));
        assert!(targets.contains(&"d.rs"));
        // Each edge must be a &DepEdge
        for e in &edges {
            assert_eq!(e.from.path, "a.rs");
            assert!(e.kind == DepKind::Use || e.kind == DepKind::UseExternal);
        }
    }

    #[test]
    fn depends_on_leaf() {
        let g = sample_graph();
        let edges = depends_on(&g, "c.rs");
        assert!(edges.is_empty());
    }

    #[test]
    fn depended_by_returns_edges() {
        let g = sample_graph();
        let edges = depended_by(&g, "b.rs");
        assert_eq!(edges.len(), 2);
        let sources: Vec<&str> = edges.iter().map(|e| e.from.path.as_str()).collect();
        assert!(sources.contains(&"a.rs"));
        assert!(sources.contains(&"d.rs"));
        for e in &edges {
            assert_eq!(e.to.path, "b.rs");
        }
    }

    #[test]
    fn depended_by_root() {
        let g = sample_graph();
        let edges = depended_by(&g, "a.rs");
        assert!(edges.is_empty());
    }

    #[test]
    fn impact_analysis_covers_all_dependents() {
        let g = sample_graph();
        // Changing c should affect b (depth 1), then a and d (depth 2).
        let result = impact_analysis(&g, "c.rs", 10);
        assert_eq!(result.target, "c.rs");
        assert!(result.total_affected >= 2);
        // Depth 1 should have b.
        assert!(result.by_depth[1].contains(&"b.rs".to_string()));
        // Depth 2 should have a and d.
        assert!(result.by_depth[2].contains(&"a.rs".to_string()));
        assert!(result.by_depth[2].contains(&"d.rs".to_string()));
    }

    #[test]
    fn impact_analysis_no_deps() {
        let g = sample_graph();
        let result = impact_analysis(&g, "a.rs", 10);
        assert_eq!(result.total_affected, 0);
        assert!(result.critical_path.len() == 1);
        assert_eq!(result.critical_path[0], "a.rs");
    }

    #[test]
    fn impact_analysis_respects_max_depth() {
        let g = sample_graph();
        // Depth 1: only b, not a or d.
        let result = impact_analysis(&g, "c.rs", 1);
        assert_eq!(result.total_affected, 1);
        assert!(result.by_depth[1].contains(&"b.rs".to_string()));
        // Depth 2+ should be empty since we capped at 1.
        assert!(result.by_depth.len() <= 2);
        let beyond: usize = result.by_depth.iter().skip(2).map(|v| v.len()).sum();
        assert_eq!(beyond, 0);
    }

    #[test]
    fn impact_depth_cap_at_10() {
        // Even if caller passes huge depth, it gets capped at 10.
        let g = sample_graph();
        let r1 = impact_analysis(&g, "c.rs", 1000);
        let r2 = impact_analysis(&g, "c.rs", 10);
        assert_eq!(r1.total_affected, r2.total_affected);
    }

    #[test]
    fn no_cycles_in_sample() {
        let g = sample_graph();
        let cycles = find_cycles(&g);
        assert!(cycles.is_empty());
    }

    #[test]
    fn cycle_detection_finds_direct_cycle() {
        let modules = vec![
            ModuleNode {
                id: ModuleId {
                    path: "x.rs".into(),
                    crate_name: "t".into(),
                },
                lines: 5,
                exports: vec![],
            },
            ModuleNode {
                id: ModuleId {
                    path: "y.rs".into(),
                    crate_name: "t".into(),
                },
                lines: 5,
                exports: vec![],
            },
        ];
        let edges = vec![
            DepEdge {
                from: ModuleId {
                    path: "x.rs".into(),
                    crate_name: "t".into(),
                },
                to: ModuleId {
                    path: "y.rs".into(),
                    crate_name: "t".into(),
                },
                kind: DepKind::Use,
            },
            DepEdge {
                from: ModuleId {
                    path: "y.rs".into(),
                    crate_name: "t".into(),
                },
                to: ModuleId {
                    path: "x.rs".into(),
                    crate_name: "t".into(),
                },
                kind: DepKind::Use,
            },
        ];
        let g = DependencyGraph {
            version: 1,
            modules,
            edges,
            meta: GraphMeta::default(),
        };
        let cycles = find_cycles(&g);
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0], vec!["x.rs", "y.rs"]);
    }

    #[test]
    fn self_loop_detected() {
        let modules = vec![ModuleNode {
            id: ModuleId {
                path: "s.rs".into(),
                crate_name: "t".into(),
            },
            lines: 5,
            exports: vec![],
        }];
        let edges = vec![DepEdge {
            from: ModuleId {
                path: "s.rs".into(),
                crate_name: "t".into(),
            },
            to: ModuleId {
                path: "s.rs".into(),
                crate_name: "t".into(),
            },
            kind: DepKind::Use,
        }];
        let g = DependencyGraph {
            version: 1,
            modules,
            edges,
            meta: GraphMeta::default(),
        };
        let cycles = find_cycles(&g);
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0], vec!["s.rs"]);
    }

    #[test]
    fn three_node_cycle() {
        // a -> b -> c -> a
        let modules: Vec<ModuleNode> = ["a.rs", "b.rs", "c.rs"]
            .iter()
            .map(|p| ModuleNode {
                id: ModuleId {
                    path: p.to_string(),
                    crate_name: "t".into(),
                },
                lines: 5,
                exports: vec![],
            })
            .collect();
        let edges: Vec<DepEdge> = ["a.rs", "b.rs", "c.rs"]
            .windows(2)
            .map(|w| DepEdge {
                from: ModuleId {
                    path: w[0].into(),
                    crate_name: "t".into(),
                },
                to: ModuleId {
                    path: w[1].into(),
                    crate_name: "t".into(),
                },
                kind: DepKind::Use,
            })
            .collect();
        // Add c -> a
        let mut edges = edges;
        edges.push(DepEdge {
            from: ModuleId {
                path: "c.rs".into(),
                crate_name: "t".into(),
            },
            to: ModuleId {
                path: "a.rs".into(),
                crate_name: "t".into(),
            },
            kind: DepKind::Use,
        });
        let g = DependencyGraph {
            version: 1,
            modules,
            edges,
            meta: GraphMeta::default(),
        };
        let cycles = find_cycles(&g);
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0], vec!["a.rs", "b.rs", "c.rs"]);
    }

    #[test]
    fn save_and_load_round_trip() {
        use std::fs;

        let root = std::env::temp_dir().join(format!("bebok-cg-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();

        let mut g = sample_graph();
        g.meta.root = root.to_string_lossy().to_string();

        save_graph(&root, &g).unwrap();
        let loaded = load_graph(&root).unwrap();
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.modules.len(), g.modules.len());
        assert_eq!(loaded.edges.len(), g.edges.len());

        // Clean up.
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn graph_file_path_is_bebok_dir() {
        let root = Path::new("/home/user/my-project");
        let p = graph_file_path(root);
        assert_eq!(
            p,
            PathBuf::from("/home/user/my-project/.bebok/code_graph.json")
        );
    }

    #[test]
    fn save_graph_returns_error_string() {
        use std::fs;

        // Platform-independent failure: use a regular FILE as the root, so
        // create_dir_all(<file>/.bebok) fails on both Linux and Windows
        // (a path component is a file, not a directory).
        let dir = std::env::temp_dir().join(format!("bebok-cg-bad-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let blocker = dir.join("blocker");
        fs::write(&blocker, b"x").unwrap();

        let g = sample_graph();
        let result = save_graph(&blocker, &g);
        assert!(result.is_err(), "saving under a file path should fail");
        let err = result.unwrap_err();
        assert!(!err.is_empty());

        // Clean up.
        fs::remove_dir_all(&dir).ok();
    }
}
