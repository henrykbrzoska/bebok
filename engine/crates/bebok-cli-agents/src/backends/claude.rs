//! Claude Code (`claude`) backend.
//!
//! Claude Code runs its own agent loop: it decides to run Bash, edit files,
//! read the tree. That activity is *displayed*, never re-executed — see
//! [`crate::backend`] for the hard rule that a CLI's tool use must never
//! surface as [`bebok_llm::StreamEvent::ToolCall`]. This module therefore maps
//! `tool_use` / `tool_result` blocks to [`CliEvent::ToolActivity`] only.
//!
//! The shapes below were verified against `claude` 2.1.318 (`-p`,
//! `--output-format stream-json`, `--verbose`, `--model`,
//! `--append-system-prompt`, `--resume`, `--permission-mode`,
//! `--disallowedTools`; `--max-turns` is accepted but hidden from `--help`).

use std::path::Path;

use serde_json::{Value, json};

use crate::backend::{
    CliBackend, CliCapabilities, CliError, CliEvent, CliProbe, CliRequest, CliStreamState,
    PromptBlock, TurnScratch,
};
use crate::profile::{
    ProfileFile, ProfileInput, ProfileWriter, RenderedProfile, deep_merge, read_project_layer,
    system_text, to_json_file, validate_settings,
};
use crate::spec::{CliSpec, ProfileMode, SystemPromptMode};

/// How much of a tool input / result we keep for the one-line UI row: long
/// enough to recognise the call, short enough not to flood the transcript.
const DETAIL_CAP: usize = 200;

/// Bytes of prompt text allowed on the command line. Only used when the prompt
/// cannot go on stdin. Windows runs the shim through `cmd /C`, whose whole line
/// is capped at 8191 characters; Linux allows 128 KiB per argument.
const ARGV_TEXT_BUDGET: usize = if cfg!(windows) { 6_000 } else { 100_000 };

/// Tools that can mutate the workspace. Denied explicitly in read-only mode in
/// addition to `--permission-mode plan`: permission *modes* are a policy of the
/// CLI and have changed between releases, while an explicit tool denial is
/// enforced by its tool dispatcher.
const MUTATING_TOOLS: [&str; 3] = ["Edit", "Write", "Bash"];

/// Pre-granted tool list for a write-enabled turn (`extra.allow_write`).
///
/// Shell tools lead the list on purpose: a CLI that may edit files but not run
/// anything spends its whole budget reading and re-reading the tree, which
/// looks exactly like a stuck agent. Everything here is a capability the user
/// already opted into; nothing outside it is loosened, and
/// `bypassPermissions` stays banned.
const WRITE_TOOLS: [&str; 9] = [
    "Bash",
    "BashOutput",
    "KillShell",
    "KillBash",
    "Edit",
    "MultiEdit",
    "Write",
    "NotebookEdit",
    "TodoWrite",
];

pub struct ClaudeBackend;

impl CliBackend for ClaudeBackend {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn label(&self) -> &'static str {
        "Claude Code"
    }

    fn capabilities(&self) -> CliCapabilities {
        CliCapabilities {
            streaming: true,
            resume: true,
            system_prompt: true,
            tool_events: true,
            usage: true,
            // Images travel as `image` content blocks on stdin when the binary
            // accepts stream-json input. On the argv fallback they become
            // placeholders (see `transcript::render_prompt`).
            images: true,
            mcp: true,
        }
    }

    fn default_models(&self) -> Vec<String> {
        vec![
            "opus".to_string(),
            "sonnet".to_string(),
            "haiku".to_string(),
        ]
    }

    fn binary_names(&self) -> &'static [&'static str] {
        &["claude"]
    }

    fn env_override(&self) -> &'static str {
        "BEBOK_CLAUDE_BIN"
    }

    fn profile_writer(&self) -> Option<&dyn ProfileWriter> {
        Some(self)
    }

    /// `--resume` is only used when the installed binary's own `--help`
    /// advertises it (see `build_command`).
    fn resume_flag_available(&self, probe: &CliProbe) -> bool {
        probe.has("--resume")
    }

    /// Build `claude <flags>`, with the prompt on stdin or, as a fallback, on
    /// the command line.
    ///
    /// The program is the bare `"claude"` name: path resolution
    /// (`extra.command`, [`Self::env_override`], `PATH`) belongs to the runner,
    /// which is also the place that reports [`CliError::BinaryMissing`] with the
    /// right env var.
    ///
    /// Preferred transport: `--input-format stream-json`, the prompt as one
    /// JSON user message on stdin ([`Self::stdin_payload`]). The runner closes
    /// stdin after the write, which is what lets `claude -p` start. The command
    /// line then carries no transcript, so OS argument limits stop applying.
    ///
    /// Fallback, when the installed binary does not advertise stream-json
    /// input: the prompt is a positional argument after `--`. An oversized one
    /// is refused with [`CliError::Config`] rather than failing with E2BIG.
    ///
    /// Every flag here is gated on the binary's own `--help` ([`CliProbe`]):
    /// a flag the probe does not show is never emitted.
    fn build_command(
        &self,
        spec: &CliSpec,
        req: &CliRequest,
        probe: &CliProbe,
        scratch: &mut TurnScratch,
    ) -> Result<(String, Vec<String>), CliError> {
        let stdin_input = supports_stream_input(probe);

        // Non-interactive streaming. `--verbose` is not optional:
        // `-p --output-format stream-json` is rejected without it.
        let mut args: Vec<String> = vec![
            "-p".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
        ];

        if stdin_input {
            args.push("--input-format".to_string());
            args.push("stream-json".to_string());
        }
        if probe.has("--include-partial-messages") {
            // Token deltas as they are generated. `parse_line` drops the full
            // message that follows a block it already showed from deltas.
            args.push("--include-partial-messages".to_string());
        }

        if let Some(model) = non_empty(req.model.as_deref()) {
            args.push("--model".to_string());
            args.push(model.to_string());
        }

        // Bytes of system prompt that have to travel on the command line.
        let mut argv_bytes = 0usize;
        // A managed profile hands the system prompt over as a file of its own
        // (see `render`); in that case this function emits nothing for it.
        let project_append = read_project_layer(&req.cwd, self.id(), "system.append.md")?;
        let system = system_text(req.system.as_deref(), project_append.as_deref());
        let profile_owns_system = system_file_flag(spec, probe)?.is_some();
        if self.capabilities().system_prompt
            && !profile_owns_system
            && let Some(system) = system.as_deref()
        {
            if supports_system_prompt_file(probe) {
                // The file lives as long as the process stream (see `runner`).
                let path = scratch.write_file("system-prompt.txt", system)?;
                args.push("--append-system-prompt-file".to_string());
                args.push(path.to_string_lossy().into_owned());
            } else {
                argv_bytes += system.len();
                args.push("--append-system-prompt".to_string());
                args.push(system.to_string());
            }
        }

        if let Some(turns) = req.max_turns {
            args.push("--max-turns".to_string());
            args.push(turns.to_string());
        }

        if let Some(key) = non_empty(req.resume_key.as_deref())
            && self.resume_flag_available(probe)
        {
            args.push("--resume".to_string());
            args.push(key.to_string());
        }

        // SECURITY. A CLI edits files without going through Bebok's permission
        // engine or change tracking, so writing is opt-in (`extra.allow_write`,
        // folded into `req.write` by the runner). Without it the CLI gets the
        // most restrictive policy it offers: `plan` mode *and* an explicit
        // denial of the file/shell tools — belt and braces, because a mode
        // alone is one config key away from being the wrong one.
        //
        // `--permission-mode bypassPermissions` (and
        // `--dangerously-skip-permissions`) is deliberately NEVER emitted: it
        // would disable the CLI's own guard rails on a machine where Bebok has
        // no say, which is strictly worse than Bebok's YOLO mode. Even when
        // writing is allowed we use `acceptEdits`, which still keeps the CLI's
        // prompts for shell escalation.
        // `--permission-mode acceptEdits` on its own is NOT enough for a
        // non-interactive turn: under `-p` there is nobody to answer a
        // permission prompt, so every tool that would still ask — `Bash` above
        // all — is denied outright and simply reported back as a
        // `permission_denials` entry. The CLI then re-reads the tree instead of
        // running the command, and the turn looks hung. `--allowedTools`
        // pre-grants exactly the capability the user opted into with
        // `extra.allow_write`; nothing is granted that `acceptEdits` would not
        // have approved interactively, and `bypassPermissions` stays banned.
        if req.write {
            args.push("--permission-mode".to_string());
            args.push("acceptEdits".to_string());
            args.push("--allowedTools".to_string());
            args.extend(WRITE_TOOLS.iter().map(|t| (*t).to_string()));
        } else {
            args.push("--permission-mode".to_string());
            args.push("plan".to_string());
            args.push("--disallowedTools".to_string());
            args.extend(MUTATING_TOOLS.iter().map(|t| (*t).to_string()));
        }

        // User flags last, so they can override our defaults (their own
        // `--permission-mode`, an extra `--add-dir`, ...).
        args.extend(spec.args.iter().cloned());

        if !stdin_input {
            argv_bytes += req.prompt.len();
        }
        if argv_bytes > ARGV_TEXT_BUDGET {
            let reason = if stdin_input {
                "the system prompt is too large, and the installed claude has no --append-system-prompt-file"
            } else {
                "the installed claude does not accept --input-format stream-json, so the transcript has to go on the command line"
            };
            return Err(CliError::Config(format!(
                "{argv_bytes} bytes of prompt do not fit on the command line (limit {ARGV_TEXT_BUDGET}): {reason}"
            )));
        }

        if !stdin_input {
            // `--disallowedTools` takes a *variadic* value list, so a trailing
            // bare positional would be swallowed as another tool name
            // (verified: claude then fails with "Input must be provided ...").
            // `--` terminates option parsing and hands the prompt over intact;
            // with it the prompt is still the last argument.
            args.push("--".to_string());
            args.push(req.prompt.clone());
        }

        Ok(("claude".to_string(), args))
    }

    /// One user message in the stream-json input format, newline-terminated.
    /// Images travel as base64 `image` blocks, the form the API takes.
    fn stdin_payload(&self, req: &CliRequest, probe: &CliProbe) -> Option<Vec<u8>> {
        if !supports_stream_input(probe) {
            return None;
        }
        let content: Vec<Value> = if req.blocks.is_empty() {
            vec![json!({ "type": "text", "text": req.prompt })]
        } else {
            req.blocks.iter().map(prompt_block_json).collect()
        };
        let message = json!({
            "type": "user",
            "message": { "role": "user", "content": content },
        });
        let mut line = message.to_string().into_bytes();
        line.push(b'\n');
        Some(line)
    }

    /// The init line names the model exactly (`claude-opus-5`); aliases such as
    /// `opus` or `sonnet` are resolved to the newest catalog entry of that
    /// family.
    fn catalog_model(&self, cli_model: &str, init_model: Option<&str>) -> Option<String> {
        let catalog = bebok_llm::ModelCatalog::global();
        let known: Vec<String> = catalog.provider_models("anthropic");

        for candidate in [non_empty(init_model), non_empty(Some(cli_model))]
            .into_iter()
            .flatten()
        {
            if known
                .iter()
                .any(|name| name.eq_ignore_ascii_case(candidate))
            {
                return Some(format!("anthropic/{candidate}"));
            }
        }

        let alias = non_empty(Some(cli_model))?.to_ascii_lowercase();
        if !matches!(alias.as_str(), "opus" | "sonnet" | "haiku") {
            return None;
        }
        let family = format!("claude-{alias}");
        known
            .iter()
            .filter(|name| name.starts_with(&family))
            .max()
            .map(|name| format!("anthropic/{name}"))
    }

    fn parse_line(
        &self,
        line: &str,
        st: &mut CliStreamState,
    ) -> Result<Option<CliEvent>, CliError> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(None);
        }

        // A CLI may print warnings or progress on stdout. Those must not fail a
        // turn — they are dropped with a debug log instead.
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(err) => {
                tracing::debug!(error = %err, "claude: skipping non-JSON stdout line");
                return Ok(None);
            }
        };
        if value.as_object().is_none() {
            tracing::debug!("claude: skipping non-object stdout line");
            return Ok(None);
        }

        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();

        match kind {
            "system" => {
                let subtype = value
                    .get("subtype")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if subtype != "init" {
                    tracing::debug!(subtype, "claude: ignoring system event");
                    return Ok(None);
                }
                // The conversation id lives on `session_id`; it is what
                // `--resume` takes on the next turn.
                let Some(session_id) = non_empty(value.get("session_id").and_then(Value::as_str))
                else {
                    tracing::debug!("claude: init event without session_id");
                    return Ok(None);
                };
                st.session_key = Some(session_id.to_string());
                Ok(Some(CliEvent::SessionKey(session_id.to_string())))
            }

            "assistant" => Ok(assistant_event(&value, st)),

            // Token deltas, present with `--include-partial-messages`.
            "stream_event" => Ok(stream_event(&value, st)),

            "user" => {
                // Tool results come back as a synthetic user message. Only the
                // `tool_result` blocks matter; the rest is a prompt echo that
                // must not be rendered a second time.
                let detail = tool_result_text(value.pointer("/message/content"));
                if detail.is_empty() {
                    return Ok(None);
                }
                Ok(Some(CliEvent::ToolActivity {
                    name: "tool_result".to_string(),
                    detail,
                }))
            }

            "result" => {
                if let Some(session_id) = non_empty(value.get("session_id").and_then(Value::as_str))
                {
                    st.session_key = Some(session_id.to_string());
                }
                let usage = value.get("usage");
                let number = |key: &str| usage.and_then(|u| u.get(key)).and_then(Value::as_u64);

                // Denied tools are reported as a note: the user should see
                // that the CLI could not do something, even on a successful run.
                let denied = permission_denials(&value);
                if !denied.is_empty() {
                    st.pending.push_back(CliEvent::Thinking(format!(
                        "⚠ Claude Code could not run: {}",
                        denied.join(", ")
                    )));
                }
                // The usage below is still emitted first, so the cost of a
                // failed run is counted before the failure surfaces.
                st.failure = run_failure(&value);

                Ok(Some(CliEvent::Usage(bebok_llm::Usage {
                    input_tokens: number("input_tokens").unwrap_or(0),
                    output_tokens: number("output_tokens").unwrap_or(0),
                    cost: value.get("total_cost_usd").and_then(Value::as_f64),
                    cache_read_input_tokens: number("cache_read_input_tokens"),
                    cache_creation_input_tokens: number("cache_creation_input_tokens"),
                    // The window fill is the last API call, not this turn's sum.
                    context_tokens: st.context_tokens,
                })))
            }

            // `rate_limit_event`, `stream_event`, hook output, future
            // additions: unknown shapes are normal and must never break a turn.
            other => {
                tracing::debug!(kind = other, "claude: ignoring unknown event type");
                Ok(None)
            }
        }
    }
}

