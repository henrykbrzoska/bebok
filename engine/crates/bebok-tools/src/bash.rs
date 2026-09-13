use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::time::{Duration, timeout};

use crate::processes::{ProcessRegistry, relative_log_path};
use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Run a shell command and capture its stdout/stderr.
///
/// With `background: true` (F9-14) the command is instead handed to the
/// [`ProcessRegistry`]: spawned detached with its output appended to
/// `.bebok/run/<id>.log`, and the call returns immediately.
pub struct Bash;

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command in the project root and return its combined stdout and stderr. \
         Set background:true for long-running processes (dev servers, watchers): the command is \
         started detached and the call returns with its id/pid/log path; stop it later with \
         bash_kill. With background:true you can also wait for readiness in the same call: \
         ready_port (a TCP port that must start answering) and/or ready_text (a line the log \
         must print), up to ready_timeout ms (default 120000, max 300000) - the call then returns \
         'ready' with the log tail, or the failure reason (process exited, timeout) with the log \
         tail so you can fix it. Prefer this over polling the log yourself. When the command \
         goes through npm (`npm exec`/`npm run`), put server flags after `--` \
         (`npm exec nx serve app -- --port 4317`), otherwise npm swallows them."
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
                },
                "background": {
                    "type": "boolean",
                    "description": "true: start a long-running process (dev server, watcher) detached and return with its id/pid/log path; stop it later with bash_kill.",
                    "default": false
                },
                "ready_port": {
                    "type": "integer",
                    "description": "background only: wait until this local TCP port accepts connections (e.g. the dev server's port) before returning.",
                    "minimum": 1,
                    "maximum": 65535
                },
                "ready_text": {
                    "type": "string",
                    "description": "background only: wait until the process log contains this text (case-insensitive, e.g. 'Application is running' or 'Local:') before returning."
                },
                "ready_timeout": {
                    "type": "integer",
                    "description": "background only: how long to wait for ready_port/ready_text in milliseconds (default 120000, max 300000). A first nx/webpack/vite build can take 60-120 s.",
                    "minimum": 1000,
                    "maximum": 300000
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'command'", "bash");
        };
        if args
            .get("background")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let ready = ReadySpec {
                port: args
                    .get("ready_port")
                    .and_then(Value::as_u64)
                    .filter(|p| (1..=65535).contains(p))
                    .map(|p| p as u16),
                text: args
                    .get("ready_text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string),
                timeout: Duration::from_millis(
                    args.get("ready_timeout")
                        .and_then(Value::as_u64)
                        .unwrap_or(120_000)
                        .clamp(1_000, 300_000),
                ),
            };
            return spawn_background(&ctx, command, ready).await;
        }
        let timeout_ms = args
            .get("timeout")
            .and_then(Value::as_u64)
            .unwrap_or(120_000)
            .clamp(1_000, 600_000);

        // `sh -c` on Unix, `cmd /c` on Windows (there is no `sh` there).
        let (shell, flag) = shell_command();

        let mut child = Command::new(&shell);
        child.arg(flag);
        #[cfg(windows)]
        {
            if flag == "/C" {
                // cmd.exe does not follow the C runtime argument-quoting rules.
                // Escaping an entire shell expression with `.arg()` changes
                // quoted `set "NAME=value"` commands and paths with spaces.
                use std::os::windows::process::CommandExt;
                child.as_std_mut().raw_arg(command);
            } else {
                child.arg(command);
            }
        }
        #[cfg(not(windows))]
        child.arg(command);
        let child = child.current_dir(&ctx.root).kill_on_drop(true).output();

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

/// Readiness conditions for a background command (F9-8): the call blocks
/// until the port answers and/or the log prints the text, the process
/// exits, or the timeout elapses.
#[derive(Debug, Clone, Default)]
pub struct ReadySpec {
    pub port: Option<u16>,
    pub text: Option<String>,
    pub timeout: Duration,
}

impl ReadySpec {
    fn is_set(&self) -> bool {
        self.port.is_some() || self.text.is_some()
    }
}

/// Outcome of waiting for [`ReadySpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadyOutcome {
    /// Every requested condition holds.
    Ready,
    /// The process ended before becoming ready (exit code, if known).
    Exited(Option<i32>),
    /// The timeout elapsed; says which condition is still unmet.
    TimedOut(String),
    /// The turn was aborted while waiting (the process keeps running).
    Aborted,
}

