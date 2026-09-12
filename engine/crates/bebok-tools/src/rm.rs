use async_trait::async_trait;
use serde_json::{Value, json};

use crate::pathguard::resolve_in_root;
use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Remove a file or directory.
///
/// Portable replacement for `rm` / `rm -rf` via the `bash` tool. Without
/// `recursive` a directory is refused (like plain `rm`); with it the whole
/// tree is removed.
pub struct Rm;

#[async_trait]
impl Tool for Rm {
    fn name(&self) -> &str {
        "rm"
    }

    fn description(&self) -> &str {
        "Remove a file or directory. Portable alternative to `rm` / `rm -rf` via bash. Directories need `recursive: true`."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File or directory to remove, relative to the project root."
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Remove a directory and all its contents. Defaults to false (plain files only)."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'path'", "rm");
        };
        let recursive = args
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let full = match resolve_in_root(&ctx.root, path) {
            Ok(full) => full,
            Err(e) => return ToolOutput::new(format!("error: {e}"), "rm"),
        };
        let meta = match tokio::fs::symlink_metadata(&full).await {
            Ok(m) => m,
            Err(e) => {
                return ToolOutput::new(format!("error: cannot access {path}: {e}"), "rm");
            }
        };

        let result = if meta.is_dir() {
            if !recursive {
                return ToolOutput::new(
                    format!("error: {path} is a directory; pass recursive=true to remove it"),
                    "rm",
                );
            }
            tokio::fs::remove_dir_all(&full).await
        } else {
            tokio::fs::remove_file(&full).await
        };

        match result {
            Ok(()) => ToolOutput::new(format!("removed {path}"), format!("rm {path}")),
            Err(e) => ToolOutput::new(format!("error: failed to remove {path}: {e}"), "rm"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Rm;
    use crate::tool::{Tool, tool_ctx};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn rejects_paths_outside_root_without_removing() {
        let base = std::env::temp_dir().join(format!("bebok-rm-{}", uuid::Uuid::new_v4()));
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
            let out = Rm
                .execute(
                    tool_ctx(root.clone(), String::new(), CancellationToken::new()),
                    json!({ "path": path, "recursive": true }),
                )
                .await;
            assert!(out.text.starts_with("error: "), "{path}: {}", out.text);
        }
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "original");
        std::fs::remove_dir_all(base).unwrap();
    }
}