/// Collapse one `assistant` message into the single event the client renders.
///
/// A message carries an ordered `content` array. Blocks already shown from
/// partial deltas are skipped (see [`stream_event`]), so text is never rendered
/// twice; tool use in the same message is still processed. Among the rest we
/// pick the first block that yields a non-empty event, preferring prose over
/// reasoning over tool use (text → thinking → tool_use): prose is what the user
/// reads, and when a message holds both, the tool row belongs after it. In
/// practice Claude emits one block per message, so the preference rarely
/// decides anything.
///
/// The message's usage is the prompt size of this API call, so it is kept as
/// the latest context size (the `result` line sums the whole turn instead).
fn assistant_event(value: &Value, st: &mut CliStreamState) -> Option<CliEvent> {
    let message = value.get("message")?;
    if let Some(tokens) = message.get("usage").and_then(prompt_tokens) {
        st.context_tokens = Some(tokens);
    }

    let content = message.get("content").and_then(Value::as_array)?;
    let streamed: &[String] = match message.get("id").and_then(Value::as_str) {
        Some(id) if st.message_id.as_deref() == Some(id) => &st.streamed,
        _ => &[],
    };
    let blocks: Vec<&Value> = content
        .iter()
        .filter(|block| !already_streamed(block, streamed))
        .collect();

    let text = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .find_map(|b| non_empty(b.get("text").and_then(Value::as_str)));
    if let Some(text) = text {
        return Some(CliEvent::Text(text.to_string()));
    }

    let thinking = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("thinking"))
        .find_map(|b| non_empty(b.get("thinking").and_then(Value::as_str)));
    if let Some(thinking) = thinking {
        return Some(CliEvent::Thinking(thinking.to_string()));
    }

    blocks
        .iter()
        .find(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
        .map(|block| {
            let name = block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string();
            let detail = tool_detail(&name, block.get("input"));
            CliEvent::ToolActivity { name, detail }
        })
}

/// One-line summary of a tool call: the argument a human would recognise.
fn tool_detail(name: &str, input: Option<&Value>) -> String {
    let Some(input) = input else {
        return String::new();
    };

    let preferred = match name {
        "Bash" | "BashOutput" => "command",
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => "file_path",
        "Grep" | "Glob" => "pattern",
        "WebFetch" => "url",
        "Task" => "description",
        _ => "",
    };

    if !preferred.is_empty()
        && let Some(text) = non_empty(input.get(preferred).and_then(Value::as_str))
    {
        return truncate(text);
    }

    // Unknown tool or missing argument: fall back to the compact input object.
    truncate(&input.to_string())
}

