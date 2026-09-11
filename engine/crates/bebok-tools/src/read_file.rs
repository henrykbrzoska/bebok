use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Read a UTF-8 text file from disk, optionally a line range.
pub struct ReadFile;

impl ReadFile {
    /// Slice `offset`/`limit` out of `text`.
    ///
    /// `offset` is 1-based (line 1 = start of file); `limit` counts lines.
    /// Returns the sliced text plus a trailing marker describing the window
    /// and how to see more, so the model never has to shell out to `head`,
    /// `sed -n` or a throw-away script just to read a fragment.
    fn slice(text: &str, offset: usize, limit: Option<usize>) -> String {
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        let start = offset.max(1);

        if total == 0 {
            return "[read_file: empty file]".to_string();
        }
        if start > total {
            return format!("[read_file: file has {total} lines; offset {start} is past the end]");
        }

        let end = match limit {
            Some(l) if l > 0 => start.saturating_add(l - 1).min(total),
            _ => total,
        };

        let mut out = lines[start - 1..end].join("\n");
        if start != 1 || end != total {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("[read_file: lines {start}-{end} of {total}"));
            if end < total {
                out.push_str(&format!("; {} more, pass offset={}", total - end, end + 1));
            }
            out.push(']');
        }
        out
    }
}

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a UTF-8 text file from disk and return its contents. Use `offset`/`limit` to read a fragment of a large file instead of running `head`/`sed`/`node`."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, relative to the project root."
                },
                "offset": {
                    "type": "integer",
                    "description": "1-based line number to start reading from. Omit to read from the first line."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to return. Omit to read to the end of the file."
                }
            },
            "required": ["path"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'path'", "read_file");
        };
        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .filter(|v| *v > 0)
            .map(|v| v as usize);

        let full = ctx.root.join(path);
        match tokio::fs::read_to_string(&full).await {
            Ok(text) => {
                // A plain whole-file read returns the file verbatim (no
                // marker), so nothing downstream has to un-decorate it.
                let out = if offset <= 1 && limit.is_none() {
                    text
                } else {
                    Self::slice(&text, offset, limit)
                };
                let title = match (offset, limit) {
                    (1, None) => format!("read_file {path}"),
                    (o, Some(l)) => format!("read_file {path} (offset {o}, limit {l})"),
                    (o, None) => format!("read_file {path} (offset {o})"),
                };
                ToolOutput::new(out, title)
            }
            Err(e) => ToolOutput::new(
                format!("error: failed to read {path}: {e}"),
                "read_file",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ReadFile;

    const DOC: &str = "one\ntwo\nthree\nfour\nfive";

    #[test]
    fn whole_file_is_verbatim() {
        assert_eq!(ReadFile::slice(DOC, 1, None), DOC);
    }

    #[test]
    fn offset_and_limit_window() {
        assert_eq!(
            ReadFile::slice(DOC, 2, Some(2)),
            "two\nthree\n[read_file: lines 2-3 of 5; 2 more, pass offset=4]"
        );
        assert_eq!(
            ReadFile::slice(DOC, 4, None),
            "four\nfive\n[read_file: lines 4-5 of 5]"
        );
    }

    #[test]
    fn past_the_end_is_reported_not_panicking() {
        assert_eq!(
            ReadFile::slice(DOC, 99, Some(3)),
            "[read_file: file has 5 lines; offset 99 is past the end]"
        );
        assert_eq!(ReadFile::slice("", 1, None), "[read_file: empty file]");
    }
}
