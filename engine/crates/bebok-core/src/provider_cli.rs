//! CLI agents as providers (1.8): Claude Code, Codex, Cursor agent, Grok,
//! Antigravity (`agy`) and Gemini CLI driven headlessly, so a subscription
//! login on this machine replaces an API key.
//!
//! The CLI *is* the agent: it runs its own tools in the session directory
//! under its own permission mode, and the engine only streams what it
//! reports - text and thinking deltas, tool calls as closed
//! [`StreamEvent::ToolActivity`] parts, usage at the end. The CLI's own
//! conversation id is remembered per engine session (`<data dir>/cli-sessions/`)
//! so the next message resumes it, which is what keeps the CLI's context and
//! prompt cache warm. Aborting the turn kills the child process.
//!
//! Analysis with every CLI's headless contract:
//! `~/projects/ai/analyses/2026-09-15-bebok-cli-providers.md`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use bebok_llm::{
    ChatRequest, ChatRole, LlmError, Provider, ProviderSpec, StreamEvent, StreamResult,
    ToolActivity, Usage,
};
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// The CLIs the engine knows how to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CliAgent {
    Claude,
    Codex,
    Cursor,
    Grok,
    Agy,
    Gemini,
}

/// How much the CLI may do on its own. Maps onto each CLI's own flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CliPermission {
    /// Read-only / planning: no edits, no commands.
    Plan,
    /// Edits in the workspace are auto-approved; the rest is the CLI's default.
    #[default]
    Edits,
    /// Everything auto-approved (the CLI's "yolo" / bypass mode).
    All,
}

impl CliAgent {
    pub const ALL: [CliAgent; 6] = [
        CliAgent::Claude,
        CliAgent::Codex,
        CliAgent::Cursor,
        CliAgent::Grok,
        CliAgent::Agy,
        CliAgent::Gemini,
    ];

    pub fn id(self) -> &'static str {
        match self {
            CliAgent::Claude => "claude",
            CliAgent::Codex => "codex",
            CliAgent::Cursor => "cursor",
            CliAgent::Grok => "grok",
            CliAgent::Agy => "agy",
            CliAgent::Gemini => "gemini",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|a| a.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            CliAgent::Claude => "Claude Code",
            CliAgent::Codex => "Codex (OpenAI)",
            CliAgent::Cursor => "Cursor agent",
            CliAgent::Grok => "Grok Build (xAI)",
            CliAgent::Agy => "Antigravity (Google)",
            CliAgent::Gemini => "Gemini CLI",
        }
    }

    /// Default binary name on PATH.
    pub fn command(self) -> &'static str {
        match self {
            CliAgent::Claude => "claude",
            CliAgent::Codex => "codex",
            CliAgent::Cursor => "cursor-agent",
            CliAgent::Grok => "grok",
            CliAgent::Agy => "agy",
            CliAgent::Gemini => "gemini",
        }
    }

    /// Provider name in the config / model prefix (`cli-claude/sonnet`).
    pub fn provider_name(self) -> String {
        format!("cli-{}", self.id())
    }

    /// Models offered by default (the CLI's own aliases; editable in Settings).
    pub fn default_models(self) -> Vec<&'static str> {
        match self {
            CliAgent::Claude => vec!["default", "opus", "sonnet", "haiku"],
            CliAgent::Codex => vec!["default", "gpt-5.5", "gpt-5.5-mini"],
            CliAgent::Cursor => vec!["default", "auto", "sonnet-4.6", "gpt-5.5"],
            CliAgent::Grok => vec!["default", "grok-4.6"],
            CliAgent::Agy => vec!["default", "gemini-3.1-pro-high", "gemini-3.8-flash-high"],
            CliAgent::Gemini => vec!["default", "gemini-2.5-pro", "gemini-2.5-flash"],
        }
    }

    /// `<cmd> --version` output, or None when the binary is not runnable.
    pub async fn probe(self, command: Option<&str>) -> CliProbe {
        let cmd = command.unwrap_or(self.command()).to_string();
        let path = which(&cmd);
        let mut probe = CliProbe {
            agent: self,
            id: self.id().to_string(),
            label: self.label().to_string(),
            command: cmd.clone(),
            path: path.clone(),
            installed: false,
            version: None,
            provider: self.provider_name(),
            default_models: self
                .default_models()
                .iter()
                .map(|m| m.to_string())
                .collect(),
        };
        if path.is_none() {
            return probe;
        }
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            Command::new(&cmd)
                .arg(match self {
                    CliAgent::Agy => "--version",
                    _ => "--version",
                })
                .stdin(Stdio::null())
                .output(),
        )
        .await;
        if let Ok(Ok(out)) = out {
            probe.installed = out.status.success();
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let text = if text.is_empty() {
                String::from_utf8_lossy(&out.stderr).trim().to_string()
            } else {
                text
            };
            probe.version = text.lines().next().map(|l| l.chars().take(80).collect());
        }
        probe
    }
}

/// `GET /providers/cli` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliProbe {
    pub agent: CliAgent,
    pub id: String,
    pub label: String,
    pub command: String,
    pub path: Option<String>,
    pub installed: bool,
    pub version: Option<String>,
    pub provider: String,
    pub default_models: Vec<String>,
}

