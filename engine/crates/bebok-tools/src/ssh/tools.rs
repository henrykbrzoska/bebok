//! The `ssh` tool: one persistent remote session per host.
//!
//! Four actions, all on one tool so a single permission rule (`ssh(*)`)
//! covers the whole feature:
//!
//! * `connect` — open the session (one `ssh` process, reused afterwards);
//! * `run` — execute a command *in that same shell* (so `cd`, exports and
//!   shell functions survive between calls);
//! * `close` — end it;
//! * `list` — what is open right now (read-only, no prompt).

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ConnectOpts, DEFAULT_RUN_TIMEOUT_MS, SshRegistry};
use crate::tool::{Tool, ToolCtx, ToolOutput};

/// The `ssh` tool.
pub struct SshTool;

#[derive(Debug, Deserialize)]
struct SshArgs {
    action: String,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    identity: Option<String>,
    #[serde(default)]
    strict_host_key: Option<bool>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    sudo: Option<bool>,
    #[serde(default)]
    options: Option<Vec<String>>,
}

impl SshArgs {
    fn trimmed(value: Option<&String>) -> Option<String> {
        value
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }
}

#[async_trait]
impl Tool for SshTool {
    fn name(&self) -> &str {
        "ssh"
    }

    fn description(&self) -> &str {
        "Work on a remote server through ONE persistent SSH session instead of a new \
         `ssh host ...` per command. `action: \"connect\"` logs in once (key/agent auth is used \
         when available; otherwise the user is asked for the password in the UI - never put a \
         password in a tool call) and returns a `session` id. `action: \"run\"` executes a \
         command in that already open remote shell: the working directory, exported variables \
         and shell state persist between calls, so `cd /var/www` once and every later `run` \
         stays there. The result is the combined output plus the exit code and the remote \
         working directory. `action: \"close\"` ends the session; `action: \"list\"` shows the \
         open ones. Reuse the session for follow-up commands (git pull, build, deploy) - do not \
         call `ssh` through `bash` for each step. Commands that need root (`sudo ...`, or \
         `sudo: true`) run as root in the same session: the sudo password is asked in the chat \
         once per session (never put it in a tool call), reused for later commands, and cleared \
         as soon as the host rejects it. Hosts with NOPASSWD sudo are detected at connect, so no \
         prompt appears there."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["connect", "run", "close", "list"],
                    "description": "connect = log in once and keep the shell; run = execute a command in it; close = end the session; list = show open sessions."
                },
                "session": {
                    "type": "string",
                    "description": "Session id returned by connect (run/close). Optional when exactly one session is open in this directory."
                },
                "host": {
                    "type": "string",
                    "description": "connect only: host, user@host, an IPv4/IPv6 address or an alias from ~/.ssh/config (required)."
                },
                "user": {
                    "type": "string",
                    "description": "connect only: SSH user (when host does not already carry user@)."
                },
                "port": {
                    "type": "integer",
                    "description": "connect only: SSH port (default 22 or the one in ~/.ssh/config).",
                    "minimum": 1,
                    "maximum": 65535
                },
                "identity": {
                    "type": "string",
                    "description": "connect only: private key path (identity file). Optional; the ssh agent and ~/.ssh/config are used otherwise."
                },
                "strict_host_key": {
                    "type": "boolean",
                    "description": "connect only: when true, refuse an unknown host key instead of accepting it on first use (default false).",
                    "default": false
                },
                "cwd": {
                    "type": "string",
                    "description": "Remote directory: on connect it is entered once and becomes the session jail (the shell cannot leave it - omit it to work anywhere on the server); on run it changes the directory for this and all following commands."
                },
                "command": {
                    "type": "string",
                    "description": "run only: the command line to execute in the persistent remote shell (required)."
                },
                "timeout": {
                    "type": "integer",
                    "description": "run only: milliseconds to wait for the command (default 120000, max 600000). The session stays open after a timeout.",
                    "minimum": 1000,
                    "maximum": 600000
                },
                "sudo": {
                    "type": "boolean",
                    "description": "run only: run this command as root with sudo. The sudo password is asked in the chat once per session (never put it in a tool call) and reused afterwards; a rejected password is cleared so the next sudo asks again. Detected automatically when the command contains `sudo`.",
                    "default": false
                },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "connect only: extra arguments for ssh itself, exactly as on the command line. A bare `Key=Value` becomes `-o Key=Value`, so [\"PreferredAuthentications=password\"] forces a password login on a host that also accepts keys, and [\"-o\", \"ProxyJump=bastion\"] adds a jump host. Values win over the built-in defaults."
                }
            },
            "required": ["action"]
        })
    }

    /// `list` and `close` only touch state the agent itself created; running
    /// commands on a server (and opening a connection) is a session-level
    /// side effect and must be asked for.
    fn is_read_only_for(&self, args: &Value) -> bool {
        matches!(
            args.get("action").and_then(Value::as_str),
            Some("list") | Some("close")
        )
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let parsed: SshArgs = match serde_json::from_value(args) {
            Ok(parsed) => parsed,
            Err(e) => return ToolOutput::new(format!("error: {e}"), "ssh"),
        };
        let registry = SshRegistry::global();
        match parsed.action.trim() {
            "connect" => connect(&ctx, registry, parsed).await,
            "run" => run(&ctx, registry, parsed).await,
            "close" => close(registry, parsed),
            "list" => list(&ctx, registry),
            other => ToolOutput::new(
                format!("error: unknown action '{other}'; use connect, run, close or list"),
                "ssh",
            ),
        }
    }
}

