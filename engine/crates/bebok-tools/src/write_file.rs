use async_trait::async_trait;
use serde_json::{Value, json};

use crate::pathguard::resolve_in_root;
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
            None => {
                return ToolOutput::new("error: missing required parameter 'path'", "write_file");
            }
        };
        let content = match args.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => {
                return ToolOutput::new(
                    "error: missing required parameter 'content'",
                    "write_file",
                );
            }
        };

        let full = match resolve_in_root(&ctx.root, path) {
            Ok(full) => full,
            Err(e) => return ToolOutput::new(format!("error: {e}"), "write_file"),
        };
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WriteFile;
    use crate::tool::{Tool, tool_ctx};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn rejects_paths_outside_root_without_writing() {
        let base = std::env::temp_dir().join(format!("bebok-write-{}", uuid::Uuid::new_v4()));
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let outside = base.join("secret.txt");
        for path in [
            "../../secret",
            r"..\secret.txt",
            "/tmp/secret",
            r"C:\secret",
            outside.to_str().unwrap(),
        ] {
            let out = WriteFile
                .execute(
                    tool_ctx(root.clone(), String::new(), CancellationToken::new()),
                    json!({ "path": path, "content": "leak" }),
                )
                .await;
            assert!(out.text.starts_with("error: "), "{path}: {}", out.text);
        }
        assert!(!outside.exists());
        std::fs::remove_dir_all(base).unwrap();
    }
}
