use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Locate an executable on `PATH`.
///
/// Portable replacement for `which` (Unix) / `where` (Windows) via the `bash`
/// tool. Use it to check what is installed before running a build or test
/// command.
pub struct Which;

#[async_trait]
impl Tool for Which {
    fn name(&self) -> &str {
        "which"
    }

    fn description(&self) -> &str {
        "Locate an executable on PATH (e.g. `cargo`, `node`, `docker`). Portable alternative to `which`/`where` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Executable name to look up (or an explicit path)."
                }
            },
            "required": ["command"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'command'", "which");
        };
        let title = format!("which {command}");

        // An explicit path (absolute or containing a separator) is checked
        // directly rather than searched on PATH.
        if command.contains('/') || command.contains('\\') {
            let direct = ctx.root.join(command);
            if is_executable(&direct) {
                return ToolOutput::new(direct.to_string_lossy().to_string(), title);
            }
            return ToolOutput::new(
                format!("(not found) {command} is not an executable"),
                title,
            );
        }

        let mut found: Vec<String> = Vec::new();
        for dir in path_dirs() {
            for candidate in candidates(&dir, command) {
                if is_executable(&candidate) {
                    found.push(candidate.to_string_lossy().to_string());
                }
            }
        }

        if found.is_empty() {
            ToolOutput::new(format!("(not found) {command}"), title)
        } else {
            ToolOutput::new(found.join("\n"), title)
        }
    }
}

/// Directories from `PATH` (split with the platform separator by std).
fn path_dirs() -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(&path)
        .filter(|p| !p.as_os_str().is_empty())
        .collect()
}

/// Candidate file names for `name` in `dir` (Windows tries each `PATHEXT`).
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let mut out = vec![dir.join(name)];
        if Path::new(name).extension().is_none() {
            let exts =
                std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
            for ext in exts.split(';').filter(|e| !e.is_empty()) {
                out.push(dir.join(format!("{name}{ext}")));
                out.push(dir.join(format!("{name}{}", ext.to_lowercase())));
            }
        }
        out
    }

    #[cfg(not(windows))]
    {
        vec![dir.join(name)]
    }
}

/// Whether `path` points at an executable file.
fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}
