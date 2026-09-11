use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Resolve a path to an absolute, canonical path.
///
/// Portable replacement for `realpath`/`readlink -f` via the `bash` tool.
/// Relative paths are resolved against the project root. When the target does
/// not exist, the lexical absolute path is returned instead of failing (so it
/// can be used before creating a file).
pub struct Realpath;

#[async_trait]
impl Tool for Realpath {
    fn name(&self) -> &str {
        "realpath"
    }

    fn description(&self) -> &str {
        "Resolve a path to an absolute canonical path (portable `realpath`/`readlink -f`). Relative paths are resolved against the project root."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to resolve, relative to the project root."
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
            return ToolOutput::new("error: missing required parameter 'path'", "realpath");
        };
        let full = ctx.root.join(path.trim());
        // Canonicalize when possible (resolves symlinks + `..`); otherwise fall
        // back to a lexically absolute path so it works for not-yet-created files.
        let resolved = std::fs::canonicalize(&full).unwrap_or(full);
        ToolOutput::new(resolved.to_string_lossy().to_string(), format!("realpath {path}"))
    }
}