async fn connect(ctx: &ToolCtx, registry: &SshRegistry, args: SshArgs) -> ToolOutput {
    let Some(host) = SshArgs::trimmed(args.host.as_ref()) else {
        return ToolOutput::new("error: missing required parameter 'host'", "ssh");
    };
    let directory = ctx.root.to_string_lossy().to_string();
    let opts = ConnectOpts {
        host: host.clone(),
        user: SshArgs::trimmed(args.user.as_ref()),
        port: args.port,
        identity: SshArgs::trimmed(args.identity.as_ref()),
        strict_host_key: args.strict_host_key.unwrap_or(false),
        cwd: SshArgs::trimmed(args.cwd.as_ref()),
        options: args
            .options
            .unwrap_or_default()
            .into_iter()
            .map(|option| option.trim().to_string())
            .filter(|option| !option.is_empty())
            .collect(),
    };
    match registry.connect(&directory, &ctx.session_id, opts).await {
        Ok(session) => {
            let info = session.info();
            ToolOutput::new(
                format!(
                    "connected to {} — session {} ({})\n\
                     This is ONE persistent shell: `run` commands with\n\
                     session: \"{}\" and the working directory, exports and shell state stay put\n\
                     between calls. Close it with action: \"close\" when you are done.\n\
                     remote cwd: {}",
                    info.label, info.id, info.status, info.id, info.cwd
                ),
                format!("ssh connect {}", info.label),
            )
            .with_structured(json!({ "session": info }))
        }
        Err(e) => ToolOutput::new(format!("error: {e}"), format!("ssh connect {host}")),
    }
}

async fn run(ctx: &ToolCtx, registry: &SshRegistry, args: SshArgs) -> ToolOutput {
    let Some(command) = SshArgs::trimmed(args.command.as_ref()) else {
        return ToolOutput::new("error: missing required parameter 'command'", "ssh");
    };
    let session = match resolve_session(ctx, registry, args.session.as_ref()) {
        Ok(session) => session,
        Err(message) => return ToolOutput::new(message, "ssh"),
    };
    let timeout_ms = args.timeout.unwrap_or(DEFAULT_RUN_TIMEOUT_MS);
    let cwd = SshArgs::trimmed(args.cwd.as_ref());
    let label = format!("ssh {} · {}", session.label, first_line(&command));

    match session
        .run(
            &command,
            cwd.as_deref(),
            timeout_ms,
            args.sudo.unwrap_or(false),
            &ctx.abort,
        )
        .await
    {
        Ok(result) => {
            let mut text = String::new();
            if result.output.trim().is_empty() {
                text.push_str("(no output)");
            } else {
                text.push_str(&result.output);
            }
            if result.truncated {
                text.push_str("\n\n[output truncated - narrow the command]");
            }
            text.push_str(&format!(
                "\n\n[exit code {} · remote cwd {} · session {}]",
                result.exit_code,
                result.cwd,
                session.id()
            ));
            ToolOutput::new(text, label).with_structured(json!({
                "session": session.info(),
                "exitCode": result.exit_code,
                "cwd": result.cwd,
            }))
        }
        Err(e) => ToolOutput::new(format!("error: {e}"), label)
            .with_structured(json!({ "session": session.info() })),
    }
}

fn close(registry: &SshRegistry, args: SshArgs) -> ToolOutput {
    let Some(id) = SshArgs::trimmed(args.session.as_ref()) else {
        return ToolOutput::new(
            "error: missing required parameter 'session' (see action: \"list\")",
            "ssh close",
        );
    };
    let label = registry
        .get(&id)
        .map(|s| s.label.clone())
        .unwrap_or_else(|| id.clone());
    if registry.close(&id) {
        ToolOutput::new(format!("closed SSH session {id} ({label})"), "ssh close")
            .with_structured(json!({ "closed": id }))
    } else {
        ToolOutput::new(format!("error: no open SSH session {id}"), "ssh close")
    }
}

fn list(ctx: &ToolCtx, registry: &SshRegistry) -> ToolOutput {
    let directory = ctx.root.to_string_lossy().to_string();
    let sessions = registry.for_directory(&directory);
    if sessions.is_empty() {
        return ToolOutput::new(
            "no open SSH sessions in this project; open one with action: \"connect\", host: \"...\"",
            "ssh list",
        )
        .with_structured(json!({ "sessions": [] }));
    }
    let mut text = String::new();
    let infos: Vec<Value> = sessions
        .iter()
        .map(|session| {
            let info = session.info();
            text.push_str(&format!(
                "- {} · {} · {} · {} commands · started {}\n",
                info.id, info.label, info.cwd, info.commands, info.status
            ));
            serde_json::to_value(&info).unwrap_or(Value::Null)
        })
        .collect();
    ToolOutput::new(text.trim_end().to_string(), "ssh list")
        .with_structured(json!({ "sessions": infos }))
}

/// Resolve which session a `run`/`close` targets.
fn resolve_session(
    ctx: &ToolCtx,
    registry: &SshRegistry,
    requested: Option<&String>,
) -> Result<std::sync::Arc<super::SshSession>, String> {
    if let Some(id) = SshArgs::trimmed(requested) {
        return registry
            .get(&id)
            .ok_or_else(|| format!("error: no open SSH session {id} (see action: \"list\")"));
    }
    // Ergonomics: an agent that just connected should not have to remember an
    // id it never printed. Unambiguous only.
    let directory = ctx.root.to_string_lossy().to_string();
    let live: Vec<_> = registry
        .for_directory(&directory)
        .into_iter()
        .filter(|s| s.dead_reason().is_none())
        .collect();
    match live.len() {
        0 => Err(
            "error: no open SSH session in this project; call action: \"connect\" first"
                .to_string(),
        ),
        1 => Ok(live[0].clone()),
        _ => Err(format!(
            "error: {} SSH sessions are open; pass `session` (see action: \"list\")",
            live.len()
        )),
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    let mut shortened: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        shortened.push('…');
    }
    shortened
}
