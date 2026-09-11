use std::path::Path;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Copy a file or directory.
///
/// Portable replacement for `cp` / `cp -r` via the `bash` tool.
pub struct Cp;

#[async_trait]
impl Tool for Cp {
    fn name(&self) -> &str {
        "cp"
    }

    fn description(&self) -> &str {
        "Copy a file or directory (recursively). Portable alternative to `cp` / `cp -r` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from": {
                    "type": "string",
                    "description": "Source file or directory, relative to the project root."
                },
                "to": {
                    "type": "string",
                    "description": "Destination path, relative to the project root."
                }
            },
            "required": ["from", "to"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let (Some(from), Some(to)) = (
            args.get("from").and_then(|v| v.as_str()),
            args.get("to").and_then(|v| v.as_str()),
        ) else {
            return ToolOutput::new(
                "error: missing required parameters 'from' and 'to'",
                "cp",
            );
        };

        let src = ctx.root.join(from);
        let dst = ctx.root.join(to);

        if tokio::fs::symlink_metadata(&src).await.is_err() {
            return ToolOutput::new(format!("error: source not found: {from}"), "cp");
        }

        match copy_path(&src, &dst).await {
            Ok(()) => ToolOutput::new(format!("copied {from} -> {to}"), format!("cp {from} {to}")),
            Err(e) => ToolOutput::new(format!("error: failed to copy {from} -> {to}: {e}"), "cp"),
        }
    }
}

/// Recursively copy `src` to `dst` (file or directory tree).
fn copy_path<'a>(
    src: &'a Path,
    dst: &'a Path,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>> + Send + 'a>> {
    Box::pin(async move {
        let meta = tokio::fs::symlink_metadata(src).await?;
        if meta.is_dir() {
            tokio::fs::create_dir_all(dst).await?;
            let mut entries = tokio::fs::read_dir(src).await?;
            while let Some(entry) = entries.next_entry().await? {
                let child_src = entry.path();
                let child_dst = dst.join(entry.file_name());
                copy_path(&child_src, &child_dst).await?;
            }
            Ok(())
        } else {
            if let Some(parent) = dst.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::copy(src, dst).await?;
            Ok(())
        }
    })
}
