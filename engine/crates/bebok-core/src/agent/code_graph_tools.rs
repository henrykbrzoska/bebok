//! `code_graph_*` tools: query the per-project module dependency graph.
//!
//! The graph is built by [`crate::code_graph::build_graph`] (scan of `use` /
//! `mod` declarations) and cached at `<project>/.bebok/code_graph.json`.
//! These tools only *read* that cache — a missing file means the graph was
//! never built (or `code_graph.enabled` is off), which is reported as
//! `{ok: false}` rather than an error.
//!
//! Three tools, one per query direction:
//! - `code_graph_depends` — what does `module` depend on (outgoing edges);
//! - `code_graph_dependents` — what depends on `module` (incoming edges);
//! - `code_graph_impact` — blast-radius BFS over reverse edges.

use async_trait::async_trait;
use bebok_tools::{Tool, ToolCtx, ToolOutput};
use serde_json::{Value, json};

use crate::code_graph::{self, DepEdge};

fn missing_graph_output(tool: &str) -> ToolOutput {
    ToolOutput::new(
        serde_json::to_string(&json!({
            "ok": false,
            "error": "code graph not built for this project (enable `code_graph` and rescan)"
        }))
        .unwrap_or_default(),
        tool,
    )
}

fn edge_json(e: &DepEdge, other: &str, other_crate: &str) -> Value {
    json!({
        "path": other,
        "crate_name": other_crate,
        "kind": format!("{:?}", e.kind),
    })
}

// ---------------------------------------------------------------------------
// code_graph_depends
// ---------------------------------------------------------------------------

/// What does `module` depend on (outgoing edges).
pub struct CodeGraphDepends;

#[async_trait]
impl Tool for CodeGraphDepends {
    fn name(&self) -> &str {
        "code_graph_depends"
    }

    fn description(&self) -> &str {
        "List the modules a given module directly depends on (outgoing edges \
         of the project module-dependency graph: `use` / `mod` declarations). \
         The module is addressed by its project-relative path, e.g. \
         `crates/bebok-core/src/agent/mod.rs`."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "module": {
                    "type": "string",
                    "description": "Project-relative path of the module, e.g. `src/main.rs`."
                }
            },
            "required": ["module"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(module) = args.get("module").and_then(|v| v.as_str()) else {
            return ToolOutput::new(
                "error: missing required parameter 'module'",
                "code_graph_depends",
            );
        };
        let Some(g) = code_graph::load_graph(&ctx.root) else {
            return missing_graph_output("code_graph_depends");
        };
        let deps: Vec<Value> = code_graph::depends_on(&g, module)
            .iter()
            .map(|e| edge_json(e, &e.to.path, &e.to.crate_name))
            .collect();
        ToolOutput::new(
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "module": module,
                "depends_on": deps,
            }))
            .unwrap_or_default(),
            "code_graph_depends",
        )
    }
}

// ---------------------------------------------------------------------------
// code_graph_dependents
// ---------------------------------------------------------------------------

/// What depends on `module` (incoming edges).
pub struct CodeGraphDependents;

#[async_trait]
impl Tool for CodeGraphDependents {
    fn name(&self) -> &str {
        "code_graph_dependents"
    }

    fn description(&self) -> &str {
        "List the modules that directly depend on a given module (incoming \
         edges of the project module-dependency graph). The module is \
         addressed by its project-relative path, e.g. `src/main.rs`."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "module": {
                    "type": "string",
                    "description": "Project-relative path of the module, e.g. `src/main.rs`."
                }
            },
            "required": ["module"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(module) = args.get("module").and_then(|v| v.as_str()) else {
            return ToolOutput::new(
                "error: missing required parameter 'module'",
                "code_graph_dependents",
            );
        };
        let Some(g) = code_graph::load_graph(&ctx.root) else {
            return missing_graph_output("code_graph_dependents");
        };
        let rdeps: Vec<Value> = code_graph::depended_by(&g, module)
            .iter()
            .map(|e| edge_json(e, &e.from.path, &e.from.crate_name))
            .collect();
        ToolOutput::new(
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "module": module,
                "depended_by": rdeps,
            }))
            .unwrap_or_default(),
            "code_graph_dependents",
        )
    }
}

// ---------------------------------------------------------------------------
// code_graph_impact
// ---------------------------------------------------------------------------

/// Blast-radius analysis: what breaks if `module` changes.
pub struct CodeGraphImpact;

#[async_trait]
impl Tool for CodeGraphImpact {
    fn name(&self) -> &str {
        "code_graph_impact"
    }

    fn description(&self) -> &str {
        "Impact analysis for a module change: BFS over reverse dependency \
         edges up to `max_depth` hops (capped at 10). Returns affected \
         modules per depth plus a critical-path chain."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "module": {
                    "type": "string",
                    "description": "Project-relative path of the changed module."
                },
                "max_depth": {
                    "type": "integer",
                    "description": "How many hops to follow (default 5, hard cap 10)."
                }
            },
            "required": ["module"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(module) = args.get("module").and_then(|v| v.as_str()) else {
            return ToolOutput::new(
                "error: missing required parameter 'module'",
                "code_graph_impact",
            );
        };
        let max_depth = args.get("max_depth").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
        let Some(g) = code_graph::load_graph(&ctx.root) else {
            return missing_graph_output("code_graph_impact");
        };
        let result = code_graph::impact_analysis(&g, module, max_depth.min(10));
        ToolOutput::new(
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "target": result.target,
                "total_affected": result.total_affected,
                "by_depth": result.by_depth,
                "critical_path": result.critical_path,
            }))
            .unwrap_or_default(),
            "code_graph_impact",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names() {
        assert_eq!(CodeGraphDepends.name(), "code_graph_depends");
        assert_eq!(CodeGraphDependents.name(), "code_graph_dependents");
        assert_eq!(CodeGraphImpact.name(), "code_graph_impact");
    }

    #[test]
    fn tools_are_read_only() {
        assert!(CodeGraphDepends.is_read_only());
        assert!(CodeGraphDependents.is_read_only());
        assert!(CodeGraphImpact.is_read_only());
    }

    #[test]
    fn schemas_require_module() {
        for s in [
            CodeGraphDepends.parameters_schema(),
            CodeGraphDependents.parameters_schema(),
            CodeGraphImpact.parameters_schema(),
        ] {
            let req = s["required"].as_array().unwrap();
            assert!(req.iter().any(|v| v == "module"));
        }
    }

    #[tokio::test]
    async fn missing_graph_reports_ok_false() {
        use tokio_util::sync::CancellationToken;
        let root = std::env::temp_dir().join(format!("bebok-cg-tools-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let ctx = ToolCtx {
            root: root.clone(),
            session_id: "test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = CodeGraphDepends
            .execute(ctx, json!({ "module": "src/main.rs" }))
            .await;
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(v["ok"], json!(false));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn missing_module_param_is_error() {
        use tokio_util::sync::CancellationToken;
        let ctx = ToolCtx {
            root: std::env::temp_dir(),
            session_id: "test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = CodeGraphImpact.execute(ctx, json!({})).await;
        assert!(out.text.contains("missing required parameter"));
    }
}
