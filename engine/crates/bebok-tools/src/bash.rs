use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Run a shell command and capture its stdout/stderr.
pub struct Bash;

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command in the project root and return its combined stdout and stderr."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute (passed to `sh -c` on Unix, `cmd /c` on Windows)."
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'command'", "bash");
        };

        // `sh -c` on Unix, `cmd /c` on Windows (there is no `sh` there).
        let (shell, flag) = shell_command();

        let child = Command::new(&shell)
            .arg(flag)
            .arg(command)
            .current_dir(&ctx.root)
            .kill_on_drop(true)
            .output();

        let output = tokio::select! {
            _ = ctx.abort.cancelled() => {
                return ToolOutput::new("aborted", "bash");
            }
            out = child => out,
        };

        match output {
            Ok(out) => {
                let mut text = String::new();
                if !out.stdout.is_empty() {
                    text.push_str(&String::from_utf8_lossy(&out.stdout));
                }
                if !out.stderr.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&String::from_utf8_lossy(&out.stderr));
                }
                // Surface a non-zero exit clearly so the model reacts to the
                // failure (the full output is fed back into context either way).
                if !out.status.success() {
                    let code = out.status.code().unwrap_or(-1);
                    text = format!("command failed (exit code {code}):\n{text}");
                } else if text.is_empty() {
                    text = format!("(exit code 0)");
                }
                ToolOutput::new(text, format!("bash {command}"))
            }
            Err(e) => ToolOutput::new(format!("error: failed to run command: {e}"), "bash"),
        }
    }
}

/// The shell + flag used to run a single command on this platform.
///
/// On Unix the shell is resolved from `BEBOK_SHELL` (explicit override, used
/// by the Android/iOS embedding to point at a bundled shell binary), then
/// `$SHELL`, then `sh`. On Windows it is always `cmd /C`.
fn shell_command() -> (String, &'static str) {
    #[cfg(windows)]
    {
        ("cmd".to_string(), "/C")
    }

    #[cfg(not(windows))]
    {
        let shell = std::env::var("BEBOK_SHELL")
            .or_else(|_| std::env::var("SHELL"))
            .unwrap_or_else(|_| "sh".to_string());
        (shell, "-c")
    }
}
