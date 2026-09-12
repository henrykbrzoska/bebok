use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Move or rename a file or directory.
///
/// Portable replacement for `mv` via the `bash` tool. Falls back to a
/// copy + delete when a plain rename is not possible (e.g. across devices).
pub struct Mv;

#[async_trait]
impl Tool for Mv {
    fn name(&self) -> &str {
        "mv"
    }

    fn description(&self) -> &str {
        "Move or rename a file or directory. Portable alternative to `mv` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from": {
                    "type": "string",
                    "description": "Source file or directory, relative to the project root."
                },
                "to": {
                    "type": "string",
                    "description": "Destination path, relative to the project root."
                }
            },
            "required": ["from", "to"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let (Some(from), Some(to)) = (
            args.get("from").and_then(|v| v.as_str()),
            args.get("to").and_then(|v| v.as_str()),
        ) else {
            return ToolOutput::new("error: missing required parameters 'from' and 'to'", "mv");
        };

        let src = ctx.root.join(from);
        let dst = ctx.root.join(to);

        if tokio::fs::symlink_metadata(&src).await.is_err() {
            return ToolOutput::new(format!("error: source not found: {from}"), "mv");
        }

        if let Some(parent) = dst.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return ToolOutput::new(format!("error: failed to create parent of {to}: {e}"), "mv");
        }

        if let Err(e) = tokio::fs::rename(&src, &dst).await {
            return ToolOutput::new(format!("error: failed to move {from} -> {to}: {e}"), "mv");
        }

        ToolOutput::new(format!("moved {from} -> {to}"), format!("mv {from} {to}"))
    }
}
