use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::fs;

use crate::pathguard::resolve_in_root;
use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Replace a literal `old_string` with `new_string` in a file (precise string
/// replacement). Performs an atomic write (tmp + rename).
pub struct EditFile;

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Replace a literal string in a text file. Provide the exact old_string to replace and the new_string replacement."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, relative to the project root."
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact literal string to find and replace."
                },
                "new_string": {
                    "type": "string",
                    "description": "The string to replace old_string with."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "If true, replace all occurrences. If false (default), replace only the first match and error if there are multiple."
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'path'", "edit_file");
        };
        let Some(old_string) = args.get("old_string").and_then(|v| v.as_str()) else {
            return ToolOutput::new(
                "error: missing required parameter 'old_string'",
                "edit_file",
            );
        };
        let Some(new_string) = args.get("new_string").and_then(|v| v.as_str()) else {
            return ToolOutput::new(
                "error: missing required parameter 'new_string'",
                "edit_file",
            );
        };
        let replace_all = args
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let resolved = match resolve_in_root(&ctx.root, path) {
            Ok(resolved) => resolved,
            Err(e) => return ToolOutput::new(format!("error: {e}"), "edit_file"),
        };

        if old_string.is_empty() {
            return ToolOutput::new("error: old_string must not be empty", "edit_file");
        }

        // Read current content.
        let content = match fs::read_to_string(&resolved).await {
            Ok(c) => c,
            Err(e) => {
                return ToolOutput::new(format!("error: failed to read {path}: {e}"), "edit_file");
            }
        };

        // Count occurrences.
        let count = content.matches(old_string).count();
        if count == 0 {
            return ToolOutput::new(
                format!("error: old_string not found in {path}"),
                "edit_file",
            );
        }
        if count > 1 && !replace_all {
            return ToolOutput::new(
                format!(
                    "error: old_string found {count} times in {path}; \
                     provide more context to uniquely identify the target, \
                     or set replace_all to true"
                ),
                "edit_file",
            );
        }

        // Replace.
        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            // Replace only the first occurrence.
            let idx = content.find(old_string).unwrap(); // safe: count > 0
            let mut s = String::with_capacity(content.len() - old_string.len() + new_string.len());
            s.push_str(&content[..idx]);
            s.push_str(new_string);
            s.push_str(&content[idx + old_string.len()..]);
            s
        };

        // Atomic write: write to a temp file then rename.
        let tmp = resolved.with_file_name(format!(
            "{}.tmp-{}",
            resolved.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id(),
        ));

        if let Err(e) = fs::write(&tmp, &new_content).await {
            return ToolOutput::new(
                format!("error: failed to write temp file: {e}"),
                "edit_file",
            );
        }
        if let Err(e) = fs::rename(&tmp, &resolved).await {
            // Best-effort cleanup.
            let _ = fs::remove_file(&tmp).await;
            return ToolOutput::new(
                format!("error: failed to rename temp file: {e}"),
                "edit_file",
            );
        }

        ToolOutput::new(
            format!(
                "edited {path} ({count} replacement{})",
                if count == 1 { "" } else { "s" }
            ),
            format!("edit_file {path}"),
        )
    }
}