/// Text of the `tool_result` blocks, joined when the content is an array.
fn tool_result_text(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };

    match content {
        Value::String(text) => truncate(text),
        Value::Array(blocks) => {
            let mut parts: Vec<&str> = Vec::new();
            for block in blocks {
                if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                    continue;
                }
                match block.get("content") {
                    Some(Value::String(text)) => parts.push(text.as_str()),
                    Some(Value::Array(inner)) => {
                        for item in inner {
                            if let Some(text) = item.get("text").and_then(Value::as_str) {
                                parts.push(text);
                            }
                        }
                    }
                    _ => {}
                }
            }
            truncate(&parts.join("\n"))
        }
        _ => String::new(),
    }
}

/// Token-level events (`--include-partial-messages`). Text and thinking deltas
/// are shown as they arrive. A finished block is remembered by content, so the
/// full `assistant` message that follows does not show it again.
fn stream_event(value: &Value, st: &mut CliStreamState) -> Option<CliEvent> {
    let event = value.get("event")?;
    match event.get("type").and_then(Value::as_str)? {
        "message_start" => {
            st.message_id = event
                .pointer("/message/id")
                .and_then(Value::as_str)
                .map(str::to_string);
            st.streamed.clear();
            st.block = None;
            None
        }
        "content_block_start" => {
            let kind = event.pointer("/content_block/type").and_then(Value::as_str);
            st.block = matches!(kind, Some("text" | "thinking")).then(String::new);
            None
        }
        "content_block_delta" => {
            let delta = event.get("delta")?;
            let (text, thinking) = match delta.get("type").and_then(Value::as_str)? {
                "text_delta" => (delta.get("text").and_then(Value::as_str)?, false),
                "thinking_delta" => (delta.get("thinking").and_then(Value::as_str)?, true),
                _ => return None,
            };
            if text.is_empty() {
                return None;
            }
            if let Some(block) = st.block.as_mut() {
                block.push_str(text);
            }
            Some(if thinking {
                CliEvent::Thinking(text.to_string())
            } else {
                CliEvent::Text(text.to_string())
            })
        }
        "content_block_stop" => {
            if let Some(block) = st.block.take() {
                let block = block.trim();
                if !block.is_empty() {
                    st.streamed.push(block.to_string());
                }
            }
            None
        }
        _ => None,
    }
}

/// True when a full-message block is a text/thinking block that was already
/// shown from deltas. Compared on trimmed content, as the full message is.
fn already_streamed(block: &Value, streamed: &[String]) -> bool {
    let text = match block.get("type").and_then(Value::as_str) {
        Some("text") => block.get("text").and_then(Value::as_str),
        Some("thinking") => block.get("thinking").and_then(Value::as_str),
        _ => None,
    };
    text.is_some_and(|text| streamed.iter().any(|shown| shown == text.trim()))
}

/// Prompt size of one API call: fresh input plus both cache buckets. Missing
/// cache buckets count as zero; a usage object without `input_tokens` gives
/// `None`.
fn prompt_tokens(usage: &Value) -> Option<u64> {
    let input = usage.get("input_tokens").and_then(Value::as_u64)?;
    let cached = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    Some(input + cached("cache_read_input_tokens") + cached("cache_creation_input_tokens"))
}

/// Stream-json encoding of one prompt block.
fn prompt_block_json(block: &PromptBlock) -> Value {
    match block {
        PromptBlock::Text(text) => json!({ "type": "text", "text": text }),
        PromptBlock::Image { media_type, data } => json!({
            "type": "image",
            "source": { "type": "base64", "media_type": media_type, "data": data },
        }),
    }
}

/// The binary accepts the prompt as stream-json on stdin. Both halves must be
/// advertised: the flag, and a `stream-json` choice in its help. The choice is
/// matched case-insensitively, because help formatting (quotes, casing) varies
/// between releases and a miss here silently falls back to argv.
fn supports_stream_input(probe: &CliProbe) -> bool {
    probe.has("--input-format") && probe.mentions_ci("stream-json")
}

/// The binary takes the system prompt from a file. Accepts both the literal
/// flag and the `[-file]` notation the help text uses for the pair.
fn supports_system_prompt_file(probe: &CliProbe) -> bool {
    probe.mentions("--append-system-prompt-file") || probe.mentions("--append-system-prompt[-file]")
}

/// The binary takes a replacement system prompt from a file.
fn supports_replace_file(probe: &CliProbe) -> bool {
    probe.has("--system-prompt-file") || probe.mentions("--system-prompt[-file]")
}

/// The flag that hands the system prompt to the CLI by file, when a managed
/// profile owns it. `None` means the profile does not own it: `build_command`
/// keeps its historical path (a scratch file, or argv on old binaries).
///
/// `user` mode never owns it. `replace` mode without file support is an error,
/// not a quiet downgrade to append: the user asked for a replaced prompt.
fn system_file_flag(spec: &CliSpec, probe: &CliProbe) -> Result<Option<&'static str>, CliError> {
    if spec.profile == ProfileMode::User {
        return Ok(None);
    }
    match spec.system_prompt_mode {
        SystemPromptMode::Append => Ok(supports_system_prompt_file(probe)
            .then_some("--append-system-prompt-file")),
        SystemPromptMode::Replace if supports_replace_file(probe) => Ok(Some("--system-prompt-file")),
        SystemPromptMode::Replace => Err(CliError::Config(
            "extra.system_prompt_mode = \"replace\" needs a claude that accepts --system-prompt-file; \
             this one does not (use \"append\")"
                .into(),
        )),
    }
}

/// The Bebok side of `settings.json`'s `hooks` block: every tool call is gated
/// by `bebok-server cli-hook <event>` before it runs and reported after it.
///
/// The hook child finds the engine through `BEBOK_HOOK_URL` /
/// `BEBOK_HOOK_TOKEN`, which the runner puts in the CLI's environment for each
/// turn (see `bebok_core::cli_turn_token`); nothing engine-side is baked into
/// the profile, so the cached settings file stays valid across restarts.
///
/// `timeout` is the CLI's own hook timeout. `pre` answers itself 5 s earlier
/// (`routes::cli_hook::PRE_ANSWER_TIMEOUT`), so the CLI receives a real `deny`
/// with a reason instead of "hook timed out".
///
/// [verify] shape checked against the Claude Code hooks documentation
/// (`hooks.<Event>[].hooks[].{type,command,timeout}`); a `Stop` hook takes no
/// matcher.
fn hook_settings() -> Value {
    // The hook must run *this* binary: a bundled sidecar and `cargo run` both
    // land on the engine executable, and a rename keeps working because the
    // path is resolved at profile-write time.
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "bebok-server".to_string());
    // Quoted: the path may contain spaces (`C:\Program Files\…`) and the CLI
    // runs the command through a shell. A `.exe` suffix is harmless on Unix and
    // required on Windows.
    let command = |event: &str| format!("\"{exe}\" cli-hook {event}");
    let entry = |event: &str, timeout: u32| {
        json!([{
            "matcher": "*",
            "hooks": [{ "type": "command", "command": command(event), "timeout": timeout }],
        }])
    };
    json!({
        "PreToolUse": entry("pre", 600),
        "PostToolUse": entry("post", 60),
        "Stop": [{
            "hooks": [{ "type": "command", "command": command("stop"), "timeout": 60 }],
        }],
    })
}

/// `settings.json` values Bebok puts under its own control for a managed
/// profile. `write` selects the permission posture; the rest is fixed.
fn managed_base(write: bool) -> Value {
    let (default_mode, allow, deny) = if write {
        ("acceptEdits", json!(WRITE_TOOLS), json!([]))
    } else {
        ("plan", json!([]), json!(MUTATING_TOOLS))
    };
    json!({
        "permissions": { "defaultMode": default_mode, "allow": allow, "deny": deny },
        // AGENTS.md §6: no attribution trailers in anything Bebok makes.
        "includeCoAuthoredBy": false,
        // Deterministic, quiet runs: no self-update, no nonessential traffic.
        "env": {
            "DISABLE_AUTOUPDATER": "1",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
            "DISABLE_TELEMETRY": "1",
        },
        // Bebok's own hook entries: every CLI tool call is gated by Bebok
        // before it runs. The validator refuses overrides that drop them.
        "hooks": hook_settings(),
    })
}

/// Parse a project `settings.json` layer; it must be a JSON object.
fn parse_settings_layer(text: &str, path: &Path) -> Result<Value, CliError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|e| CliError::Config(format!("{}: not valid JSON ({e})", path.display())))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(CliError::Config(format!(
            "{}: must contain a JSON object",
            path.display()
        )))
    }
}

fn abs_path(dir: &Path, name: &str) -> String {
    dir.join(name).display().to_string()
}

