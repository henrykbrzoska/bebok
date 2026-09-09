use async_trait::async_trait;
use glob::glob;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Expand a glob pattern relative to the project root.
pub struct Glob;

#[async_trait]
impl Tool for Glob {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Expand a glob pattern (e.g. `src/**/*.rs`) relative to the project root and list matching paths."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern, relative to the project root."
                }
            },
            "required": ["pattern"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(pattern) = args.get("pattern").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'pattern'", "glob");
        };

        let full_pattern = ctx.root.join(pattern);
        let full_pattern = full_pattern.to_string_lossy().to_string();

        let mut matches: Vec<String> = Vec::new();
        let mut error: Option<String> = None;
        match glob(&full_pattern) {
            Ok(paths) => {
                for entry in paths.flatten() {
                    // Prefer a path relative to the root for readability.
                    let rel = entry
                        .strip_prefix(&ctx.root)
                        .unwrap_or(&entry)
                        .to_string_lossy()
                        .to_string();
                    matches.push(rel);
                }
            }
            Err(e) => error = Some(e.to_string()),
        }

        if let Some(e) = error {
            return ToolOutput::new(format!("error: bad pattern: {e}"), "glob");
        }

        if matches.is_empty() {
            return ToolOutput::new("(no matches)", "glob");
        }

        matches.sort();
        ToolOutput::new(matches.join("\n"), format!("glob {pattern}"))
    }
}