/// Locate a command on PATH (plus the usual per-user install dirs, which a
/// GUI-launched engine's PATH often lacks).
pub fn which(cmd: &str) -> Option<String> {
    if cmd.contains('/') || cmd.contains('\\') {
        return Path::new(cmd).is_file().then(|| cmd.to_string());
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        for extra in [".local/bin", ".grok/bin", ".cargo/bin", ".bun/bin", "bin"] {
            dirs.push(home.join(extra));
        }
        if let Ok(entries) = std::fs::read_dir(home.join(".nvm/versions/node")) {
            for e in entries.flatten() {
                dirs.push(e.path().join("bin"));
            }
        }
    }
    dirs.extend(
        ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"]
            .iter()
            .map(PathBuf::from),
    );
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{cmd}.exe"), format!("{cmd}.cmd"), cmd.to_string()]
    } else {
        vec![cmd.to_string()]
    };
    for dir in dirs {
        for name in &names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().to_string());
            }
        }
    }
    None
}

/// Settings of one CLI provider, read from `ProviderSpec::extra`.
#[derive(Debug, Clone)]
pub struct CliSettings {
    pub agent: CliAgent,
    pub command: String,
    pub permission: CliPermission,
}

impl CliSettings {
    pub fn from_spec(spec: &ProviderSpec) -> Result<Self, String> {
        let agent_id = spec
            .extra
            .get("cli")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| spec.name.strip_prefix("cli-").map(str::to_string))
            .ok_or_else(|| format!("provider '{}': missing `cli` (which agent CLI)", spec.name))?;
        let agent = CliAgent::parse(&agent_id)
            .ok_or_else(|| format!("provider '{}': unknown CLI agent '{agent_id}'", spec.name))?;
        let command = spec
            .extra
            .get("command")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| agent.command().to_string());
        let permission = spec
            .extra
            .get("permission")
            .and_then(|v| v.as_str())
            .and_then(|s| match s {
                "plan" => Some(CliPermission::Plan),
                "edits" => Some(CliPermission::Edits),
                "all" => Some(CliPermission::All),
                _ => None,
            })
            .unwrap_or_default();
        Ok(Self {
            agent,
            command,
            permission,
        })
    }
}

/// Where the CLI conversation id of an engine session is remembered.
fn session_map_path(data_dir: &Path, agent: CliAgent, session_id: &str) -> PathBuf {
    data_dir
        .join("cli-sessions")
        .join(format!("{}-{}", agent.id(), session_id))
}

pub fn load_cli_session(data_dir: &Path, agent: CliAgent, session_id: &str) -> Option<String> {
    std::fs::read_to_string(session_map_path(data_dir, agent, session_id))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn save_cli_session(data_dir: &Path, agent: CliAgent, session_id: &str, cli_session: &str) {
    let path = session_map_path(data_dir, agent, session_id);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, cli_session);
}

/// The provider: one per configured CLI provider spec.
pub struct CliProvider {
    name: String,
    settings: CliSettings,
    data_dir: PathBuf,
}

impl CliProvider {
    pub fn new(spec: &ProviderSpec, data_dir: PathBuf) -> Result<Self, String> {
        Ok(Self {
            name: spec.name.clone(),
            settings: CliSettings::from_spec(spec)?,
            data_dir,
        })
    }

    /// The message the CLI gets: the last user message. Everything before it
    /// lives in the CLI's own resumed conversation; on a fresh CLI
    /// conversation with earlier engine history (a provider switch mid-chat)
    /// the prior turns are replayed as a transcript so nothing is lost.
    fn prompt_for(req: &ChatRequest, resumed: bool) -> String {
        let last_user = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == ChatRole::User && !m.content.trim().is_empty())
            .map(|m| m.content.clone())
            .unwrap_or_default();
        if resumed || req.messages.len() <= 1 {
            return last_user;
        }
        let mut transcript = String::from(
            "Earlier conversation (from another model, for context; reply only to the last message):\n\n",
        );
        for m in req.messages.iter().take(req.messages.len() - 1) {
            let who = match m.role {
                ChatRole::User => "User",
                ChatRole::Assistant => "Assistant",
            };
            let text = m.content.trim();
            if !text.is_empty() {
                transcript.push_str(&format!("{who}: {text}\n\n"));
            }
        }
        transcript.push_str(&format!("---\n\n{last_user}"));
        transcript
    }
}

fn model_arg(model: &str) -> Option<&str> {
    let bare = model.rsplit('/').next().unwrap_or(model);
    (!bare.is_empty() && bare != "default").then_some(bare)
}