/// Renders the Claude Code profile. Order of layers, lowest first: Bebok's
/// managed base, `extra.profile_overrides`, the project's own
/// `.bebok/cli/claude/settings.json`. Validation runs on the result.
impl ProfileWriter for ClaudeBackend {
    fn render(&self, input: &ProfileInput<'_>) -> Result<RenderedProfile, CliError> {
        let ProfileInput {
            spec,
            req,
            probe,
            dir,
        } = *input;
        let project = req.cwd.as_path();
        let mut out = RenderedProfile {
            version: probe.features().version.clone(),
            ..RenderedProfile::default()
        };

        let base = match spec.profile {
            ProfileMode::User => json!({}),
            ProfileMode::Bebok | ProfileMode::Isolated => managed_base(req.write),
        };
        let mut settings = base.clone();
        if let Some(overrides) = &spec.profile_overrides {
            deep_merge(&mut settings, overrides);
        }
        if let Some(text) = read_project_layer(project, self.id(), "settings.json")? {
            let path = crate::profile::project_layer_path(project, self.id(), "settings.json");
            deep_merge(&mut settings, &parse_settings_layer(&text, &path)?);
        }
        validate_settings(&base, &settings)?;

        let managed = spec.profile != ProfileMode::User;
        if !managed && settings == json!({}) {
            // `user` mode with nothing to add: today's behaviour, no files.
            return Ok(out);
        }
        if !probe.has("--settings") {
            let why = if probe.features().help.is_empty() {
                "its --help did not answer, so the feature probe is empty"
            } else {
                "its --help does not list it"
            };
            return Err(CliError::Config(format!(
                "the CLI profile needs claude's --settings, and {why}; \
                 update claude or set extra.profile to \"user\""
            )));
        }

        out.files.push(ProfileFile {
            name: "settings.json".into(),
            contents: to_json_file(&settings)?,
        });
        out.args.push("--settings".into());
        out.args.push(abs_path(dir, "settings.json"));
        out.flags.push("--settings".into());

        if !managed {
            return Ok(out);
        }

        // Bebok mode: only the user's *user* settings, never the repository's
        // own `.claude/` (it would duplicate AGENTS.md). Isolated mode reads
        // nothing but its own config dir, so the flag is not needed there.
        if spec.profile == ProfileMode::Bebok && probe.has("--setting-sources") {
            out.args.push("--setting-sources".into());
            out.args.push("user".into());
            out.flags.push("--setting-sources".into());
        }

        if probe.has("--mcp-config") {
            out.files.push(ProfileFile {
                name: "mcp.json".into(),
                contents: to_json_file(&json!({ "mcpServers": {} }))?,
            });
            out.args.push("--mcp-config".into());
            out.args.push(abs_path(dir, "mcp.json"));
            out.flags.push("--mcp-config".into());
            if probe.has("--strict-mcp-config") {
                out.args.push("--strict-mcp-config".into());
                out.flags.push("--strict-mcp-config".into());
            }
        }

        if let Some(flag) = system_file_flag(spec, probe)?
            && let Some(text) = system_text(
                req.system.as_deref(),
                read_project_layer(project, self.id(), "system.append.md")?.as_deref(),
            )
        {
            out.files.push(ProfileFile {
                name: "system.md".into(),
                contents: format!("{text}\n"),
            });
            out.args.push(flag.into());
            out.args.push(abs_path(dir, "system.md"));
            out.flags.push(flag.into());
        }

        if spec.profile == ProfileMode::Isolated {
            out.env
                .push(("CLAUDE_CONFIG_DIR".into(), dir.display().to_string()));
        }
        Ok(out)
    }
}

/// Tool names Claude Code refused to run in a `result` line.
fn permission_denials(value: &Value) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let denials = value.get("permission_denials").and_then(Value::as_array);
    for denial in denials.into_iter().flatten() {
        if let Some(name) = denial.get("tool_name").and_then(Value::as_str)
            && !names.iter().any(|known| known == name)
        {
            names.push(name.to_string());
        }
    }
    names
}

