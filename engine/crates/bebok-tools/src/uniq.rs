use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Deduplicate the lines of a file.
///
/// Portable replacement for `sort -u` / `uniq -c` via the `bash` tool. Unlike
/// POSIX `uniq`, blocks of equal lines are collapsed globally (not just
/// adjacent ones).
pub struct Uniq;

#[async_trait]
impl Tool for Uniq {
    fn name(&self) -> &str {
        "uniq"
    }

    fn description(&self) -> &str {
        "Return the distinct lines of a file (globally deduplicated). Portable alternative to `sort -u` / `uniq -c` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File to deduplicate, relative to the project root."
                },
                "counts": {
                    "type": "boolean",
                    "description": "Prefix each line with its occurrence count (like `uniq -c`). Defaults to false."
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
            return ToolOutput::new("error: missing required parameter 'path'", "uniq");
        };
        let counts = args.get("counts").and_then(|v| v.as_bool()).unwrap_or(false);

        let full = ctx.root.join(path);
        let text = match tokio::fs::read_to_string(&full).await {
            Ok(t) => t,
            Err(e) => {
                return ToolOutput::new(format!("error: failed to read {path}: {e}"), "uniq");
            }
        };

        let mut map: BTreeMap<&str, usize> = BTreeMap::new();
        for line in text.lines() {
            *map.entry(line).or_insert(0) += 1;
        }

        let mut out = String::new();
        for (line, count) in &map {
            if counts {
                out.push_str(&format!("{count}\t{line}\n"));
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
        if out.is_empty() {
            out = "(empty file)".to_string();
        }

        ToolOutput::new(out.trim_end().to_string(), format!("uniq {path}"))
    }
}
