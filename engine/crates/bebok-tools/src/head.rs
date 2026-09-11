use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Print the first N lines of a file.
///
/// Portable replacement for `head` via the `bash` tool. Prefer `read_file`
/// with `limit` for arbitrary ranges; `head` is the familiar shorthand.
pub struct Head;

#[async_trait]
impl Tool for Head {
    fn name(&self) -> &str {
        "head"
    }

    fn description(&self) -> &str {
        "Return the first N lines of a file (default 20). Portable alternative to `head` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File to read, relative to the project root."
                },
                "lines": {
                    "type": "integer",
                    "description": "Number of lines to return. Defaults to 20."
                }
            },
            "required": ["path"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'path'", "head");
        };
        let lines = args
            .get("lines")
            .and_then(|v| v.as_u64())
            .filter(|v| *v > 0)
            .unwrap_or(20) as usize;

        let full = ctx.root.join(path);
        match tokio::fs::read_to_string(&full).await {
            Ok(text) => {
                let out: Vec<&str> = text.lines().take(lines).collect();
                let mut rendered = out.join("\n");
                if rendered.is_empty() {
                    rendered = "(empty file)".to_string();
                }
                ToolOutput::new(rendered, format!("head {path}"))
            }
            Err(e) => ToolOutput::new(format!("error: failed to read {path}: {e}"), "head"),
        }
    }
}
