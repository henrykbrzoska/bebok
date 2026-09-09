use async_trait::async_trait;
use serde_json::{Value, json};

use crate::explorer;
use crate::tool::{Tool, ToolCtx, ToolOutput};

/// List the immediate contents of a directory (gitignore-aware).
pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn description(&self) -> &str {
        "List the immediate contents of a directory (gitignore-aware), relative to the project root."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to list, relative to the project root. Defaults to '.'."
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
        let entries = explorer::list_children(&ctx.root, path);
        if entries.is_empty() {
            return ToolOutput::new("(empty directory)", format!("list_dir {path}"));
        }
        let mut out = String::new();
        for e in &entries {
            let suffix = if e.is_dir { "/" } else { "" };
            out.push_str(&format!("{}{}\n", e.path, suffix));
        }
        ToolOutput::new(out, format!("list_dir {path}"))
    }
}