/// Poll the registry until `spec` is satisfied (see [`ReadyOutcome`]).
pub async fn wait_ready(
    id: &str,
    spec: &ReadySpec,
    abort: &tokio_util::sync::CancellationToken,
) -> ReadyOutcome {
    let registry = ProcessRegistry::global();
    let deadline = tokio::time::Instant::now() + spec.timeout;
    let needle = spec.text.as_ref().map(|t| t.to_ascii_lowercase());
    loop {
        let port_ok = match spec.port {
            Some(port) => tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok(),
            None => true,
        };
        let text_ok = match &needle {
            Some(needle) => registry
                .read_log(id, Some(256 * 1024))
                .map(|log| log.to_ascii_lowercase().contains(needle.as_str()))
                .unwrap_or(false),
            None => true,
        };
        if port_ok && text_ok {
            return ReadyOutcome::Ready;
        }
        if let Some(info) = registry.get(id)
            && !info.is_running()
        {
            return ReadyOutcome::Exited(info.exit_code);
        }
        if tokio::time::Instant::now() >= deadline {
            let mut unmet = Vec::new();
            if !port_ok {
                unmet.push(format!("port {} is not answering", spec.port.unwrap_or(0)));
            }
            if !text_ok {
                unmet.push(format!(
                    "log does not contain {:?}",
                    spec.text.clone().unwrap_or_default()
                ));
            }
            return ReadyOutcome::TimedOut(unmet.join("; "));
        }
        tokio::select! {
            _ = abort.cancelled() => return ReadyOutcome::Aborted,
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
    }
}

/// Last `max` bytes of a process log, for the tool text.
fn log_tail(id: &str, max: usize) -> String {
    ProcessRegistry::global()
        .read_log(id, Some(max))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// `background: true`: hand the command to the [`ProcessRegistry`], wait for
/// readiness when asked, and report how to follow / stop it.
async fn spawn_background(ctx: &ToolCtx, command: &str, ready: ReadySpec) -> ToolOutput {
    let info = match ProcessRegistry::global()
        .spawn_background(&ctx.session_id, &ctx.root, command)
        .await
    {
        Ok(info) => info,
        Err(e) => {
            return ToolOutput::new(
                format!("error: failed to start background command: {e}"),
                "bash",
            );
        }
    };
    let log = relative_log_path(&info.id);
    let mut structured = json!({
        "id": info.id,
        "pid": info.pid,
        "log": log,
        "command": command,
    });
    let title = format!("bash (background) {command}");
    if !ready.is_set() {
        let text = format!(
            "started in background: id={} pid={} log={log}\nUse `bash_kill` with the id to stop it; read the log with read_file/tail (or pass ready_port/ready_text to wait for readiness).",
            info.id, info.pid
        );
        return ToolOutput::new(text, title).with_structured(structured);
    }
    let outcome = wait_ready(&info.id, &ready, &ctx.abort).await;
    let tail = log_tail(&info.id, 4 * 1024);
    let (status, headline) = match &outcome {
        ReadyOutcome::Ready => (
            "ready",
            format!(
                "ready: id={} pid={} log={log}{}{}",
                info.id,
                info.pid,
                ready
                    .port
                    .map(|p| format!(" · port {p} answering"))
                    .unwrap_or_default(),
                ready
                    .text
                    .as_ref()
                    .map(|t| format!(" · log contains {t:?}"))
                    .unwrap_or_default(),
            ),
        ),
        ReadyOutcome::Exited(code) => (
            "exited",
            format!(
                "FAILED: the process exited before becoming ready (exit code {}). Read the log below, fix the cause (port in use? missing dependency? build error?) and start it again. id={} log={log}",
                code.map(|c| c.to_string())
                    .unwrap_or_else(|| "unknown".into()),
                info.id
            ),
        ),
        ReadyOutcome::TimedOut(unmet) => (
            "timeout",
            format!(
                "NOT READY after {} s: {unmet}. The process is still running (id={} pid={} log={log}); read the log, and either wait longer with another bash call (ready_port/ready_text) or fix the cause.",
                ready.timeout.as_secs(),
                info.id,
                info.pid
            ),
        ),
        ReadyOutcome::Aborted => (
            "aborted",
            format!(
                "aborted while waiting; process still running (id={} log={log})",
                info.id
            ),
        ),
    };
    structured["ready"] = json!(status);
    let text = if tail.is_empty() {
        format!("{headline}\n(log is still empty)")
    } else {
        format!("{headline}\n--- log tail ---\n{tail}")
    };
    ToolOutput::new(text, title).with_structured(structured)
}

/// The shell + flag used to run a single command on this platform.
///
/// On Unix the shell is resolved from `BEBOK_SHELL` (explicit override, used
/// by the Android/iOS embedding to point at a bundled shell binary), then
/// `$SHELL`, then `sh`. On Windows `BEBOK_SHELL` overrides the default
/// `cmd` shell.
pub(crate) fn shell_command() -> (String, &'static str) {
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

#[cfg(test)]
mod background_tests {
    use super::Bash;
    use crate::processes::ProcessRegistry;
    use crate::tool::{Tool, tool_ctx};
    use tokio_util::sync::CancellationToken;

    /// F9-8: `ready_text` blocks until the log prints the text; a process
    /// that exits before a `ready_port` answers is reported as FAILED with
    /// the log tail instead of a bare "started".
    #[tokio::test]
    async fn background_ready_text_and_early_exit() {
        let root = std::env::temp_dir().join(format!("bebok-bash-ready-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let output = Bash
            .execute(
                tool_ctx(
                    root.clone(),
                    "ready-session".into(),
                    CancellationToken::new(),
                ),
                serde_json::json!({
                    "command": "echo server READY on 4317",
                    "background": true,
                    "ready_text": "ready on",
                    "ready_timeout": 20000,
                }),
            )
            .await;
        assert!(output.text.starts_with("ready: id="), "{}", output.text);
        assert!(
            output.text.contains("log contains \"ready on\""),
            "{}",
            output.text
        );
        assert!(output.text.contains("READY on 4317"), "{}", output.text);
        assert_eq!(output.structured.as_ref().unwrap()["ready"], "ready");

        // A command that ends without ever opening the port -> FAILED + tail.
        let output = Bash
            .execute(
                tool_ctx(
                    root.clone(),
                    "ready-session".into(),
                    CancellationToken::new(),
                ),
                serde_json::json!({
                    "command": "echo boom-no-server",
                    "background": true,
                    "ready_port": 1,
                    "ready_timeout": 20000,
                }),
            )
            .await;
        assert!(
            output.text.starts_with("FAILED: the process exited"),
            "{}",
            output.text
        );
        assert!(output.text.contains("boom-no-server"), "{}", output.text);
        assert_eq!(output.structured.as_ref().unwrap()["ready"], "exited");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn background_returns_id_pid_log_and_creates_the_log_file() {
        let root = std::env::temp_dir().join(format!("bebok-bash-bg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let output = Bash
            .execute(
                tool_ctx(root.clone(), "bg-session".into(), CancellationToken::new()),
                serde_json::json!({"command": "echo background-hi", "background": true}),
            )
            .await;
        assert!(
            output.text.starts_with("started in background: id="),
            "{}",
            output.text
        );
        assert!(output.text.contains("bash_kill"), "{}", output.text);
        let structured = output.structured.expect("structured payload");
        let id = structured["id"].as_str().unwrap().to_string();
        assert!(structured["pid"].as_u64().unwrap() > 0);
        assert_eq!(structured["command"], "echo background-hi");
        let log = structured["log"].as_str().unwrap();
        assert_eq!(log, format!(".bebok/run/{id}.log"));
        assert!(
            output.text.contains(&format!("log={log}")),
            "{}",
            output.text
        );
        assert!(root.join(log).exists(), "log file must exist right away");
        assert!(root.join(".bebok/run/.gitignore").exists());

        let registry = ProcessRegistry::global();
        let info = registry.get(&id).expect("registered");
        assert_eq!(info.session_id, "bg-session");
        // Let it finish, then clean up.
        for _ in 0..100 {
            if !registry.get(&id).unwrap().is_running() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(!registry.get(&id).unwrap().is_running());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn background_false_keeps_the_foreground_path() {
        let output = Bash
            .execute(
                tool_ctx(
                    std::env::temp_dir(),
                    "fg-session".into(),
                    CancellationToken::new(),
                ),
                serde_json::json!({"command": "echo fg-hi", "background": false}),
            )
            .await;
        assert!(output.text.contains("fg-hi"), "{}", output.text);
        assert!(output.structured.is_none());
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

    #[tokio::test]
    async fn cmd_preserves_quoted_set_value() {
        let output = Bash
            .execute(
                tool_ctx(
                    std::env::temp_dir(),
                    "quoted-set-test".into(),
                    CancellationToken::new(),
                ),
                serde_json::json!({"command": "set \"BEBOK_BASH_QUOTED_VALUE=true\" && set BEBOK_BASH_QUOTED_VALUE"}),
            )
            .await;
        assert_eq!(output.text.trim(), "BEBOK_BASH_QUOTED_VALUE=true");
    }
}
