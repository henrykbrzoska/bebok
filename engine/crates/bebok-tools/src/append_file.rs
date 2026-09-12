use async_trait::async_trait;
use serde_json::{Value, json};

use crate::pathguard::resolve_in_root;
use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Append text to a file (creating it if needed) without rewriting the rest.
///
/// Portable replacement for `>>` redirection via the `bash` tool. Useful for
/// growing logs/manifests where a full `write_file` would mean re-sending the
/// entire content through the model.
pub struct AppendFile;

#[async_trait]
impl Tool for AppendFile {
    fn name(&self) -> &str {
        "append_file"
    }

    fn description(&self) -> &str {
        "Append text to a file, creating it (and parents) if needed. The existing content is preserved. Portable alternative to `>>` via bash."
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
                    "description": "Text to append."
                },
                "newline": {
                    "type": "boolean",
                    "description": "Append a trailing newline after the content. Defaults to true."
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'path'", "append_file");
        };
        let Some(content) = args.get("content").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'content'", "append_file");
        };
        let newline = args
            .get("newline")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let full = match resolve_in_root(&ctx.root, path) {
            Ok(full) => full,
            Err(e) => return ToolOutput::new(format!("error: {e}"), "append_file"),
        };
        if let Some(parent) = full.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return ToolOutput::new(
                format!("error: failed to create parent dir: {e}"),
                "append_file",
            );
        }

        let mut payload = content.to_string();
        if newline && !payload.ends_with('\n') {
            payload.push('\n');
        }

        match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&full)
            .await
        {
            Ok(mut file) => {
                use tokio::io::AsyncWriteExt;
                match file.write_all(payload.as_bytes()).await {
                    Ok(()) => ToolOutput::new(
                        format!("appended {} bytes to {path}", payload.len()),
                        format!("append_file {path}"),
                    ),
                    Err(e) => ToolOutput::new(
                        format!("error: failed to append to {path}: {e}"),
                        "append_file",
                    ),
                }
            }
            Err(e) => ToolOutput::new(format!("error: failed to open {path}: {e}"), "append_file"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AppendFile;
    use crate::tool::{Tool, tool_ctx};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn rejects_paths_outside_root_without_appending() {
        let base = std::env::temp_dir().join(format!("bebok-append-{}", uuid::Uuid::new_v4()));
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let outside = base.join("secret.txt");
        std::fs::write(&outside, "original").unwrap();
        for path in [
            "../../secret",
            r"..\secret.txt",
            "/tmp/secret",
            r"C:\secret",
            outside.to_str().unwrap(),
        ] {
            let out = AppendFile
                .execute(
                    tool_ctx(root.clone(), String::new(), CancellationToken::new()),
                    json!({ "path": path, "content": "leak" }),
                )
                .await;
            assert!(out.text.starts_with("error: "), "{path}: {}", out.text);
        }
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "original");
        std::fs::remove_dir_all(base).unwrap();
    }
}
