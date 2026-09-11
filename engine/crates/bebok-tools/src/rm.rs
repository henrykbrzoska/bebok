use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Remove a file or directory.
///
/// Portable replacement for `rm` / `rm -rf` via the `bash` tool. Without
/// `recursive` a directory is refused (like plain `rm`); with it the whole
/// tree is removed.
pub struct Rm;

#[async_trait]
impl Tool for Rm {
    fn name(&self) -> &str {
        "rm"
    }

    fn description(&self) -> &str {
        "Remove a file or directory. Portable alternative to `rm` / `rm -rf` via bash. Directories need `recursive: true`."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File or directory to remove, relative to the project root."
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Remove a directory and all its contents. Defaults to false (plain files only)."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'path'", "rm");
        };
        let recursive = args
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let full = ctx.root.join(path);
        let meta = match tokio::fs::symlink_metadata(&full).await {
            Ok(m) => m,
            Err(e) => {
                return ToolOutput::new(format!("error: cannot access {path}: {e}"), "rm");
            }
        };

        let result = if meta.is_dir() {
            if !recursive {
                return ToolOutput::new(
                    format!("error: {path} is a directory; pass recursive=true to remove it"),
                    "rm",
                );
            }
            tokio::fs::remove_dir_all(&full).await
        } else {
            tokio::fs::remove_file(&full).await
        };

        match result {
            Ok(()) => ToolOutput::new(format!("removed {path}"), format!("rm {path}")),
            Err(e) => ToolOutput::new(format!("error: failed to remove {path}: {e}"), "rm"),
        }
    }
}
