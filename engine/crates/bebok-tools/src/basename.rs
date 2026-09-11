use std::path::Path;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// The final component of a path.
///
/// Portable replacement for `basename` via the `bash` tool.
pub struct Basename;

#[async_trait]
impl Tool for Basename {
    fn name(&self) -> &str {
        "basename"
    }

    fn description(&self) -> &str {
        "Return the final component of a path, optionally stripping a suffix (portable `basename`)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to take the final component of."
                },
                "suffix": {
                    "type": "string",
                    "description": "Optional suffix to strip from the result (e.g. \".rs\")."
                }
            },
            "required": ["path"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'path'", "basename");
        };
        // Trailing separators would make `file_name()` None; trim them first.
        let trimmed = path.trim_end_matches(['/', '\\']);
        let name = Path::new(trimmed)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| trimmed.to_string());
        let name = match args.get("suffix").and_then(|v| v.as_str()) {
            Some(sfx) if !sfx.is_empty() && name != sfx && name.ends_with(sfx) => {
                name[..name.len() - sfx.len()].to_string()
            }
            _ => name,
        };
        ToolOutput::new(name, format!("basename {path}"))
    }
}
