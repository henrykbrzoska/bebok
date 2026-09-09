use async_trait::async_trait;
use serde_json::{Value, json};

use crate::explorer;
use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Render a recursive directory tree (gitignore-aware).
pub struct Tree;

#[async_trait]
impl Tool for Tree {
    fn name(&self) -> &str {
        "tree"
    }

    fn description(&self) -> &str {
        "Render a recursive directory tree (gitignore-aware) relative to the project root."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to render, relative to the project root. Defaults to '.'."
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum depth to recurse. Defaults to 3."
                }
            },
            "required": []
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let max_depth = args
            .get("max_depth")
            .and_then(|v| v.as_u64())
            .unwrap_or(3)
            .clamp(1, 24) as usize;

        let text = explorer::tree_text(&ctx.root, path, max_depth);
        ToolOutput::new(text, format!("tree {path}"))
    }
}
