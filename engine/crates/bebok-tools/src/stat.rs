use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Show metadata for a file or directory (type, size, mtime, permissions).
///
/// Portable replacement for `stat` / `ls -l` via the `bash` tool.
pub struct Stat;

#[async_trait]
impl Tool for Stat {
    fn name(&self) -> &str {
        "stat"
    }

    fn description(&self) -> &str {
        "Show metadata (type, size, modified time, read-only) for a file or directory. Portable alternative to `stat`/`ls -l` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to inspect, relative to the project root. Defaults to '.'."
                }
            },
            "required": []
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let full = ctx.root.join(path);

        let meta = match tokio::fs::symlink_metadata(&full).await {
            Ok(m) => m,
            Err(e) => {
                return ToolOutput::new(
                    format!("error: cannot stat {path}: {e}"),
                    format!("stat {path}"),
                );
            }
        };

        let kind = if meta.file_type().is_symlink() {
            "symlink"
        } else if meta.is_dir() {
            "directory"
        } else if meta.is_file() {
            "file"
        } else {
            "other"
        };

        // For symlinks, also report the resolved target kind when it exists.
        let target_note = if meta.file_type().is_symlink() {
            match tokio::fs::metadata(&full).await {
                Ok(t) => format!(" -> {}", if t.is_dir() { "directory" } else { "file" }),
                Err(_) => " -> (broken)".to_string(),
            }
        } else {
            String::new()
        };

        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let mut out = String::new();
        out.push_str(&format!("path:      {path}\n"));
        out.push_str(&format!("type:      {kind}{target_note}\n"));
        out.push_str(&format!("size:      {} bytes\n", meta.len()));
        out.push_str(&format!("modified:  {modified}\n"));
        if modified != "unknown" {
            if let Ok(m) = modified.parse::<u64>() {
                out.push_str(&format!("age:       {}s ago\n", now.saturating_sub(m)));
            }
        }
        out.push_str(&format!("readonly:  {}\n", meta.permissions().readonly()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            out.push_str(&format!("mode:      {:o}\n", meta.permissions().mode() & 0o7777));
        }

        ToolOutput::new(out.trim_end().to_string(), format!("stat {path}"))
    }
}
