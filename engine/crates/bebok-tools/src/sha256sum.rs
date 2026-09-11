use sha2::{Digest, Sha256};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// SHA-256 digest of a file or inline text.
///
/// Portable replacement for `sha256sum` via the `bash` tool. Streams files in
/// chunks so large files do not need to be loaded whole.
pub struct Sha256Sum;

#[async_trait]
impl Tool for Sha256Sum {
    fn name(&self) -> &str {
        "sha256sum"
    }

    fn description(&self) -> &str {
        "Compute the SHA-256 hex digest of a file (`path`) or inline `text`. Portable alternative to `sha256sum` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File to hash, relative to the project root."
                },
                "text": {
                    "type": "string",
                    "description": "Inline text to hash when `path` is omitted."
                }
            },
            "required": []
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
            let full = ctx.root.join(path.trim());
            let title = format!("sha256sum {path}");
            let result = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
                use std::io::Read;
                let mut file = std::fs::File::open(&full)?;
                let mut hasher = Sha256::new();
                let mut buf = [0u8; 64 * 1024];
                loop {
                    let n = file.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                }
                Ok(format!("{:x}", hasher.finalize()))
            })
            .await;
            return match result {
                Ok(Ok(hex)) => ToolOutput::new(format!("{hex}  {path}"), title),
                Ok(Err(e)) => ToolOutput::new(format!("error: failed to read {path}: {e}"), title),
                Err(e) => ToolOutput::new(format!("error: hash task failed: {e}"), title),
            };
        }

        if let Some(text) = args.get("text").and_then(|v| v.as_str()) {
            let mut hasher = Sha256::new();
            hasher.update(text.as_bytes());
            return ToolOutput::new(format!("{:x}", hasher.finalize()), "sha256sum");
        }

        ToolOutput::new("error: provide either 'path' or 'text'", "sha256sum")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn hashes_inline_text() {
        let ctx = ToolCtx {
            root: std::env::temp_dir(),
            session_id: "sha-test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = Sha256Sum.execute(ctx, json!({ "text": "abc" })).await;
        assert_eq!(
            out.text,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