/// Build the command line for `agent`. `resume` is the CLI's own conversation
/// id from a previous turn of this engine session.
pub fn build_args(
    settings: &CliSettings,
    model: &str,
    prompt: &str,
    resume: Option<&str>,
    system: &str,
    directory: Option<&str>,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let model = model_arg(model);
    match settings.agent {
        CliAgent::Claude => {
            args.extend(
                [
                    "-p",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--include-partial-messages",
                ]
                .map(String::from),
            );
            match settings.permission {
                CliPermission::Plan => args.extend(["--permission-mode", "plan"].map(String::from)),
                CliPermission::Edits => {
                    args.extend(["--permission-mode", "acceptEdits"].map(String::from))
                }
                CliPermission::All => args.push("--dangerously-skip-permissions".into()),
            }
            if let Some(m) = model {
                args.extend(["--model", m].map(String::from));
            }
            if let Some(id) = resume {
                args.extend(["--resume", id].map(String::from));
            } else if !system.trim().is_empty() {
                args.extend(["--append-system-prompt".to_string(), system.to_string()]);
            }
            args.push(prompt.to_string());
        }
        CliAgent::Codex => {
            // `exec` options come before the `resume` subcommand.
            args.extend(["exec", "--json", "--skip-git-repo-check"].map(String::from));
            if let Some(dir) = directory {
                args.extend(["-C".to_string(), dir.to_string()]);
            }
            match settings.permission {
                CliPermission::Plan => args.extend(["-s", "read-only"].map(String::from)),
                CliPermission::Edits => args.extend(["-s", "workspace-write"].map(String::from)),
                CliPermission::All => {
                    args.push("--dangerously-bypass-approvals-and-sandbox".into())
                }
            }
            if let Some(m) = model {
                args.extend(["-m", m].map(String::from));
            }
            if let Some(id) = resume {
                args.extend(["resume", id].map(String::from));
            }
            args.push(prompt.to_string());
        }
        CliAgent::Cursor => {
            args.extend(
                [
                    "-p",
                    "--output-format",
                    "stream-json",
                    "--stream-partial-output",
                    "--trust",
                ]
                .map(String::from),
            );
            if let Some(dir) = directory {
                args.extend(["--workspace".to_string(), dir.to_string()]);
            }
            match settings.permission {
                CliPermission::Plan => args.extend(["--mode", "plan"].map(String::from)),
                CliPermission::Edits => {}
                CliPermission::All => args.push("--force".into()),
            }
            if let Some(m) = model {
                args.extend(["--model", m].map(String::from));
            }
            if let Some(id) = resume {
                args.extend(["--resume", id].map(String::from));
            }
            args.push(prompt.to_string());
        }
        CliAgent::Grok => {
            args.extend(
                [
                    "-p",
                    prompt,
                    "--output-format",
                    "streaming-messages-json",
                    "--include-partial-messages",
                ]
                .map(String::from),
            );
            if let Some(dir) = directory {
                args.extend(["--cwd".to_string(), dir.to_string()]);
            }
            match settings.permission {
                CliPermission::Plan => args.extend(["--permission-mode", "plan"].map(String::from)),
                CliPermission::Edits => {
                    args.extend(["--permission-mode", "acceptEdits"].map(String::from))
                }
                CliPermission::All => args.push("--always-approve".into()),
            }
            if let Some(m) = model {
                args.extend(["--model", m].map(String::from));
            }
            if let Some(id) = resume {
                args.extend(["--resume", id].map(String::from));
            }
        }
        CliAgent::Agy => {
            args.extend(["--print", prompt, "--output-format", "stream-json"].map(String::from));
            match settings.permission {
                CliPermission::Plan => args.extend(["--mode", "plan"].map(String::from)),
                CliPermission::Edits => args.extend(["--mode", "accept-edits"].map(String::from)),
                CliPermission::All => args.push("--dangerously-skip-permissions".into()),
            }
            if let Some(m) = model {
                args.extend(["--model", m].map(String::from));
            }
            if let Some(id) = resume {
                args.extend(["--conversation", id].map(String::from));
            }
        }
        CliAgent::Gemini => {
            args.extend(["-p", prompt, "-o", "stream-json"].map(String::from));
            match settings.permission {
                CliPermission::Plan => args.extend(["--approval-mode", "plan"].map(String::from)),
                CliPermission::Edits => {
                    args.extend(["--approval-mode", "auto_edit"].map(String::from))
                }
                CliPermission::All => args.extend(["--approval-mode", "yolo"].map(String::from)),
            }
            if let Some(m) = model {
                args.extend(["-m", m].map(String::from));
            }
            if let Some(id) = resume {
                args.extend(["--resume", id].map(String::from));
            }
        }
    }
    args
}

/// Grok wraps tool results as `{"type":"Bash","output":[bytes…]}` (or an
/// `output` string); other CLIs hand back plain text. Normalise to text.
fn tool_result_text(raw: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return raw.to_string();
    };
    match v.get("output") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) if items.iter().all(|i| i.is_u64()) => {
            let bytes: Vec<u8> = items
                .iter()
                .filter_map(|i| i.as_u64())
                .map(|b| b as u8)
                .collect();
            String::from_utf8_lossy(&bytes).to_string()
        }
        _ => raw.to_string(),
    }
}

/// Incremental parser state for one CLI run.
#[derive(Default)]
pub struct CliParser {
    pub agent: Option<CliAgent>,
    pub session_id: Option<String>,
    /// Claude/Cursor/Grok: text already delivered through deltas, so the
    /// final `assistant` message is not appended a second time.
    streamed_text: String,
    /// Same for thinking: once deltas streamed it, the final block is skipped.
    streamed_thinking: bool,
    /// Codex/agy: text emitted so far for the same reason.
    emitted_text: String,
    pub usage: Usage,
    pub error: Option<String>,
    pub finished: bool,
}

impl CliParser {
    pub fn new(agent: CliAgent) -> Self {
        Self {
            agent: Some(agent),
            ..Default::default()
        }
    }

    /// Parse one NDJSON line into stream events.
    pub fn line(&mut self, line: &str) -> Vec<StreamEvent> {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            return Vec::new();
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        match self.agent.unwrap_or(CliAgent::Claude) {
            CliAgent::Claude | CliAgent::Cursor | CliAgent::Grok => self.anthropic_shaped(&v),
            CliAgent::Codex => self.codex(&v),
            CliAgent::Agy => self.agy(&v),
            CliAgent::Gemini => self.gemini(&v),
        }
    }

    fn take_usage(&mut self, u: &Value) {
        let n = |k: &str| u.get(k).and_then(|x| x.as_u64());
        self.usage.input_tokens = n("input_tokens")
            .or(n("inputTokens"))
            .unwrap_or(self.usage.input_tokens);
        self.usage.output_tokens = n("output_tokens")
            .or(n("outputTokens"))
            .unwrap_or(self.usage.output_tokens);
        self.usage.cache_read_input_tokens = n("cache_read_input_tokens")
            .or(n("cached_input_tokens"))
            .or(n("cacheReadTokens"))
            .or(n("cache_read_tokens"))
            .or(self.usage.cache_read_input_tokens);
        self.usage.cache_creation_input_tokens = n("cache_creation_input_tokens")
            .or(n("cache_write_input_tokens"))
            .or(n("cacheWriteTokens"))
            .or(self.usage.cache_creation_input_tokens);
    }

