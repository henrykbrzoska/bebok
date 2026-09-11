use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Create an empty file if it does not exist.
///
/// Portable replacement for `touch` via the `bash` tool. Existing files are
/// left untouched (their contents are never truncated).
pub struct Touch;

#[async_trait]
impl Tool for Touch {
    fn name(&self) -> &str {
        "touch"
    }

    fn description(&self) -> &str {
        "Create an empty file if it does not exist (parents are created). Existing files are left unchanged. Portable alternative to `touch` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File to create, relative to the project root."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'path'", "touch");
        };

        let full = ctx.root.join(path);
        if let Some(parent) = full.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolOutput::new(
                    format!("error: failed to create parent dir: {e}"),
                    "touch",
                );
            }
        }

        // `create` + `append` opens without truncating an existing file.
        match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&full)
            .await
        {
            Ok(_) => ToolOutput::new(format!("touched {path}"), format!("touch {path}")),
            Err(e) => ToolOutput::new(format!("error: failed to touch {path}: {e}"), "touch"),
        }
    }
}
