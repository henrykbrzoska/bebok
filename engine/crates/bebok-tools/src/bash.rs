use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::time::{Duration, timeout};

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
                },
                "timeout": {
                    "type": "integer",
                    "description": "Maximum execution time in milliseconds (default 120000, maximum 600000).",
                    "minimum": 1000,
                    "maximum": 600000
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'command'", "bash");
        };
        let timeout_ms = args
            .get("timeout")
            .and_then(Value::as_u64)
            .unwrap_or(120_000)
            .clamp(1_000, 600_000);

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
            out = timeout(Duration::from_millis(timeout_ms), child) => match out {
                Ok(out) => out,
                Err(_) => return ToolOutput::new(
                    format!("command timed out after {timeout_ms} ms"),
                    "bash",
                ),
            },
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
                    text = "(exit code 0)".to_string();
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
/// `$SHELL`, then `sh`. On Windows `BEBOK_SHELL` overrides the default
/// `cmd` shell.
fn shell_command() -> (String, &'static str) {
    #[cfg(windows)]
    {
        let shell = std::env::var("BEBOK_SHELL").unwrap_or_else(|_| "cmd".to_string());
        let flag = if shell.rsplit(['\\', '/']).next().is_some_and(|name| {
            name.eq_ignore_ascii_case("cmd") || name.eq_ignore_ascii_case("cmd.exe")
        }) {
            "/C"
        } else {
            "-Command"
        };
        (shell, flag)
    }

    #[cfg(not(windows))]
    {
        let shell = std::env::var("BEBOK_SHELL")
            .or_else(|_| std::env::var("SHELL"))
            .unwrap_or_else(|_| "sh".to_string());
        (shell, "-c")
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::{Bash, shell_command};
    use crate::tool::{Tool, tool_ctx};
    use std::sync::{Mutex, OnceLock};
    use tokio_util::sync::CancellationToken;

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    #[test]
    fn windows_shell_override_selects_pwsh() {
        let _lock = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let prior = std::env::var_os("BEBOK_SHELL");
        unsafe { std::env::set_var("BEBOK_SHELL", "pwsh") };
        assert_eq!(shell_command(), ("pwsh".to_string(), "-Command"));
        match prior {
            Some(value) => unsafe { std::env::set_var("BEBOK_SHELL", value) },
            None => unsafe { std::env::remove_var("BEBOK_SHELL") },
        }
    }

    #[tokio::test]
    async fn command_timeout_is_enforced() {
        let command = if cfg!(windows) {
            "ping -n 5 127.0.0.1 >NUL"
        } else {
            "sleep 4"
        };
        let output = Bash
            .execute(
                tool_ctx(
                    std::env::temp_dir(),
                    "timeout-test".into(),
                    CancellationToken::new(),
                ),
                serde_json::json!({"command": command, "timeout": 1000}),
            )
            .await;
        assert!(
            output.text.contains("timed out after 1000 ms"),
            "{}",
            output.text
        );
    }
}
