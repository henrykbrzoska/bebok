use std::path::Path;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// The directory component of a path.
///
/// Portable replacement for `dirname` via the `bash` tool.
pub struct Dirname;

#[async_trait]
impl Tool for Dirname {
    fn name(&self) -> &str {
        "dirname"
    }

    fn description(&self) -> &str {
        "Return the directory part of a path (portable `dirname`). Uses \".\" when there is no parent."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to take the directory of."
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
            return ToolOutput::new("error: missing required parameter 'path'", "dirname");
        };
        let trimmed = path.trim_end_matches(['/', '\\']);
        let dir = Path::new(trimmed)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
        ToolOutput::new(dir, format!("dirname {path}"))
    }
}