/// The failure message for a `result` line Claude Code reports as an error:
/// `is_error`, or an `error_*` subtype. Hitting the turn limit gets a hint,
/// because the provider setting is the one thing the user can change.
fn run_failure(value: &Value) -> Option<String> {
    let subtype = value
        .get("subtype")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let is_error = value
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !is_error && !subtype.starts_with("error") {
        return None;
    }
    if subtype == "error_max_turns" {
        return Some(
            "Claude Code stopped after reaching the turn limit (raise extra.max_turns in the CLI provider settings)"
                .to_string(),
        );
    }
    Some(
        match non_empty(value.get("result").and_then(Value::as_str)) {
            Some(text) => text.to_string(),
            None if subtype.is_empty() => "Claude Code reported an error".to_string(),
            None => format!("Claude Code reported an error ({subtype})"),
        },
    )
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

/// Char-boundary-safe truncation; keeps the head, which is where a command or a
/// patch header lives.
fn truncate(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= DETAIL_CAP {
        return text.to_string();
    }
    let mut out: String = text.chars().take(DETAIL_CAP).collect();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{CliMode, ProfileDir};
    use std::path::PathBuf;
    use std::time::Duration;

    // --- fixtures: verbatim shapes of `claude --output-format stream-json` ---

    const INIT_LINE: &str = r#"{"type":"system","subtype":"init","cwd":"/tmp/p","session_id":"733f80f1-74ea-40ee-8ac9-f23edffb6c82","tools":["Read","Bash"],"model":"claude-opus-5","permissionMode":"plan","apiKeySource":"none","claude_code_version":"2.1.318"}"#;

    const ASSISTANT_TEXT_LINE: &str = r#"{"type":"assistant","message":{"model":"claude-opus-5","id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":2,"output_tokens":1}},"parent_tool_use_id":null,"session_id":"733f80f1","uuid":"89afd42e"}"#;

    const ASSISTANT_THINKING_LINE: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"let me look at the file first","signature":"CAISuQQ"}],"usage":{"input_tokens":2,"output_tokens":8}},"session_id":"733f80f1","uuid":"9f52a7b2"}"#;

    const ASSISTANT_BASH_LINE: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"cargo test --workspace","description":"run tests"},"caller":{"type":"direct"}}]},"session_id":"733f80f1","uuid":"d7e2c547"}"#;

    const ASSISTANT_READ_LINE: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_2","name":"Read","input":{"file_path":"/tmp/claudetest/foo.txt"},"caller":{"type":"direct"}}]},"session_id":"733f80f1","uuid":"4770f003"}"#;

    const USER_TOOL_RESULT_LINE: &str = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_2","type":"tool_result","content":"1\thello\n2\t"}]},"parent_tool_use_id":null,"session_id":"733f80f1","uuid":"0f40f32c"}"#;

    const USER_TOOL_RESULT_BLOCKS_LINE: &str = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_3","content":[{"type":"text","text":"line one"},{"type":"text","text":"line two"}]}]},"session_id":"733f80f1"}"#;

    const RESULT_LINE: &str = r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":2371,"num_turns":1,"result":"hi","session_id":"733f80f1-74ea-40ee-8ac9-f23edffb6c82","total_cost_usd":0.21992,"usage":{"input_tokens":2,"cache_creation_input_tokens":21981,"cache_read_input_tokens":7,"output_tokens":4,"server_tool_use":{"web_search_requests":0}},"modelUsage":{"claude-opus-5":{"inputTokens":2,"outputTokens":4}}}"#;

    const RATE_LIMIT_LINE: &str = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"five_hour"},"uuid":"47370b31","session_id":"733f80f1"}"#;

    fn spec_with(args: &[&str]) -> CliSpec {
        CliSpec {
            backend: "claude".to_string(),
            command: None,
            args: args.iter().map(|a| (*a).to_string()).collect(),
            mode: CliMode::Agent,
            allow_write: false,
            timeout_secs: 900,
            max_turns: None,
            profile: Default::default(),
            profile_overrides: None,
            system_prompt_mode: Default::default(),
        }
    }

    fn request() -> CliRequest {
        CliRequest {
            prompt: "explain the store module".to_string(),
            blocks: vec![PromptBlock::Text("explain the store module".to_string())],
            system: None,
            model: None,
            cwd: PathBuf::from("/tmp/p"),
            resume_key: None,
            write: false,
            max_turns: None,
            timeout: Duration::from_secs(120),
            extra_env: Vec::new(),
        }
    }

    fn args_of(spec: &CliSpec, req: &CliRequest) -> Vec<String> {
        args_with(spec, req, &CliProbe::default())
    }

    /// A probe for a binary whose `--help` advertises the given flags. The
    /// words mirror the real `claude` 2.1.325 help text.
    fn probe_with(flags: &[&str]) -> CliProbe {
        let mut help = String::from("Usage: claude [options]\n");
        for flag in flags {
            match *flag {
                "stream-input" => help.push_str(
                    "  --input-format <format>  Input format (only works with --print):\n\
                     \"text\" (default), or \"stream-json\" (choices: \"text\", \"stream-json\")\n",
                ),
                "partial" => {
                    help.push_str("  --include-partial-messages  Include partial message chunks\n")
                }
                "system-file" => help.push_str(
                    "  via: --system-prompt[-file], --append-system-prompt[-file], --add-dir\n",
                ),
                other => help.push_str(&format!("  {other}\n")),
            }
        }
        CliProbe::new(help)
    }

    fn args_with(spec: &CliSpec, req: &CliRequest, probe: &CliProbe) -> Vec<String> {
        let mut scratch = TurnScratch::default();
        let (program, args) = ClaudeBackend
            .build_command(spec, req, probe, &mut scratch)
            .expect("command builds");
        assert_eq!(program, "claude");
        args
    }

    fn position(args: &[String], needle: &str) -> usize {
        args.iter()
            .position(|a| a == needle)
            .unwrap_or_else(|| panic!("missing {needle} in {args:?}"))
    }

    #[test]
    fn read_only_command_denies_mutating_tools() {
        let args = args_of(&spec_with(&[]), &request());

        assert_eq!(
            &args[..4],
            ["-p", "--output-format", "stream-json", "--verbose"]
        );

        let mode = position(&args, "--permission-mode");
        assert_eq!(args[mode + 1], "plan");

        let denied = position(&args, "--disallowedTools");
        assert_eq!(&args[denied + 1..denied + 4], ["Edit", "Write", "Bash"]);

        assert!(args.iter().all(|a| a != "bypassPermissions"));
        assert!(args.iter().all(|a| a != "--dangerously-skip-permissions"));
    }

    #[test]
    fn write_mode_accepts_edits_and_never_bypasses_permissions() {
        let mut req = request();
        req.write = true;
        let args = args_of(&spec_with(&[]), &req);

        let mode = position(&args, "--permission-mode");
        assert_eq!(args[mode + 1], "acceptEdits");
        assert!(!args.iter().any(|a| a == "--disallowedTools"));
        assert!(args.iter().all(|a| a != "plan"));
        assert!(args.iter().all(|a| a != "bypassPermissions"));
        assert!(args.iter().all(|a| a != "--dangerously-skip-permissions"));
    }

    #[test]
    fn write_mode_pre_grants_shell_tools_so_bash_actually_runs() {
        // Without `--allowedTools`, `-p` denies every prompting tool and the
        // CLI reports `permission_denials: ["Bash"]` instead of executing.
        let mut req = request();
        req.write = true;
        let args = args_of(&spec_with(&[]), &req);

        let allowed = position(&args, "--allowedTools");
        let granted = &args[allowed + 1..];
        assert!(
            granted.iter().any(|t| t == "Bash"),
            "Bash must be pre-granted in write mode: {args:?}"
        );
        assert!(granted.iter().any(|t| t == "Edit"));
        assert!(granted.iter().any(|t| t == "Write"));

        // The variadic list must stop before the prompt, which `--` guards.
        let sep = position(&args, "--");
        assert!(allowed < sep, "allowedTools must come before `--`");
        assert_eq!(args.last().unwrap(), "explain the store module");
    }

    #[test]
    fn read_only_mode_grants_nothing() {
        let args = args_of(&spec_with(&[]), &request());
        assert!(
            !args.iter().any(|a| a == "--allowedTools"),
            "read-only turns must not pre-grant tools: {args:?}"
        );
    }

    #[test]
    fn resume_is_dropped_when_the_binary_does_not_advertise_it() {
        let mut req = request();
        req.resume_key = Some("abc-123".to_string());
        let args = args_with(&spec_with(&[]), &req, &probe_with(&[]));
        assert!(!args.iter().any(|a| a == "--resume"), "{args:?}");
    }

    #[test]
    fn model_system_turns_resume_and_extra_args_come_before_prompt() {
        let mut req = request();
        req.model = Some("sonnet".to_string());
        req.system = Some("be terse".to_string());
        req.max_turns = Some(7);
        req.resume_key = Some("abc-123".to_string());
        let spec = spec_with(&["--add-dir", "/tmp/other"]);
        // `--resume` is only emitted when the binary advertises it.
        let probe = probe_with(&["--resume"]);

        let args = args_with(&spec, &req, &probe);

        assert_eq!(args[position(&args, "--model") + 1], "sonnet");
        assert_eq!(
            args[position(&args, "--append-system-prompt") + 1],
            "be terse"
        );
        assert_eq!(args[position(&args, "--max-turns") + 1], "7");
        assert_eq!(args[position(&args, "--resume") + 1], "abc-123");

        // User flags are appended after ours, still ahead of the prompt.
        let extra = position(&args, "--add-dir");
        assert_eq!(args[extra + 1], "/tmp/other");
        assert!(extra < args.len() - 2);

        // The prompt is the last argument, guarded by `--` so the variadic
        // `--disallowedTools` cannot swallow it.
        assert_eq!(args.last().unwrap(), "explain the store module");
        assert_eq!(args[args.len() - 2], "--");
    }

    #[test]
    fn empty_optional_fields_are_omitted() {
        let mut req = request();
        req.model = Some("   ".to_string());
        req.system = Some(String::new());
        req.resume_key = Some(String::new());
        let args = args_of(&spec_with(&[]), &req);

        assert!(!args.iter().any(|a| a == "--model"));
        assert!(!args.iter().any(|a| a == "--append-system-prompt"));
        assert!(!args.iter().any(|a| a == "--resume"));
        assert!(!args.iter().any(|a| a == "--max-turns"));
    }

    #[test]
    fn init_line_emits_session_key() {
        let mut st = CliStreamState::default();
        let event = ClaudeBackend.parse_line(INIT_LINE, &mut st).unwrap();

        match event {
            Some(CliEvent::SessionKey(key)) => {
                assert_eq!(key, "733f80f1-74ea-40ee-8ac9-f23edffb6c82");
            }
            other => panic!("expected SessionKey, got {other:?}"),
        }
        assert_eq!(
            st.session_key.as_deref(),
            Some("733f80f1-74ea-40ee-8ac9-f23edffb6c82")
        );
    }

    #[test]
    fn assistant_text_line_emits_text() {
        let mut st = CliStreamState::default();
        match ClaudeBackend
            .parse_line(ASSISTANT_TEXT_LINE, &mut st)
            .unwrap()
        {
            Some(CliEvent::Text(text)) => assert_eq!(text, "hi"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn assistant_thinking_line_emits_thinking() {
        let mut st = CliStreamState::default();
        match ClaudeBackend
            .parse_line(ASSISTANT_THINKING_LINE, &mut st)
            .unwrap()
        {
            Some(CliEvent::Thinking(text)) => {
                assert_eq!(text, "let me look at the file first")
            }
            other => panic!("expected Thinking, got {other:?}"),
        }
    }

    #[test]
    fn assistant_tool_use_shows_the_recognisable_argument() {
        let mut st = CliStreamState::default();
        match ClaudeBackend
            .parse_line(ASSISTANT_BASH_LINE, &mut st)
            .unwrap()
        {
            Some(CliEvent::ToolActivity { name, detail }) => {
                assert_eq!(name, "Bash");
                assert_eq!(detail, "cargo test --workspace");
            }
            other => panic!("expected ToolActivity, got {other:?}"),
        }
        match ClaudeBackend
            .parse_line(ASSISTANT_READ_LINE, &mut st)
            .unwrap()
        {
            Some(CliEvent::ToolActivity { name, detail }) => {
                assert_eq!(name, "Read");
                assert_eq!(detail, "/tmp/claudetest/foo.txt");
            }
            other => panic!("expected ToolActivity, got {other:?}"),
        }
    }

    #[test]
    fn tool_use_never_becomes_a_bebok_tool_call() {
        // A CLI's tool use is presentation only: the parse surface has no
        // variant that the agent loop would execute.
        let mut st = CliStreamState::default();
        let event = ClaudeBackend
            .parse_line(ASSISTANT_BASH_LINE, &mut st)
            .unwrap();
        assert!(matches!(event, Some(CliEvent::ToolActivity { .. })));
    }

    #[test]
    fn user_tool_result_line_emits_tool_activity() {
        let mut st = CliStreamState::default();
        match ClaudeBackend
            .parse_line(USER_TOOL_RESULT_LINE, &mut st)
            .unwrap()
        {
            Some(CliEvent::ToolActivity { name, detail }) => {
                assert_eq!(name, "tool_result");
                assert_eq!(detail, "1\thello\n2");
            }
            other => panic!("expected ToolActivity, got {other:?}"),
        }

        // Array content is joined from its text blocks.
        match ClaudeBackend
            .parse_line(USER_TOOL_RESULT_BLOCKS_LINE, &mut st)
            .unwrap()
        {
            Some(CliEvent::ToolActivity { detail, .. }) => {
                assert_eq!(detail, "line one\nline two")
            }
            other => panic!("expected ToolActivity, got {other:?}"),
        }
    }

    #[test]
    fn result_line_emits_usage_with_cache_and_cost() {
        let mut st = CliStreamState::default();
        match ClaudeBackend.parse_line(RESULT_LINE, &mut st).unwrap() {
            Some(CliEvent::Usage(usage)) => {
                assert_eq!(usage.input_tokens, 2);
                assert_eq!(usage.output_tokens, 4);
                assert_eq!(usage.cost, Some(0.21992));
                assert_eq!(usage.cache_read_input_tokens, Some(7));
                assert_eq!(usage.cache_creation_input_tokens, Some(21981));
            }
            other => panic!("expected Usage, got {other:?}"),
        }
        assert_eq!(
            st.session_key.as_deref(),
            Some("733f80f1-74ea-40ee-8ac9-f23edffb6c82")
        );
    }

    #[test]
    fn unknown_event_types_and_garbage_are_ignored() {
        let mut st = CliStreamState::default();

        assert!(
            ClaudeBackend
                .parse_line(RATE_LIMIT_LINE, &mut st)
                .unwrap()
                .is_none()
        );
        assert!(
            ClaudeBackend
                .parse_line(
                    r#"{"type":"stream_event","event":{"type":"ping"}}"#,
                    &mut st
                )
                .unwrap()
                .is_none()
        );
        assert!(
            ClaudeBackend
                .parse_line("Warning: something happened", &mut st)
                .unwrap()
                .is_none()
        );
        assert!(
            ClaudeBackend
                .parse_line("[1,2,3]", &mut st)
                .unwrap()
                .is_none()
        );
        assert!(ClaudeBackend.parse_line("   ", &mut st).unwrap().is_none());
        // A half-written line (the runner hands us whole lines, but be safe).
        assert!(
            ClaudeBackend
                .parse_line(r#"{"type":"assistant","#, &mut st)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn half_formed_events_do_not_panic() {
        let mut st = CliStreamState::default();

        // Init without a usable session id.
        assert!(
            ClaudeBackend
                .parse_line(r#"{"type":"system","subtype":"init"}"#, &mut st)
                .unwrap()
                .is_none()
        );
        // Assistant without a content array.
        assert!(
            ClaudeBackend
                .parse_line(r#"{"type":"assistant","message":{}}"#, &mut st)
                .unwrap()
                .is_none()
        );
        // Empty text block yields nothing rather than an empty message part.
        assert!(
            ClaudeBackend
                .parse_line(
                    r#"{"type":"assistant","message":{"content":[{"type":"text","text":"  "}]}}"#,
                    &mut st
                )
                .unwrap()
                .is_none()
        );
        // Result without usage still reports zeroed counters.
        match ClaudeBackend
            .parse_line(r#"{"type":"result","subtype":"error_max_turns"}"#, &mut st)
            .unwrap()
        {
            Some(CliEvent::Usage(usage)) => {
                assert_eq!(usage.input_tokens, 0);
                assert_eq!(usage.cost, None);
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn unknown_tool_falls_back_to_compact_input() {
        let mut st = CliStreamState::default();
        let long = "x".repeat(400);
        let line = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"SomeNewTool","input":{{"blob":"{long}"}}}}]}}}}"#
        );
        match ClaudeBackend.parse_line(&line, &mut st).unwrap() {
            Some(CliEvent::ToolActivity { name, detail }) => {
                assert_eq!(name, "SomeNewTool");
                assert!(detail.starts_with("{\"blob\":\"xxx"));
                assert!(detail.len() <= DETAIL_CAP + 3);
            }
            other => panic!("expected ToolActivity, got {other:?}"),
        }
    }

    #[test]
    fn block_priority_prefers_text_then_thinking_then_tool_use() {
        let mut st = CliStreamState::default();
        let mixed = r#"{"type":"assistant","message":{"content":[
            {"type":"tool_use","name":"Read","input":{"file_path":"a.rs"}},
            {"type":"thinking","thinking":"hmm"},
            {"type":"text","text":"answer"}
        ]}}"#;
        match ClaudeBackend.parse_line(mixed, &mut st).unwrap() {
            Some(CliEvent::Text(text)) => assert_eq!(text, "answer"),
            other => panic!("expected Text, got {other:?}"),
        }

        let no_text = r#"{"type":"assistant","message":{"content":[
            {"type":"tool_use","name":"Read","input":{"file_path":"a.rs"}},
            {"type":"thinking","thinking":"hmm"}
        ]}}"#;
        match ClaudeBackend.parse_line(no_text, &mut st).unwrap() {
            Some(CliEvent::Thinking(text)) => assert_eq!(text, "hmm"),
            other => panic!("expected Thinking, got {other:?}"),
        }
    }

    // --- 1.1: prompt on stdin, system prompt by file, argv fallback ---

    #[test]
    fn stream_input_keeps_the_transcript_off_the_command_line() {
        let probe = probe_with(&["stream-input"]);
        let mut req = request();
        req.system = Some("be terse".to_string());
        let args = args_with(&spec_with(&[]), &req, &probe);

        assert_eq!(args[position(&args, "--input-format") + 1], "stream-json");
        assert!(
            !args.iter().any(|a| a.contains("explain the store module")),
            "prompt leaked into argv: {args:?}"
        );
        assert!(!args.iter().any(|a| a == "--"), "{args:?}");
        // No file flag advertised, so the (short) system prompt stays on argv.
        assert_eq!(
            args[position(&args, "--append-system-prompt") + 1],
            "be terse"
        );
    }

    #[test]
    fn stdin_payload_is_one_user_message_with_text_and_image_blocks() {
        let probe = probe_with(&["stream-input"]);
        let mut req = request();
        req.blocks = vec![
            PromptBlock::Text("look".to_string()),
            PromptBlock::Image {
                media_type: "image/png".to_string(),
                data: "AAAA".to_string(),
            },
        ];
        let bytes = ClaudeBackend
            .stdin_payload(&req, &probe)
            .expect("stream-json payload");
        assert_eq!(bytes.last(), Some(&b'\n'));

        let value: Value = serde_json::from_slice(&bytes).expect("valid JSON line");
        assert_eq!(value["type"], "user");
        assert_eq!(value["message"]["role"], "user");
        let content = &value["message"]["content"];
        assert_eq!(content[0], json!({ "type": "text", "text": "look" }));
        assert_eq!(
            content[1],
            json!({
                "type": "image",
                "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" }
            })
        );
    }

    #[test]
    fn without_stream_input_the_prompt_stays_on_argv_after_the_separator() {
        let probe = probe_with(&[]);
        let args = args_with(&spec_with(&[]), &request(), &probe);
        assert!(!args.iter().any(|a| a == "--input-format"));
        assert_eq!(args[args.len() - 2], "--");
        assert_eq!(args.last().unwrap(), "explain the store module");
        assert!(ClaudeBackend.stdin_payload(&request(), &probe).is_none());
    }

    #[test]
    fn oversized_prompt_without_stdin_is_a_config_error_not_e2big() {
        let mut req = request();
        req.prompt = "x".repeat(ARGV_TEXT_BUDGET + 1);
        let mut scratch = TurnScratch::default();
        let err = ClaudeBackend
            .build_command(&spec_with(&[]), &req, &CliProbe::default(), &mut scratch)
            .unwrap_err();
        assert!(
            matches!(&err, CliError::Config(msg) if msg.contains("command line")),
            "{err:?}"
        );
    }

    #[test]
    fn system_prompt_goes_through_a_file_when_the_binary_supports_it() {
        let probe = probe_with(&["system-file"]);
        let mut req = request();
        req.system = Some("be terse".to_string());
        // The managed profile owns the prompt by default; the scratch-file path
        // belongs to the `user` mode.
        let mut spec = spec_with(&[]);
        spec.profile = ProfileMode::User;
        let mut scratch = TurnScratch::default();
        let (_, args) = ClaudeBackend
            .build_command(&spec, &req, &probe, &mut scratch)
            .expect("builds");

        assert!(!args.iter().any(|a| a == "--append-system-prompt"));
        let path = PathBuf::from(&args[position(&args, "--append-system-prompt-file") + 1]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "be terse");
        drop(scratch);
        assert!(!path.exists(), "the turn file must go with the turn");
    }

    #[test]
    fn write_mode_never_emits_bypass_permissions_with_stream_input() {
        let probe = probe_with(&["stream-input", "partial", "system-file"]);
        let mut req = request();
        req.write = true;
        let args = args_with(&spec_with(&[]), &req, &probe);
        assert!(args.iter().all(|a| a != "bypassPermissions"));
        assert!(args.iter().all(|a| a != "--dangerously-skip-permissions"));
    }

    // --- 1.3: token streaming ---

    #[test]
    fn partial_messages_are_requested_only_when_advertised() {
        let spec = spec_with(&[]);
        let plain = args_with(&spec, &request(), &probe_with(&[]));
        assert!(!plain.iter().any(|a| a == "--include-partial-messages"));
        let partial = args_with(&spec, &request(), &probe_with(&["partial"]));
        assert!(partial.iter().any(|a| a == "--include-partial-messages"));
    }

    #[test]
    fn streamed_deltas_are_shown_once_even_when_the_full_message_follows() {
        let mut st = CliStreamState::default();
        let lines = [
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_9","usage":{"input_tokens":5}}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
            r#"{"type":"assistant","message":{"id":"msg_9","role":"assistant","content":[{"type":"text","text":"Hello"}],"usage":{"input_tokens":5,"cache_read_input_tokens":10,"output_tokens":2}}}"#,
        ];
        let mut texts = Vec::new();
        for line in lines {
            if let Some(CliEvent::Text(text)) = ClaudeBackend.parse_line(line, &mut st).unwrap() {
                texts.push(text);
            }
        }
        assert_eq!(texts, vec!["Hel".to_string(), "lo".to_string()]);
    }

    #[test]
    fn tool_use_in_a_partially_streamed_message_is_still_shown() {
        let mut st = CliStreamState::default();
        let deltas = [
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_3"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Looking"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
        ];
        for line in deltas {
            ClaudeBackend.parse_line(line, &mut st).unwrap();
        }
        let full = r#"{"type":"assistant","message":{"id":"msg_3","content":[{"type":"text","text":"Looking"},{"type":"tool_use","name":"Read","input":{"file_path":"a.rs"}}]}}"#;
        match ClaudeBackend.parse_line(full, &mut st).unwrap() {
            Some(CliEvent::ToolActivity { name, detail }) => {
                assert_eq!(name, "Read");
                assert_eq!(detail, "a.rs");
            }
            other => panic!("expected ToolActivity, got {other:?}"),
        }
    }

    // --- 1.2: failed and denied runs ---

    #[test]
    fn error_results_emit_usage_first_and_then_the_failure() {
        let cases = [
            (
                r#"{"type":"result","subtype":"error_max_turns","is_error":true,"total_cost_usd":0.4,"usage":{"input_tokens":3,"output_tokens":9}}"#,
                "extra.max_turns",
            ),
            (
                r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"API overloaded","usage":{"input_tokens":1,"output_tokens":1}}"#,
                "API overloaded",
            ),
            (
                r#"{"type":"result","subtype":"error_something_new","is_error":false}"#,
                "error_something_new",
            ),
            (
                r#"{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key"}"#,
                "Invalid API key",
            ),
        ];
        for (line, expected) in cases {
            let mut st = CliStreamState::default();
            let primary = ClaudeBackend.parse_line(line, &mut st).unwrap();
            assert!(
                matches!(primary, Some(CliEvent::Usage(_))),
                "usage must come first: {line}"
            );
            let failure = st
                .failure
                .take()
                .unwrap_or_else(|| panic!("no failure reported for {line}"));
            assert!(failure.contains(expected), "{failure}");
        }
    }

    #[test]
    fn max_turns_failure_keeps_the_cost_of_the_run() {
        let line = r#"{"type":"result","subtype":"error_max_turns","is_error":true,"total_cost_usd":0.4,"usage":{"input_tokens":3,"output_tokens":9}}"#;
        let mut st = CliStreamState::default();
        match ClaudeBackend.parse_line(line, &mut st).unwrap() {
            Some(CliEvent::Usage(usage)) => assert_eq!(usage.cost, Some(0.4)),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn permission_denials_are_noted_on_a_successful_run() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"permission_denials":[{"tool_name":"Bash"},{"tool_name":"Bash"},{"tool_name":"Edit"}],"usage":{"input_tokens":1,"output_tokens":1}}"#;
        let mut st = CliStreamState::default();
        assert!(matches!(
            ClaudeBackend.parse_line(line, &mut st).unwrap(),
            Some(CliEvent::Usage(_))
        ));
        match st.pending.pop_front() {
            Some(CliEvent::Thinking(note)) => {
                assert_eq!(note, "⚠ Claude Code could not run: Bash, Edit")
            }
            other => panic!("expected a denial note, got {other:?}"),
        }
        assert!(st.failure.is_none(), "denials alone are not a failure");
    }

    // --- 1.4: context size and catalog ids ---

    #[test]
    fn result_context_is_the_last_api_call_not_the_turn_total() {
        let mut st = CliStreamState::default();
        ClaudeBackend
            .parse_line(
                r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Read","input":{"file_path":"a"}}],"usage":{"input_tokens":4,"cache_read_input_tokens":20000}}}"#,
                &mut st,
            )
            .unwrap();
        ClaudeBackend
            .parse_line(
                r#"{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"done"}],"usage":{"input_tokens":3,"cache_read_input_tokens":21000,"cache_creation_input_tokens":500}}}"#,
                &mut st,
            )
            .unwrap();
        match ClaudeBackend
            .parse_line(
                r#"{"type":"result","subtype":"success","usage":{"input_tokens":7,"cache_read_input_tokens":41000}}"#,
                &mut st,
            )
            .unwrap()
        {
            Some(CliEvent::Usage(usage)) => {
                assert_eq!(usage.context_tokens, Some(21_503));
                assert_eq!(usage.input_tokens, 7, "stats keep the turn total");
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn catalog_model_maps_exact_ids_and_aliases() {
        let backend = ClaudeBackend;
        assert_eq!(
            backend
                .catalog_model("sonnet", Some("claude-sonnet-5"))
                .as_deref(),
            Some("anthropic/claude-sonnet-5")
        );
        // Unknown init id: the alias still resolves to the newest of its family.
        assert_eq!(
            backend
                .catalog_model("opus", Some("claude-future-9"))
                .as_deref(),
            Some("anthropic/claude-opus-5")
        );
        assert_eq!(
            backend.catalog_model("haiku", None).as_deref(),
            Some("anthropic/claude-haiku-4-5")
        );
        assert_eq!(
            backend.catalog_model("claude-opus-4-6", None).as_deref(),
            Some("anthropic/claude-opus-4-6")
        );
        assert_eq!(backend.catalog_model("gpt-x", None), None);
    }

    // --- 07A: managed profile ---

    /// Flags a claude 2.1.325-style binary advertises for the managed profile.
    const PROFILE_FLAGS: [&str; 5] = [
        "--settings",
        "--setting-sources",
        "--mcp-config",
        "--strict-mcp-config",
        "system-file",
    ];

    /// Render against a fixed, non-existent project (so no project layer is
    /// read) and a made-up profile directory.
    fn render_with(
        spec: &CliSpec,
        req: &CliRequest,
        probe: &CliProbe,
    ) -> Result<RenderedProfile, CliError> {
        let dir = PathBuf::from("/profiles/claude");
        ClaudeBackend.render(&ProfileInput {
            spec,
            req,
            probe,
            dir: &dir,
        })
    }

    fn file_of<'a>(out: &'a RenderedProfile, name: &str) -> &'a str {
        out.files
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.contents.as_str())
            .unwrap_or_else(|| panic!("{name} was not rendered"))
    }

    /// A fresh project directory under the temp dir; removed by the caller.
    fn temp_project(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bebok-claude-profile-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_project_file(project: &Path, name: &str, contents: &str) {
        let path = crate::profile::project_layer_path(project, "claude", name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn read_only_profile_golden_settings_and_argv() {
        let probe = probe_with(&PROFILE_FLAGS);
        let out = render_with(&spec_with(&[]), &request(), &probe).expect("renders");

        let expected = json!({
            "env": {
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                "DISABLE_AUTOUPDATER": "1",
                "DISABLE_TELEMETRY": "1",
            },
            // Bebok's hook entries are exercised on their own below; the
            // command embeds this test binary's path, so it is not golden.
            "hooks": hook_settings(),
            "includeCoAuthoredBy": false,
            "permissions": {
                "allow": [],
                "defaultMode": "plan",
                "deny": ["Edit", "Write", "Bash"],
            },
        });
        // Compared as values: key order is not part of the contract.
        let settings: Value = serde_json::from_str(file_of(&out, "settings.json")).unwrap();
        assert_eq!(settings, expected);
        assert!(file_of(&out, "settings.json").ends_with('\n'));
        assert_eq!(file_of(&out, "mcp.json"), "{\n  \"mcpServers\": {}\n}\n");
        assert!(
            out.files.iter().all(|f| f.name != "system.md"),
            "no system text, no file"
        );

        let dir = "/profiles/claude";
        assert_eq!(
            out.args,
            vec![
                "--settings".to_string(),
                format!("{dir}/settings.json"),
                "--setting-sources".into(),
                "user".into(),
                "--mcp-config".into(),
                format!("{dir}/mcp.json"),
                "--strict-mcp-config".into(),
            ]
        );
        assert!(
            out.env.is_empty(),
            "only isolated mode sets CLAUDE_CONFIG_DIR"
        );
        assert_eq!(
            out.version, None,
            "probe built from help alone has no version"
        );
    }

    #[test]
    fn write_mode_maps_yolo_to_accept_edits_and_never_bypasses() {
        let probe = probe_with(&PROFILE_FLAGS);
        let mut req = request();
        req.write = true;
        let out = render_with(&spec_with(&[]), &req, &probe).expect("renders");
        let settings: Value = serde_json::from_str(file_of(&out, "settings.json")).unwrap();
        assert_eq!(settings["permissions"]["defaultMode"], "acceptEdits");
        assert!(
            settings["permissions"]["allow"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t == "Bash")
        );
        assert!(!file_of(&out, "settings.json").contains("bypassPermissions"));
    }

    #[test]
    fn managed_profile_without_settings_flag_is_a_config_error() {
        // Empty probe: the help did not answer.
        let err = render_with(&spec_with(&[]), &request(), &CliProbe::default()).unwrap_err();
        assert!(
            matches!(&err, CliError::Config(m) if m.contains("--settings")),
            "{err:?}"
        );
    }

    #[test]
    fn optional_profile_flags_are_gated_by_the_probe() {
        let probe = probe_with(&["--settings"]);
        let out = render_with(&spec_with(&[]), &request(), &probe).expect("renders");
        assert_eq!(
            out.args,
            vec![
                "--settings".to_string(),
                "/profiles/claude/settings.json".to_string()
            ]
        );
        assert!(out.files.iter().all(|f| f.name != "mcp.json"));
        assert_eq!(out.flags, vec!["--settings".to_string()]);
    }

    #[test]
    fn user_mode_renders_nothing_without_overrides() {
        let mut spec = spec_with(&[]);
        spec.profile = ProfileMode::User;
        let out = render_with(&spec, &request(), &CliProbe::default()).expect("renders");
        assert!(out.files.is_empty() && out.args.is_empty() && out.env.is_empty());
    }

    #[test]
    fn user_mode_with_overrides_writes_only_the_settings_layer() {
        let mut spec = spec_with(&[]);
        spec.profile = ProfileMode::User;
        spec.profile_overrides = Some(json!({ "env": { "FOO": "1" } }));
        let out = render_with(&spec, &request(), &probe_with(&["--settings"])).expect("renders");
        assert_eq!(out.files.len(), 1);
        assert_eq!(out.files[0].name, "settings.json");
        assert_eq!(
            file_of(&out, "settings.json"),
            to_json_file(&json!({ "env": { "FOO": "1" } })).unwrap()
        );
    }

    #[test]
    fn profile_overrides_cannot_switch_on_bypass_permissions() {
        let mut spec = spec_with(&[]);
        spec.profile_overrides =
            Some(json!({ "permissions": { "defaultMode": "bypassPermissions" } }));
        let err = render_with(&spec, &request(), &probe_with(&PROFILE_FLAGS)).unwrap_err();
        assert!(matches!(err, CliError::Config(_)), "{err:?}");
    }

    #[test]
    fn layers_apply_in_order_and_the_project_file_wins() {
        let project = temp_project("layers");
        write_project_file(
            &project,
            "settings.json",
            "{\"env\":{\"DISABLE_TELEMETRY\":\"1\"}}",
        );
        let mut spec = spec_with(&[]);
        spec.profile_overrides = Some(json!({ "env": { "DISABLE_TELEMETRY": "0", "KEEP": "x" } }));
        let mut req = request();
        req.cwd = project.clone();

        let out = render_with(&spec, &req, &probe_with(&["--settings"])).expect("renders");
        let settings: Value = serde_json::from_str(file_of(&out, "settings.json")).unwrap();
        assert_eq!(
            settings["env"]["DISABLE_TELEMETRY"], "1",
            "project layer wins"
        );
        assert_eq!(settings["env"]["KEEP"], "x", "override survives the merge");
        assert_eq!(
            settings["env"]["DISABLE_AUTOUPDATER"], "1",
            "Bebok base survives"
        );
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn project_layer_that_is_not_an_object_is_refused() {
        let project = temp_project("not-object");
        write_project_file(&project, "settings.json", "[1,2]");
        let mut req = request();
        req.cwd = project.clone();
        let err = render_with(&spec_with(&[]), &req, &probe_with(&PROFILE_FLAGS)).unwrap_err();
        assert!(
            matches!(&err, CliError::Config(m) if m.contains("JSON object")),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn project_system_append_follows_the_turn_prompt() {
        let project = temp_project("append");
        write_project_file(&project, "system.append.md", "project rule");
        let mut req = request();
        req.cwd = project.clone();
        req.system = Some("turn prompt".into());

        let out = render_with(&spec_with(&[]), &req, &probe_with(&PROFILE_FLAGS)).expect("renders");
        assert_eq!(file_of(&out, "system.md"), "turn prompt\n\nproject rule\n");
        assert!(
            out.args
                .contains(&"--append-system-prompt-file".to_string())
        );
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn managed_build_command_leaves_the_system_prompt_to_the_profile() {
        let mut req = request();
        req.system = Some("be terse".into());
        let probe = probe_with(&["system-file"]);

        let managed = args_with(&spec_with(&[]), &req, &probe);
        assert!(
            !managed
                .iter()
                .any(|a| a == "--append-system-prompt" || a == "--append-system-prompt-file")
        );

        let mut user = spec_with(&[]);
        user.profile = ProfileMode::User;
        let legacy = args_with(&user, &req, &probe);
        assert!(legacy.iter().any(|a| a == "--append-system-prompt-file"));
    }

    #[test]
    fn replace_mode_needs_the_replace_flag_and_uses_it_when_present() {
        let mut spec = spec_with(&[]);
        spec.system_prompt_mode = SystemPromptMode::Replace;
        let mut req = request();
        req.system = Some("only this".into());

        let err = render_with(&spec, &req, &probe_with(&["--settings"])).unwrap_err();
        assert!(
            matches!(&err, CliError::Config(m) if m.contains("--system-prompt-file")),
            "{err:?}"
        );

        let out = render_with(
            &spec,
            &req,
            &probe_with(&["--settings", "--system-prompt-file"]),
        )
        .expect("renders");
        assert!(out.args.contains(&"--system-prompt-file".to_string()));
        assert!(
            !out.args
                .contains(&"--append-system-prompt-file".to_string())
        );
    }

    #[test]
    fn isolated_mode_points_the_config_dir_at_the_profile() {
        let mut spec = spec_with(&[]);
        spec.profile = ProfileMode::Isolated;
        let out = render_with(&spec, &request(), &probe_with(&PROFILE_FLAGS)).expect("renders");
        assert_eq!(
            out.env,
            vec![(
                "CLAUDE_CONFIG_DIR".to_string(),
                "/profiles/claude".to_string()
            )]
        );
        assert!(
            !out.args.contains(&"--setting-sources".to_string()),
            "isolated reads only its own dir"
        );
    }

    #[test]
    fn stream_input_is_detected_from_the_flag_and_any_case_of_the_value() {
        let probe = CliProbe::new("  --input-format <format>  Input format: Stream-Json\n".into());
        let args = args_of(&spec_with(&[]), &request());
        assert!(
            !args.iter().any(|a| a == "--input-format"),
            "baseline has no probe"
        );
        let with = args_with(&spec_with(&[]), &request(), &probe);
        let at = position(&with, "--input-format");
        assert_eq!(with[at + 1], "stream-json");
    }

    #[test]
    fn managed_hooks_gate_every_tool_call() {
        let base = managed_base(false);
        let hooks = &base["hooks"];
        for event in ["PreToolUse", "PostToolUse"] {
            let entry = &hooks[event][0];
            assert_eq!(entry["matcher"], "*");
            assert_eq!(entry["hooks"][0]["type"], "command");
            assert!(
                entry["hooks"][0]["timeout"].is_number(),
                "{event} needs an explicit timeout"
            );
        }
        // The pre hook must outlive Bebok's own answer deadline.
        assert!(
            hooks["PreToolUse"][0]["hooks"][0]["timeout"]
                .as_u64()
                .unwrap()
                > 60
        );
        // Stop takes no matcher (it is not a tool event).
        assert!(hooks["Stop"][0].get("matcher").is_none());

        let commands: Vec<String> = ["PreToolUse", "PostToolUse", "Stop"]
            .iter()
            .map(|e| {
                hooks[*e][0]["hooks"][0]["command"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert!(commands[0].contains("cli-hook pre"), "{}", commands[0]);
        assert!(commands[1].contains("cli-hook post"), "{}", commands[1]);
        assert!(commands[2].contains("cli-hook stop"), "{}", commands[2]);
        for command in &commands {
            // Quoted executable: the path may contain spaces.
            assert!(command.starts_with('"'), "{command}");
            assert!(command.contains("\" cli-hook "), "{command}");
        }
    }

    #[test]
    fn prepare_writes_the_profile_once_and_reuses_it() {
        let tmp = temp_project("prepare");
        let profile = ProfileDir {
            dir: Some(tmp.join("profile")),
        };
        let probe = probe_with(&PROFILE_FLAGS);
        let spec = spec_with(&[]);
        let req = request();

        let first = ClaudeBackend
            .prepare(&spec, &req, &probe, &profile)
            .expect("prepares");
        let settings_path = tmp.join("profile").join("settings.json");
        assert!(
            first
                .args_extra
                .contains(&settings_path.display().to_string())
        );
        let mtime = std::fs::metadata(&settings_path)
            .unwrap()
            .modified()
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        let second = ClaudeBackend
            .prepare(&spec, &req, &probe, &profile)
            .expect("prepares");
        assert_eq!(first.args_extra, second.args_extra);
        assert_eq!(
            std::fs::metadata(&settings_path)
                .unwrap()
                .modified()
                .unwrap(),
            mtime
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
