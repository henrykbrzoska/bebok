use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Create a directory (and any missing parents).
///
/// Portable replacement for `mkdir -p` via the `bash` tool, so the same call
/// works on Windows (`mkdir`/`md`), macOS and Linux.
pub struct Mkdir;

#[async_trait]
impl Tool for Mkdir {
    fn name(&self) -> &str {
        "mkdir"
    }

    fn description(&self) -> &str {
        "Create a directory (and any missing parents). Portable alternative to `mkdir -p` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to create, relative to the project root."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'path'", "mkdir");
        };

        let full = ctx.root.join(path);
        match tokio::fs::create_dir_all(&full).await {
            Ok(()) => ToolOutput::new(format!("created directory {path}"), format!("mkdir {path}")),
            Err(e) => ToolOutput::new(format!("error: failed to create {path}: {e}"), "mkdir"),
        }
    }
}