    /// Claude Code / Cursor agent / Grok (`streaming-messages-json`): the
    /// Anthropic message shape with `system`/`assistant`/`user`/`result`
    /// envelopes plus optional `stream_event` deltas.
    fn anthropic_shaped(&mut self, v: &Value) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        if let Some(id) = v.get("session_id").and_then(|s| s.as_str()) {
            self.session_id = Some(id.to_string());
        }
        match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "stream_event" => {
                let ev = v.get("event").unwrap_or(&Value::Null);
                if ev.get("type").and_then(|t| t.as_str()) == Some("content_block_delta") {
                    let delta = ev.get("delta").unwrap_or(&Value::Null);
                    match delta.get("type").and_then(|t| t.as_str()) {
                        Some("text_delta") => {
                            if let Some(t) = delta.get("text").and_then(|t| t.as_str()) {
                                self.streamed_text.push_str(t);
                                out.push(StreamEvent::Text(t.to_string()));
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(t) = delta.get("thinking").and_then(|t| t.as_str()) {
                                self.streamed_thinking = true;
                                out.push(StreamEvent::Thinking(t.to_string()));
                            }
                        }
                        _ => {}
                    }
                }
            }
            "assistant" => {
                // Cursor's `--stream-partial-output` delivers deltas as
                // `assistant` messages stamped with `timestamp_ms`; the final,
                // unstamped message repeats the whole text.
                let partial = v.get("timestamp_ms").is_some();
                let content = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default();
                for block in content {
                    match block.get("type").and_then(|t| t.as_str()) {
                        Some("text") if partial => {
                            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                self.streamed_text.push_str(t);
                                out.push(StreamEvent::Text(t.to_string()));
                            }
                        }
                        Some("text") => {
                            let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                            // Deltas already delivered this text (Claude with
                            // --include-partial-messages); only emit what is new.
                            if !text.is_empty() {
                                if let Some(rest) = text.strip_prefix(self.streamed_text.as_str()) {
                                    if !rest.is_empty() {
                                        out.push(StreamEvent::Text(rest.to_string()));
                                    }
                                } else if !self.streamed_text.ends_with(text) {
                                    out.push(StreamEvent::Text(text.to_string()));
                                }
                                self.streamed_text.clear();
                            }
                        }
                        Some("thinking") => {
                            // Whole thinking block: only for CLIs that did not
                            // already stream it as deltas.
                            if !self.streamed_thinking
                                && let Some(t) = block.get("thinking").and_then(|t| t.as_str())
                            {
                                out.push(StreamEvent::Thinking(t.to_string()));
                            }
                        }
                        Some("tool_use") => {
                            out.push(StreamEvent::ToolActivity(ToolActivity {
                                id: block
                                    .get("id")
                                    .and_then(|i| i.as_str())
                                    .unwrap_or("tool")
                                    .to_string(),
                                name: block
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("tool")
                                    .to_string(),
                                input: block.get("input").cloned().unwrap_or(Value::Null),
                                output: None,
                                is_error: false,
                            }));
                        }
                        _ => {}
                    }
                }
            }
            "user" => {
                self.streamed_thinking = false;
                let content = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default();
                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                        let text = match block.get("content") {
                            Some(Value::String(s)) => tool_result_text(s),
                            Some(Value::Array(parts)) => parts
                                .iter()
                                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join("\n"),
                            _ => String::new(),
                        };
                        out.push(StreamEvent::ToolActivity(ToolActivity {
                            id: block
                                .get("tool_use_id")
                                .and_then(|i| i.as_str())
                                .unwrap_or("tool")
                                .to_string(),
                            name: String::new(),
                            input: Value::Null,
                            output: Some(text),
                            is_error: block
                                .get("is_error")
                                .and_then(|e| e.as_bool())
                                .unwrap_or(false),
                        }));
                    }
                }
            }
            "result" => {
                self.streamed_thinking = false;
                if let Some(u) = v.get("usage") {
                    self.take_usage(u);
                }
                if let Some(cost) = v.get("total_cost_usd").and_then(|c| c.as_f64()) {
                    self.usage.cost = Some(cost);
                }
                let is_error = v.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false)
                    || v.get("subtype")
                        .and_then(|s| s.as_str())
                        .is_some_and(|s| s.starts_with("error"));
                if is_error {
                    self.error = Some(
                        v.get("result")
                            .and_then(|r| r.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| "the CLI reported an error".to_string()),
                    );
                }
                self.finished = true;
            }
            _ => {}
        }
        out
    }

    /// Codex `exec --json`: `thread.started`, `item.started/completed`, `turn.completed/failed`.
    fn codex(&mut self, v: &Value) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "thread.started" => {
                if let Some(id) = v.get("thread_id").and_then(|s| s.as_str()) {
                    self.session_id = Some(id.to_string());
                }
            }
            "item.started" | "item.completed" | "item.updated" => {
                let item = v.get("item").unwrap_or(&Value::Null);
                let id = item
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("item")
                    .to_string();
                let done = v.get("type").and_then(|t| t.as_str()) == Some("item.completed");
                match item.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "agent_message" if done => {
                        if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                            self.emitted_text.push_str(t);
                            out.push(StreamEvent::Text(t.to_string()));
                        }
                    }
                    "reasoning" if done => {
                        if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                            out.push(StreamEvent::Thinking(t.to_string()));
                        }
                    }
                    "command_execution" => {
                        let command = item.get("command").and_then(|c| c.as_str()).unwrap_or("");
                        let output = done.then(|| {
                            let text = item
                                .get("aggregated_output")
                                .and_then(|o| o.as_str())
                                .unwrap_or("")
                                .to_string();
                            let code = item.get("exit_code").and_then(|c| c.as_i64());
                            match code {
                                Some(c) if c != 0 => format!("{text}\n(exit code {c})"),
                                _ => text,
                            }
                        });
                        out.push(StreamEvent::ToolActivity(ToolActivity {
                            id,
                            name: "bash".into(),
                            input: serde_json::json!({ "command": command }),
                            is_error: done
                                && item
                                    .get("exit_code")
                                    .and_then(|c| c.as_i64())
                                    .is_some_and(|c| c != 0),
                            output,
                        }));
                    }
                    "file_change" => {
                        let changes = item.get("changes").cloned().unwrap_or(Value::Null);
                        out.push(StreamEvent::ToolActivity(ToolActivity {
                            id,
                            name: "edit_file".into(),
                            input: serde_json::json!({ "changes": changes }),
                            output: done.then(|| "applied".to_string()),
                            is_error: false,
                        }));
                    }
                    "mcp_tool_call" => {
                        out.push(StreamEvent::ToolActivity(ToolActivity {
                            id,
                            name: item
                                .get("tool")
                                .and_then(|t| t.as_str())
                                .unwrap_or("mcp")
                                .to_string(),
                            input: item.get("arguments").cloned().unwrap_or(Value::Null),
                            output: done.then(|| {
                                item.get("result")
                                    .map(|r| r.to_string())
                                    .unwrap_or_default()
                            }),
                            is_error: false,
                        }));
                    }
                    "web_search" => {
                        out.push(StreamEvent::ToolActivity(ToolActivity {
                            id,
                            name: "web_search".into(),
                            input: serde_json::json!({ "query": item.get("query").cloned().unwrap_or(Value::Null) }),
                            output: done.then(String::new),
                            is_error: false,
                        }));
                    }
                    "error" => {
                        if let Some(m) = item.get("message").and_then(|m| m.as_str()) {
                            out.push(StreamEvent::Text(format!("\n> {m}\n")));
                        }
                    }
                    _ => {}
                }
            }
            "turn.completed" => {
                if let Some(u) = v.get("usage") {
                    self.take_usage(u);
                }
                self.finished = true;
            }
            "turn.failed" | "error" => {
                let msg = v
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .or_else(|| v.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("codex turn failed")
                    .to_string();
                self.error = Some(msg);
                self.finished = true;
            }
            _ => {}
        }
        out
    }

    /// Antigravity `agy --output-format stream-json`: `init`, `step_update`, `result`.
    fn agy(&mut self, v: &Value) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        match v.get("event").and_then(|e| e.as_str()).unwrap_or("") {
            "init" => {
                if let Some(id) = v.get("conversation_id").and_then(|s| s.as_str()) {
                    self.session_id = Some(id.to_string());
                }
            }
            "step_update" => {
                let step = v.get("step_update").unwrap_or(&Value::Null);
                if let Some(id) = step.get("conversation_id").and_then(|s| s.as_str()) {
                    self.session_id = Some(id.to_string());
                }
                let state = step.get("state").and_then(|s| s.as_str()).unwrap_or("");
                match step.get("step_type").and_then(|t| t.as_str()).unwrap_or("") {
                    "agent_response" => {
                        if let Some(t) = step.get("text_delta").and_then(|t| t.as_str()) {
                            self.emitted_text.push_str(t);
                            out.push(StreamEvent::Text(t.to_string()));
                        }
                        if let Some(u) = step.get("usage") {
                            let n = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
                            self.usage.input_tokens += n("input_tokens");
                            self.usage.output_tokens += n("output_tokens");
                        }
                    }
                    "tool" => {
                        let info = step.get("tool_info").unwrap_or(&Value::Null);
                        let idx = step.get("step_index").and_then(|i| i.as_u64()).unwrap_or(0);
                        let id = format!("agy-step-{idx}");
                        let name = step
                            .get("tool_name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("tool")
                            .to_string();
                        let input = info.get("parameters").cloned().unwrap_or(Value::Null);
                        let (output, is_error) = match state {
                            "DONE" => (
                                Some(
                                    info.get("result")
                                        .map(|r| r.to_string())
                                        .unwrap_or_default(),
                                ),
                                false,
                            ),
                            "ERROR" => (
                                Some(
                                    info.get("error")
                                        .and_then(|e| e.get("message"))
                                        .and_then(|m| m.as_str())
                                        .unwrap_or("tool error")
                                        .to_string(),
                                ),
                                true,
                            ),
                            _ => (None, false),
                        };
                        out.push(StreamEvent::ToolActivity(ToolActivity {
                            id,
                            name,
                            input,
                            output,
                            is_error,
                        }));
                    }
                    _ => {}
                }
            }
            "result" => {
                let r = v.get("result").unwrap_or(&Value::Null);
                if r.get("status")
                    .and_then(|s| s.as_str())
                    .is_some_and(|s| s != "SUCCESS")
                {
                    self.error = Some(
                        r.get("error")
                            .and_then(|e| e.as_str())
                            .unwrap_or("antigravity turn failed")
                            .to_string(),
                    );
                }
                // The whole response when nothing streamed (older versions).
                if self.emitted_text.is_empty()
                    && let Some(t) = r.get("response").and_then(|t| t.as_str())
                {
                    out.push(StreamEvent::Text(t.to_string()));
                }
                self.finished = true;
            }
            _ => {}
        }
        out
    }

    /// Gemini CLI `-o stream-json` (best effort: message / tool events).
    fn gemini(&mut self, v: &Value) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        if let Some(id) = v
            .get("session_id")
            .or_else(|| v.get("sessionId"))
            .and_then(|s| s.as_str())
        {
            self.session_id = Some(id.to_string());
        }
        match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "message" | "assistant" => {
                if v.get("role")
                    .and_then(|r| r.as_str())
                    .unwrap_or("assistant")
                    == "assistant"
                {
                    let text = match v.get("content") {
                        Some(Value::String(s)) => s.clone(),
                        Some(Value::Array(parts)) => parts
                            .iter()
                            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join(""),
                        _ => String::new(),
                    };
                    if !text.is_empty() {
                        out.push(StreamEvent::Text(text));
                    }
                }
            }
            "tool_use" | "tool_call" => {
                out.push(StreamEvent::ToolActivity(ToolActivity {
                    id: v
                        .get("id")
                        .or_else(|| v.get("call_id"))
                        .and_then(|i| i.as_str())
                        .unwrap_or("tool")
                        .to_string(),
                    name: v
                        .get("name")
                        .or_else(|| v.get("tool"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("tool")
                        .to_string(),
                    input: v
                        .get("input")
                        .or_else(|| v.get("args"))
                        .cloned()
                        .unwrap_or(Value::Null),
                    output: None,
                    is_error: false,
                }));
            }
            "tool_result" => {
                out.push(StreamEvent::ToolActivity(ToolActivity {
                    id: v
                        .get("id")
                        .or_else(|| v.get("call_id"))
                        .and_then(|i| i.as_str())
                        .unwrap_or("tool")
                        .to_string(),
                    name: String::new(),
                    input: Value::Null,
                    output: Some(
                        v.get("output")
                            .or_else(|| v.get("content"))
                            .map(|o| match o {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .unwrap_or_default(),
                    ),
                    is_error: v.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false),
                }));
            }
            "result" => {
                if let Some(u) = v.get("usage").or_else(|| v.get("stats")) {
                    self.take_usage(u);
                }
                self.finished = true;
            }
            "error" => {
                self.error = Some(
                    v.get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("gemini error")
                        .to_string(),
                );
                self.finished = true;
            }
            _ => {}
        }
        out
    }
}

