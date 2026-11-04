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

use serde_json::Value;

use crate::backend::{CliBackend, CliCapabilities, CliError, CliEvent, CliRequest, CliStreamState};
use crate::spec::CliSpec;

/// How much of a tool input / result we keep for the one-line UI row: long
/// enough to recognise the call, short enough not to flood the transcript.
const DETAIL_CAP: usize = 200;

/// Tools that can mutate the workspace. Denied explicitly in read-only mode in
/// addition to `--permission-mode plan`: permission *modes* are a policy of the
/// CLI and have changed between releases, while an explicit tool denial is
/// enforced by its tool dispatcher.
const MUTATING_TOOLS: [&str; 3] = ["Edit", "Write", "Bash"];

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
            // The CLI takes no inline image attachments; it reads images from
            // disk paths mentioned in the prompt.
            images: false,
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

    /// Build `claude <flags> -- <prompt>`.
    ///
    /// The program is the bare `"claude"` name: path resolution
    /// (`extra.command`, [`Self::env_override`], `PATH`) belongs to the runner,
    /// which is also the place that reports [`CliError::BinaryMissing`] with the
    /// right env var.
    ///
    /// The prompt is a **positional argument**, not stdin. The runner closes
    /// the child's stdin, and `claude -p` waits for piped input when stdin is
    /// open (then aborts after a few seconds), so the rendered transcript has
    /// to travel on the command line.
    fn build_command(&self, spec: &CliSpec, req: &CliRequest) -> (String, Vec<String>) {
        // Non-interactive streaming. `--verbose` is not optional:
        // `-p --output-format stream-json` is rejected without it.
        let mut args: Vec<String> = vec![
            "-p".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
        ];

        if let Some(model) = non_empty(req.model.as_deref()) {
            args.push("--model".to_string());
            args.push(model.to_string());
        }

        if self.capabilities().system_prompt
            && let Some(system) = non_empty(req.system.as_deref())
        {
            args.push("--append-system-prompt".to_string());
            args.push(system.to_string());
        }

        if let Some(turns) = req.max_turns {
            args.push("--max-turns".to_string());
            args.push(turns.to_string());
        }

        if let Some(key) = non_empty(req.resume_key.as_deref()) {
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
        if req.write {
            args.push("--permission-mode".to_string());
            args.push("acceptEdits".to_string());
        } else {
            args.push("--permission-mode".to_string());
            args.push("plan".to_string());
            args.push("--disallowedTools".to_string());
            args.extend(MUTATING_TOOLS.iter().map(|t| (*t).to_string()));
        }

        // User flags last, so they can override our defaults (their own
        // `--permission-mode`, an extra `--add-dir`, ...).
        args.extend(spec.args.iter().cloned());

        // `--disallowedTools` takes a *variadic* value list, so a trailing bare
        // positional would be swallowed as another tool name (verified: claude
        // then fails with "Input must be provided ..."). `--` terminates option
        // parsing and hands the prompt over intact; with it the prompt is still
        // the last argument.
        args.push("--".to_string());
        args.push(req.prompt.clone());

        ("claude".to_string(), args)
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

            "assistant" => Ok(assistant_event(&value)),

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
                Ok(Some(CliEvent::Usage(bebok_llm::Usage {
                    input_tokens: number("input_tokens").unwrap_or(0),
                    output_tokens: number("output_tokens").unwrap_or(0),
                    cost: value.get("total_cost_usd").and_then(Value::as_f64),
                    cache_read_input_tokens: number("cache_read_input_tokens"),
                    cache_creation_input_tokens: number("cache_creation_input_tokens"),
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
/// A message carries an ordered `content` array. We pick the first block that
/// yields a non-empty event, preferring prose over reasoning over tool use
/// (text → thinking → tool_use): prose is what the user reads, and when a
/// message holds both, the tool row belongs after it. In practice Claude emits
/// one block per message, so the preference rarely decides anything.
fn assistant_event(value: &Value) -> Option<CliEvent> {
    let blocks = value
        .pointer("/message/content")
        .and_then(Value::as_array)?;

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
    use crate::backend::CliMode;
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
        }
    }

    fn request() -> CliRequest {
        CliRequest {
            prompt: "explain the store module".to_string(),
            system: None,
            model: None,
            cwd: PathBuf::from("/tmp/p"),
            resume_key: None,
            write: false,
            max_turns: None,
            timeout: Duration::from_secs(120),
        }
    }

    fn args_of(spec: &CliSpec, req: &CliRequest) -> Vec<String> {
        let (program, args) = ClaudeBackend.build_command(spec, req);
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
    fn model_system_turns_resume_and_extra_args_come_before_prompt() {
        let mut req = request();
        req.model = Some("sonnet".to_string());
        req.system = Some("be terse".to_string());
        req.max_turns = Some(7);
        req.resume_key = Some("abc-123".to_string());
        let spec = spec_with(&["--add-dir", "/tmp/other"]);

        let args = args_of(&spec, &req);

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
}
