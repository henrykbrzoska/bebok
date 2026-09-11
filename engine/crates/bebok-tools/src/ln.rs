use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Create a symbolic or hard link.
///
/// Portable replacement for `ln`/`ln -s` via the `bash` tool. Symbolic links on
/// Windows may require Developer Mode or admin rights; when they fail this
/// falls back to a hard link for files.
pub struct Ln;

#[async_trait]
impl Tool for Ln {
    fn name(&self) -> &str {
        "ln"
    }

    fn description(&self) -> &str {
        "Create a link. By default a symbolic link (like `ln -s`); set hard=true for a hard link. `target` is the existing path, `link` is the new path."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "description": "Existing file or directory the link points to, relative to the project root."
                },
                "link": {
                    "type": "string",
                    "description": "New link path to create, relative to the project root."
                },
                "hard": {
                    "type": "boolean",
                    "description": "If true create a hard link instead of a symbolic link. Defaults to false."
                }
            },
            "required": ["target", "link"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(target) = args.get("target").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'target'", "ln");
        };
        let Some(link) = args.get("link").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'link'", "ln");
        };
        let hard = args.get("hard").and_then(|v| v.as_bool()).unwrap_or(false);

        let target_abs: PathBuf = ctx.root.join(target.trim());
        let link_abs: PathBuf = ctx.root.join(link.trim());
        let title = format!("ln {target} {link}");

        // Create the link's parent directory when missing.
        if let Some(parent) = link_abs.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolOutput::new(format!("error: cannot create parent dir: {e}"), title);
            }
        }

        let t = target_abs.clone();
        let l = link_abs.clone();
        let result = tokio::task::spawn_blocking(move || -> std::io::Result<&'static str> {
            if hard {
                std::fs::hard_link(&t, &l)?;
                return Ok("hard");
            }
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&t, &l)?;
                Ok("symbolic")
            }
            #[cfg(windows)]
            {
                let is_dir = t.is_dir();
                let r = if is_dir {
                    std::os::windows::fs::symlink_dir(&t, &l)
                } else {
                    std::os::windows::fs::symlink_file(&t, &l)
                };
                match r {
                    Ok(()) => Ok("symbolic"),
                    // No symlink privilege: fall back to a hard link for files.
                    Err(_) if !is_dir => {
                        std::fs::hard_link(&t, &l)?;
                        Ok("hard (symlink not permitted)")
                    }
                    Err(e) => Err(e),
                }
            }
            #[cfg(not(any(unix, windows)))]
            {
                std::fs::hard_link(&t, &l)?;
                Ok("hard")
            }
        })
        .await;

        match result {
            Ok(Ok(kind)) => ToolOutput::new(format!("created {kind} link {link} -> {target}"), title),
            Ok(Err(e)) => ToolOutput::new(format!("error: failed to link {link} -> {target}: {e}"), title),
            Err(e) => ToolOutput::new(format!("error: link task failed: {e}"), title),
        }
    }
}
