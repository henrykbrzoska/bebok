use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Change file permissions.
///
/// Portable-ish replacement for `chmod` via the `bash` tool. On Unix the `mode`
/// is the usual octal string (`644`, `755`); on Windows only the read-only bit
/// is meaningful, so a mode with no write bit clears it and any mode with a
/// write bit sets the file writable.
pub struct Chmod;

#[async_trait]
impl Tool for Chmod {
    fn name(&self) -> &str {
        "chmod"
    }

    fn description(&self) -> &str {
        "Change a file's permissions. On Unix `mode` is octal (e.g. \"755\"); on Windows only the read-only bit is applied. Portable alternative to `chmod` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File or directory, relative to the project root."
                },
                "mode": {
                    "type": "string",
                    "description": "Octal permission mode, e.g. \"644\" or \"755\"."
                }
            },
            "required": ["path", "mode"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'path'", "chmod");
        };
        let Some(mode) = args.get("mode").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'mode'", "chmod");
        };

        let mode = mode.trim().trim_start_matches("0o");
        let Ok(bits) = u32::from_str_radix(mode, 8) else {
            return ToolOutput::new(
                format!("error: '{mode}' is not a valid octal mode (try \"644\" or \"755\")"),
                "chmod",
            );
        };
        if bits > 0o7777 {
            return ToolOutput::new(format!("error: mode '{mode}' is out of range"), "chmod");
        }

        let full = ctx.root.join(path);
        let title = format!("chmod {mode} {path}");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(bits);
            match tokio::fs::set_permissions(&full, perms).await {
                Ok(()) => ToolOutput::new(format!("set {path} to mode {mode}"), title),
                Err(e) => ToolOutput::new(format!("error: failed to chmod {path}: {e}"), title),
            }
        }

        #[cfg(not(unix))]
        {
            // Without POSIX modes only the write bit is expressible.
            let writable = bits & 0o200 != 0;
            let mut perms = match tokio::fs::metadata(&full).await {
                Ok(m) => m.permissions(),
                Err(e) => {
                    return ToolOutput::new(format!("error: cannot access {path}: {e}"), title);
                }
            };
            perms.set_readonly(!writable);
            match tokio::fs::set_permissions(&full, perms).await {
                Ok(()) => ToolOutput::new(format!("set {path} read-only = {}", !writable), title),
                Err(e) => ToolOutput::new(format!("error: failed to chmod {path}: {e}"), title),
            }
        }
    }
}
