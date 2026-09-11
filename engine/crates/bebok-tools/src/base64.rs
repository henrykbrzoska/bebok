use async_trait::async_trait;
use ::base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Base64-encode or decode data.
///
/// Portable replacement for `base64` via the `bash` tool. Reads from `path`
/// (binary) or `text`, and optionally writes the result to `out`.
pub struct Base64;

#[async_trait]
impl Tool for Base64 {
    fn name(&self) -> &str {
        "base64"
    }

    fn description(&self) -> &str {
        "Base64-encode or decode. Input from `path` (file) or `text`; optional `out` writes the result to a file. Portable alternative to `base64` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["encode", "decode"],
                    "description": "encode (default) or decode."
                },
                "path": {
                    "type": "string",
                    "description": "File to read from, relative to the project root."
                },
                "text": {
                    "type": "string",
                    "description": "Inline text to operate on when `path` is omitted."
                },
                "out": {
                    "type": "string",
                    "description": "Optional file to write the result to (relative to the project root)."
                }
            },
            "required": []
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn is_read_only_for(&self, args: &Value) -> bool {
        // Pure transform unless the caller writes an output file.
        args.get("out").and_then(|v| v.as_str()).is_none()
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("encode");
        let title = format!("base64 {action}");

        // Gather the raw input bytes.
        let input: Vec<u8> = if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
            match tokio::fs::read(ctx.root.join(path.trim())).await {
                Ok(b) => b,
                Err(e) => {
                    return ToolOutput::new(format!("error: failed to read {path}: {e}"), title);
                }
            }
        } else if let Some(text) = args.get("text").and_then(|v| v.as_str()) {
            text.as_bytes().to_vec()
        } else {
            return ToolOutput::new("error: provide either 'path' or 'text'", title);
        };

        // Produce the output bytes + a human-readable rendering.
        let (out_bytes, rendered) = match action {
            "encode" => {
                let encoded = STANDARD.encode(&input);
                let shown = if encoded.len() > 4096 {
                    format!("{}... ({} chars total)", &encoded[..4096], encoded.len())
                } else {
                    encoded.clone()
                };
                (encoded.into_bytes(), shown)
            }
            "decode" => {
                let text = String::from_utf8_lossy(&input);
                let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
                match STANDARD.decode(cleaned.as_bytes()) {
                    Ok(decoded) => {
                        let shown = match String::from_utf8(decoded.clone()) {
                            Ok(s) if s.chars().all(|c| !c.is_control() || c == '\n' || c == '\t' || c == '\r') => s,
                            _ => format!("({} decoded bytes; binary/not UTF-8)", decoded.len()),
                        };
                        (decoded, shown)
                    }
                    Err(e) => return ToolOutput::new(format!("error: invalid base64: {e}"), title),
                }
            }
            other => {
                return ToolOutput::new(
                    format!("error: action must be 'encode' or 'decode', got '{other}'"),
                    title,
                );
            }
        };

        if let Some(out) = args.get("out").and_then(|v| v.as_str()) {
            let full = ctx.root.join(out.trim());
            if let Some(parent) = full.parent() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    return ToolOutput::new(format!("error: cannot create parent dir: {e}"), title);
                }
            }
            if let Err(e) = tokio::fs::write(&full, &out_bytes).await {
                return ToolOutput::new(format!("error: failed to write {out}: {e}"), title);
            }
            return ToolOutput::new(
                format!("wrote {} bytes to {out}\n{rendered}", out_bytes.len()),
                title,
            );
        }

        ToolOutput::new(rendered, title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    async fn run(args: Value) -> ToolOutput {
        let ctx = ToolCtx {
            root: std::env::temp_dir(),
            session_id: "base64-test".to_string(),
            abort: CancellationToken::new(),
        };
        Base64.execute(ctx, args).await
    }

    #[tokio::test]
    async fn encodes_and_decodes_round_trip() {
        let enc = run(json!({ "action": "encode", "text": "hello" })).await;
        assert_eq!(enc.text, "aGVsbG8=");
        let dec = run(json!({ "action": "decode", "text": "aGVsbG8=" })).await;
        assert_eq!(dec.text, "hello");
    }

    #[tokio::test]
    async fn rejects_invalid_base64() {
        let out = run(json!({ "action": "decode", "text": "!!!not-base64!!!" })).await;
        assert!(out.text.starts_with("error: invalid base64"), "{}", out.text);
    }
}
