use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Write (create or overwrite) a text file on disk.
pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Create or overwrite a UTF-8 text file with the given content. Parent directories are created as needed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, relative to the project root."
                },
                "content": {
                    "type": "string",
                    "description": "Full contents to write."
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolOutput::new("error: missing required parameter 'path'", "write_file"),
        };
        let content = match args.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => {
                return ToolOutput::new(
                    "error: missing required parameter 'content'",
                    "write_file",
                )
            }
        };

        let full = ctx.root.join(path);
        let parent = full.parent().unwrap_or_else(|| std::path::Path::new("."));
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return ToolOutput::new(
                format!("error: failed to create parent dir: {e}"),
                "write_file",
            );
        }

        match tokio::fs::write(&full, content).await {
            Ok(()) => ToolOutput::new(
                format!("wrote {} ({} bytes)", path, content.len()),
                "write_file",
            ),
            Err(e) => ToolOutput::new(format!("error: failed to write {path}: {e}"), "write_file"),
        }    }
}
