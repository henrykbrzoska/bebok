use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Gzip-compress or decompress a single file.
///
/// Portable replacement for `gzip`/`gunzip` via the `bash` tool. Defaults the
/// output name to `<path>.gz` (compress) or the name with `.gz` stripped
/// (decompress); the input is kept (unlike `gzip -d` which removes it).
pub struct Gzip;

#[async_trait]
impl Tool for Gzip {
    fn name(&self) -> &str {
        "gzip"
    }

    fn description(&self) -> &str {
        "Gzip-compress (`action: compress`) or decompress (`action: decompress`) a file. Output defaults to `<path>.gz` or the path with `.gz` removed. Portable alternative to gzip/gunzip via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["compress", "decompress"],
                    "description": "compress (default) or decompress."
                },
                "path": {
                    "type": "string",
                    "description": "File to process, relative to the project root."
                },
                "out": {
                    "type": "string",
                    "description": "Optional output path (relative to the project root)."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'path'", "gzip");
        };
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("compress");
        if action != "compress" && action != "decompress" {
            return ToolOutput::new(
                format!("error: action must be 'compress' or 'decompress', got '{action}'"),
                "gzip",
            );
        }

        let input = ctx.root.join(path.trim());
        let output: PathBuf = match args.get("out").and_then(|v| v.as_str()) {
            Some(out) => ctx.root.join(out.trim()),
            None if action == "compress" => {
                let mut s = input.clone().into_os_string();
                s.push(".gz");
                PathBuf::from(s)
            }
            None => {
                let text = input.to_string_lossy();
                match text.strip_suffix(".gz") {
                    Some(base) => PathBuf::from(base),
                    None => input.with_extension("out"),
                }
            }
        };

        let title = format!("gzip {action} {path}");
        let out_display = relative(&ctx.root, &output);
        let compress = action == "compress";
        let in_path = input.clone();
        let out_path = output.clone();
        let result = tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let src = std::fs::File::open(&in_path)?;
            let dst = std::fs::File::create(&out_path)?;
            let mut reader = BufReader::new(src);
            let mut writer = BufWriter::new(dst);
            if compress {
                let mut enc = GzEncoder::new(&mut writer, Compression::default());
                std::io::copy(&mut reader, &mut enc)?;
                enc.finish()?;
            } else {
                let mut dec = GzDecoder::new(&mut reader);
                std::io::copy(&mut dec, &mut writer)?;
            }
            writer.into_inner().map_err(|e| e.into_error())?;
            Ok(std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0))
        })
        .await;

        match result {
            Ok(Ok(bytes)) => {
                ToolOutput::new(format!("wrote {bytes} bytes to {out_display}"), title)
            }
            Ok(Err(e)) => ToolOutput::new(format!("error: gzip {action} failed: {e}"), title),
            Err(e) => ToolOutput::new(format!("error: gzip task failed: {e}"), title),
        }
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn compress_then_decompress_round_trips() {
        let base = std::env::temp_dir().join(format!("bebok-gzip-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&base).await.unwrap();
        tokio::fs::write(base.join("a.txt"), b"hello gzip world")
            .await
            .unwrap();
        let ctx = ToolCtx {
            root: base.clone(),
            session_id: "gzip-test".to_string(),
            abort: CancellationToken::new(),
        };

        let c = Gzip
            .execute(
                ctx.clone(),
                json!({ "action": "compress", "path": "a.txt" }),
            )
            .await;
        assert!(c.text.starts_with("wrote"), "{}", c.text);
        assert!(base.join("a.txt.gz").exists());

        let d = Gzip
            .execute(
                ctx,
                json!({ "action": "decompress", "path": "a.txt.gz", "out": "a.out" }),
            )
            .await;
        assert!(d.text.starts_with("wrote"), "{}", d.text);
        assert_eq!(
            tokio::fs::read_to_string(base.join("a.out")).await.unwrap(),
            "hello gzip world"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