#[async_trait]
impl Provider for CliProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn stream(
        &self,
        req: ChatRequest,
    ) -> StreamResult<BoxStream<'static, StreamResult<StreamEvent>>> {
        let settings = self.settings.clone();
        let agent = settings.agent;
        let data_dir = self.data_dir.clone();
        let session_id = req.session_id.clone();
        let resume = session_id
            .as_deref()
            .and_then(|sid| load_cli_session(&data_dir, agent, sid));
        let prompt = CliProvider::prompt_for(&req, resume.is_some());
        let args = build_args(
            &settings,
            &req.model,
            &prompt,
            resume.as_deref(),
            &req.system,
            req.directory.as_deref(),
        );
        let program = which(&settings.command).ok_or_else(|| {
            LlmError::Provider(format!(
                "{} is not installed (command '{}' not found on PATH)",
                agent.label(),
                settings.command
            ))
        })?;

        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = req.directory.as_deref().filter(|d| Path::new(d).is_dir()) {
            cmd.current_dir(dir);
        }
        // A nested Claude Code refuses to run inside another Claude Code
        // session; the engine is not one.
        cmd.env_remove("CLAUDECODE");
        cmd.env_remove("CLAUDE_CODE_ENTRYPOINT");
        let mut child = cmd
            .spawn()
            .map_err(|e| LlmError::Provider(format!("failed to start {}: {e}", agent.label())))?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let (tx, rx) = mpsc::channel::<StreamResult<StreamEvent>>(256);
        tokio::spawn(async move {
            let _child_guard = &mut child; // dropped with the task -> kill_on_drop
            let mut parser = CliParser::new(agent);
            let mut lines = BufReader::new(stdout).lines();
            let stderr_task = tokio::spawn(async move {
                let mut err_lines = BufReader::new(stderr).lines();
                let mut collected = String::new();
                loop {
                    let next: Option<String> = err_lines.next_line().await.unwrap_or_default();
                    let Some(l) = next else { break };
                    if collected.len() < 4000 {
                        collected.push_str(&l);
                        collected.push('\n');
                    }
                }
                collected
            });
            loop {
                let next: Option<String> = lines.next_line().await.unwrap_or_default();
                let Some(line) = next else { break };
                for ev in parser.line(&line) {
                    if tx.send(Ok(ev)).await.is_err() {
                        return; // consumer gone (abort): the child dies with this task
                    }
                }
            }
            let status = child.wait().await;
            let stderr_text = stderr_task.await.unwrap_or_default();
            if let (Some(sid), Some(cli_sid)) =
                (session_id.as_deref(), parser.session_id.as_deref())
            {
                save_cli_session(&data_dir, agent, sid, cli_sid);
            }
            if let Some(err) = parser.error.take() {
                let _ = tx
                    .send(Err(LlmError::Provider(format!("{}: {err}", agent.label()))))
                    .await;
                return;
            }
            match status {
                Ok(s) if !s.success() && !parser.finished => {
                    let detail = stderr_text.trim();
                    let _ = tx
                        .send(Err(LlmError::Provider(format!(
                            "{} exited with {s}{}",
                            agent.label(),
                            if detail.is_empty() {
                                String::new()
                            } else {
                                format!(": {detail}")
                            }
                        ))))
                        .await;
                    return;
                }
                Err(e) => {
                    let _ = tx
                        .send(Err(LlmError::Provider(format!("{}: {e}", agent.label()))))
                        .await;
                    return;
                }
                _ => {}
            }
            let _ = tx.send(Ok(StreamEvent::Done(parser.usage.clone()))).await;
        });
        Ok(Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })))
    }
}

