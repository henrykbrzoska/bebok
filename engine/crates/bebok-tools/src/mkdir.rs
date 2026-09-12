use async_trait::async_trait;
use serde_json::{Value, json};

use crate::pathguard::resolve_in_root;
use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Create a directory (and any missing parents).
///
/// Portable replacement for `mkdir -p` via the `bash` tool, so the same call
/// works on Windows (`mkdir`/`md`), macOS and Linux.
pub struct Mkdir;

#[async_trait]
impl Tool for Mkdir {
    fn name(&self) -> &str {
        "mkdir"
    }

    fn description(&self) -> &str {
        "Create a directory (and any missing parents). Portable alternative to `mkdir -p` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to create, relative to the project root."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'path'", "mkdir");
        };

        let full = match resolve_in_root(&ctx.root, path) {
            Ok(full) => full,
            Err(e) => return ToolOutput::new(format!("error: {e}"), "mkdir"),
        };
        match tokio::fs::create_dir_all(&full).await {
            Ok(()) => ToolOutput::new(format!("created directory {path}"), format!("mkdir {path}")),
            Err(e) => ToolOutput::new(format!("error: failed to create {path}: {e}"), "mkdir"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Mkdir;
    use crate::tool::{Tool, tool_ctx};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn rejects_paths_outside_root_without_creating_directories() {
        let base = std::env::temp_dir().join(format!("bebok-mkdir-{}", uuid::Uuid::new_v4()));
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let outside = base.join("new-dir");
        for path in [
            "../../secret",
            r"..\new-dir",
            "/tmp/secret",
            r"C:\secret",
            outside.to_str().unwrap(),
        ] {
            let out = Mkdir
                .execute(
                    tool_ctx(root.clone(), String::new(), CancellationToken::new()),
                    json!({ "path": path }),
                )
                .await;
            assert!(out.text.starts_with("error: "), "{path}: {}", out.text);
        }
        assert!(!outside.exists());
        std::fs::remove_dir_all(base).unwrap();
    }
}
