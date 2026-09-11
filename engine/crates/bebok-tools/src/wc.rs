use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Count lines, words, characters and bytes in a file.
///
/// Portable replacement for `wc` via the `bash` tool.
pub struct Wc;

#[async_trait]
impl Tool for Wc {
    fn name(&self) -> &str {
        "wc"
    }

    fn description(&self) -> &str {
        "Count lines, words, characters and bytes in a file. Portable alternative to `wc` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File to count, relative to the project root."
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
            return ToolOutput::new("error: missing required parameter 'path'", "wc");
        };

        let full = ctx.root.join(path);
        match tokio::fs::read_to_string(&full).await {
            Ok(text) => {
                let lines = text.lines().count();
                let words = text.split_whitespace().count();
                let chars = text.chars().count();
                let bytes = text.len();
                ToolOutput::new(
                    format!(
                        "{lines}\t{words}\t{chars}\t{bytes}\t{path}\n(lines, words, chars, bytes)"
                    ),
                    format!("wc {path}"),
                )
            }
            Err(e) => ToolOutput::new(format!("error: failed to read {path}: {e}"), "wc"),
        }
    }
}