/// Build a CLI provider for `spec` (kind `cli`).
pub fn build_cli_provider(
    spec: &ProviderSpec,
    data_dir: PathBuf,
) -> Result<Arc<dyn Provider>, String> {
    Ok(Arc::new(CliProvider::new(spec, data_dir)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(agent: CliAgent, permission: CliPermission) -> CliSettings {
        CliSettings {
            agent,
            command: agent.command().to_string(),
            permission,
        }
    }

    #[test]
    fn claude_args_cover_print_mode_permission_model_and_resume() {
        let args = build_args(
            &settings(CliAgent::Claude, CliPermission::Edits),
            "cli-claude/sonnet",
            "hi",
            None,
            "SYS",
            Some("/p"),
        );
        assert!(args.iter().any(|a| a == "stream-json"));
        assert!(
            args.windows(2)
                .any(|w| w == ["--permission-mode", "acceptEdits"])
        );
        assert!(args.windows(2).any(|w| w == ["--model", "sonnet"]));
        assert!(
            args.windows(2)
                .any(|w| w == ["--append-system-prompt", "SYS"])
        );
        assert_eq!(args.last().map(String::as_str), Some("hi"));
        let resumed = build_args(
            &settings(CliAgent::Claude, CliPermission::All),
            "cli-claude/default",
            "hi",
            Some("sid"),
            "SYS",
            None,
        );
        assert!(resumed.windows(2).any(|w| w == ["--resume", "sid"]));
        assert!(
            resumed
                .iter()
                .any(|a| a == "--dangerously-skip-permissions")
        );
        assert!(!resumed.iter().any(|a| a == "--model"));
        assert!(!resumed.iter().any(|a| a == "--append-system-prompt"));
    }

    #[test]
    fn codex_args_use_exec_json_sandbox_and_resume_subcommand() {
        let args = build_args(
            &settings(CliAgent::Codex, CliPermission::Plan),
            "cli-codex/gpt-5.5",
            "do it",
            Some("t1"),
            "",
            Some("/p"),
        );
        assert_eq!(&args[..2], ["exec", "--json"]);
        let n = args.len();
        assert_eq!(&args[n - 3..], ["resume", "t1", "do it"]);
        assert!(args.windows(2).any(|w| w == ["-s", "read-only"]));
        assert!(args.windows(2).any(|w| w == ["-C", "/p"]));
        assert!(args.windows(2).any(|w| w == ["-m", "gpt-5.5"]));
        assert_eq!(args.last().map(String::as_str), Some("do it"));
    }

    #[test]
    fn spec_settings_default_the_command_and_permission() {
        let mut spec = ProviderSpec {
            name: "cli-grok".into(),
            kind: bebok_llm::ProviderKind::Cli,
            ..Default::default()
        };
        let s = CliSettings::from_spec(&spec).unwrap();
        assert_eq!(s.agent, CliAgent::Grok);
        assert_eq!(s.command, "grok");
        assert_eq!(s.permission, CliPermission::Edits);
        spec.extra.insert("cli".into(), Value::String("agy".into()));
        spec.extra
            .insert("command".into(), Value::String("/opt/agy".into()));
        spec.extra
            .insert("permission".into(), Value::String("all".into()));
        let s = CliSettings::from_spec(&spec).unwrap();
        assert_eq!(s.agent, CliAgent::Agy);
        assert_eq!(s.command, "/opt/agy");
        assert_eq!(s.permission, CliPermission::All);
        assert!(
            CliSettings::from_spec(&ProviderSpec {
                name: "x".into(),
                ..Default::default()
            })
            .is_err()
        );
    }

    #[test]
    fn claude_parser_streams_deltas_tools_and_result_without_duplicating_text() {
        let mut p = CliParser::new(CliAgent::Claude);
        assert!(
            p.line(r#"{"type":"system","subtype":"init","session_id":"s-1"}"#)
                .is_empty()
        );
        assert_eq!(p.session_id.as_deref(), Some("s-1"));
        let ev = p.line(r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}}"#);
        assert!(matches!(&ev[0], StreamEvent::Text(t) if t == "Hel"));
        let ev = p.line(r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"lo"}}}"#);
        assert!(matches!(&ev[0], StreamEvent::Text(t) if t == "lo"));
        // The final assistant message repeats the streamed text: nothing new.
        let ev = p.line(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello"},{"type":"tool_use","id":"tu1","name":"Read","input":{"file_path":"a.rs"}}]}}"#);
        assert_eq!(ev.len(), 1);
        assert!(
            matches!(&ev[0], StreamEvent::ToolActivity(a) if a.id == "tu1" && a.name == "Read" && a.output.is_none())
        );
        let ev = p.line(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","content":"fn main(){}"}]}}"#);
        assert!(
            matches!(&ev[0], StreamEvent::ToolActivity(a) if a.id == "tu1" && a.output.as_deref() == Some("fn main(){}"))
        );
        let ev = p.line(r#"{"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.01,"usage":{"input_tokens":10,"output_tokens":5},"session_id":"s-1"}"#);
        assert!(ev.is_empty());
        assert!(p.finished);
        assert_eq!(p.usage.input_tokens, 10);
        assert_eq!(p.usage.cost, Some(0.01));
        assert!(p.error.is_none());
    }

    #[test]
    fn grok_tool_results_are_decoded_and_streamed_thinking_is_not_repeated() {
        assert_eq!(
            tool_result_text(r#"{"type":"Bash","output":[104,105,10]}"#),
            "hi\n"
        );
        assert_eq!(tool_result_text(r#"{"output":"plain"}"#), "plain");
        assert_eq!(tool_result_text("just text"), "just text");
        let mut p = CliParser::new(CliAgent::Grok);
        let ev = p.line(r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hmm"}}}"#);
        assert!(matches!(&ev[0], StreamEvent::Thinking(t) if t == "hmm"));
        let ev = p.line(r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"ok"}]}}"#);
        assert_eq!(
            ev.len(),
            1,
            "thinking already streamed, only the text is new"
        );
        assert!(matches!(&ev[0], StreamEvent::Text(t) if t == "ok"));
    }

    #[test]
    fn cursor_partial_assistant_messages_are_deltas() {
        let mut p = CliParser::new(CliAgent::Cursor);
        let a = p.line(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hel"}]},"timestamp_ms":1}"#);
        let b = p.line(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"lo"}]},"timestamp_ms":2}"#);
        let fin = p
            .line(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello"}]}}"#);
        assert!(matches!(&a[0], StreamEvent::Text(t) if t == "Hel"));
        assert!(matches!(&b[0], StreamEvent::Text(t) if t == "lo"));
        assert!(fin.is_empty());
    }

    #[test]
    fn claude_parser_surfaces_a_result_error() {
        let mut p = CliParser::new(CliAgent::Claude);
        p.line(r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in","session_id":"x"}"#);
        assert_eq!(p.error.as_deref(), Some("Not logged in"));
    }

    #[test]
    fn codex_parser_maps_items() {
        let mut p = CliParser::new(CliAgent::Codex);
        p.line(r#"{"type":"thread.started","thread_id":"th1"}"#);
        assert_eq!(p.session_id.as_deref(), Some("th1"));
        let ev = p.line(r#"{"type":"item.started","item":{"id":"item_0","type":"command_execution","command":"ls","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#);
        assert!(
            matches!(&ev[0], StreamEvent::ToolActivity(a) if a.name == "bash" && a.output.is_none())
        );
        let ev = p.line(r#"{"type":"item.completed","item":{"id":"item_0","type":"command_execution","command":"ls","aggregated_output":"a\nb\n","exit_code":0,"status":"completed"}}"#);
        assert!(
            matches!(&ev[0], StreamEvent::ToolActivity(a) if a.output.as_deref() == Some("a\nb\n") && !a.is_error)
        );
        let ev = p.line(r#"{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"done"}}"#);
        assert!(matches!(&ev[0], StreamEvent::Text(t) if t == "done"));
        p.line(r#"{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":7}}"#);
        assert!(p.finished);
        assert_eq!(p.usage.input_tokens, 100);
        assert_eq!(p.usage.cache_read_input_tokens, Some(40));
        let mut failed = CliParser::new(CliAgent::Codex);
        failed.line(r#"{"type":"turn.failed","error":{"message":"model not supported"}}"#);
        assert_eq!(failed.error.as_deref(), Some("model not supported"));
    }

    #[test]
    fn agy_parser_maps_steps() {
        let mut p = CliParser::new(CliAgent::Agy);
        p.line(r#"{"event":"init","conversation_id":"c1","init":{}}"#);
        assert_eq!(p.session_id.as_deref(), Some("c1"));
        let ev = p.line(r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"ACTIVE","step_type":"tool","tool_name":"list_dir","tool_info":{"name":"list_dir","parameters":{"DirectoryPath":"/x"}}}}"#);
        assert!(
            matches!(&ev[0], StreamEvent::ToolActivity(a) if a.id == "agy-step-2" && a.name == "list_dir" && a.output.is_none())
        );
        let ev = p.line(r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"ERROR","step_type":"tool","tool_name":"list_dir","tool_info":{"name":"list_dir","parameters":{},"error":{"type":"TOOL_ERROR","message":"timed out"}}}}"#);
        assert!(
            matches!(&ev[0], StreamEvent::ToolActivity(a) if a.is_error && a.output.as_deref() == Some("timed out"))
        );
        let ev = p.line(r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":3,"state":"ACTIVE","step_type":"agent_response","text_delta":"OK"}}"#);
        assert!(matches!(&ev[0], StreamEvent::Text(t) if t == "OK"));
        let ev = p.line(r#"{"event":"result","result":{"conversation_id":"c1","status":"SUCCESS","response":"OK\n","usage":{"input_tokens":1,"output_tokens":1}}}"#);
        assert!(ev.is_empty(), "streamed text is not repeated");
        assert!(p.finished);
    }

    #[test]
    fn prompt_replays_history_only_on_a_fresh_cli_conversation() {
        let req = ChatRequest {
            model: "cli-codex/default".into(),
            system: String::new(),
            messages: vec![
                bebok_llm::ChatMessage::user("first"),
                bebok_llm::ChatMessage {
                    role: ChatRole::Assistant,
                    content: "answer".into(),
                    tool_calls: Vec::new(),
                    tool_results: Vec::new(),
                    content_parts: Vec::new(),
                },
                bebok_llm::ChatMessage::user("second"),
            ],
            tools: Vec::new(),
            max_tokens: 1,
            thinking: bebok_llm::Thinking::Off,
            session_id: None,
            directory: None,
        };
        assert_eq!(CliProvider::prompt_for(&req, true), "second");
        let fresh = CliProvider::prompt_for(&req, false);
        assert!(fresh.starts_with("Earlier conversation"));
        assert!(fresh.contains("User: first"));
        assert!(fresh.contains("Assistant: answer"));
        assert!(fresh.ends_with("second"));
    }
}
