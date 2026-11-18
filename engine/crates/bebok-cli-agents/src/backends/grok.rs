//! Grok Build (`grok`) backend.
//!
//! `grok -p --output-format streaming-messages-json` speaks the same JSONL
//! dialect as Claude Code's `stream-json` (`system/init`, `assistant`, `user`
//! with `tool_result`, `stream_event` deltas, `result`), so line parsing is
//! delegated to [`ClaudeBackend`]. What differs is the command line: the
//! prompt travels as a file (`--prompt-file`, no argv limit), the system prompt
//! as `--rules`, and permissions as `--permission-mode` plus `--allow` /
//! `--disallowed-tools` rules.
//!
//! Verified against `grok` 1.0.89. Tool names differ from Claude's
//! (`run_terminal_command`, `search_replace`, `write`), and the tool input of
//! unknown tools is shown as compact JSON.
//!
//! Limitations versus the Claude backend: no managed profile / permission
//! hooks (Grok has its own `.grok/config.toml`), and no image input.

use crate::backend::{
    CliBackend, CliCapabilities, CliError, CliEvent, CliProbe, CliRequest, CliStreamState,
    TurnScratch,
};
use crate::backends::claude::ClaudeBackend;
use crate::spec::CliSpec;

/// Bytes of system prompt allowed on the command line (`--rules`).
const ARGV_TEXT_BUDGET: usize = if cfg!(windows) { 6_000 } else { 100_000 };

/// Tools that can mutate the workspace; removed in read-only mode.
const MUTATING_TOOLS: &str = "run_terminal_command,search_replace,write";

/// Grok's MCP meta-tools. Grok imports MCP servers from `~/.claude.json` and
/// `.mcp.json` (e.g. the user's own `bebok` server), but a headless run cannot
/// approve an MCP call, so every `use_tool` is cancelled ("User cancelled the
/// execution") after burning a turn. Remove them so the model stops trying.
const MCP_META_TOOLS: &str = "search_tool,use_tool";

/// Rules pre-granted on a write-enabled turn. Headless runs cannot answer a
/// prompt, so a tool that is not allowed is simply cancelled. `always-approve`
/// (`bypassPermissions`) is never used.
const WRITE_ALLOW: [&str; 3] = ["Bash", "Edit", "Write"];

pub struct GrokBackend;

impl CliBackend for GrokBackend {
    fn id(&self) -> &'static str {
        "grok"
    }

    fn label(&self) -> &'static str {
        "Grok Build"
    }

    fn capabilities(&self) -> CliCapabilities {
        CliCapabilities {
            streaming: true,
            resume: true,
            system_prompt: true,
            tool_events: true,
            usage: true,
            images: false,
            mcp: false,
        }
    }

    fn default_models(&self) -> Vec<String> {
        vec![
            "grok-4.7".to_string(),
            "grok-4.7-build-fast".to_string(),
            "grok-4.6".to_string(),
            "grok-4.5".to_string(),
        ]
    }

    fn binary_names(&self) -> &'static [&'static str] {
        &["grok"]
    }

    fn env_override(&self) -> &'static str {
        "BEBOK_GROK_BIN"
    }

    fn resume_flag_available(&self, probe: &CliProbe) -> bool {
        probe.has("--resume")
    }

    fn build_command(
        &self,
        spec: &CliSpec,
        req: &CliRequest,
        probe: &CliProbe,
        scratch: &mut TurnScratch,
    ) -> Result<(String, Vec<String>), CliError> {
        // The prompt goes through a file: no argv size limit, no quoting.
        // `--prompt-file` is the single-turn (headless) form, like `-p`.
        let prompt_path = scratch.write_file("prompt.txt", &req.prompt)?;
        let mut args: Vec<String> = vec![
            "--prompt-file".to_string(),
            prompt_path.to_string_lossy().into_owned(),
            "--output-format".to_string(),
            "streaming-messages-json".to_string(),
        ];

        if probe.has("--include-partial-messages") {
            args.push("--include-partial-messages".to_string());
        }

        if let Some(model) = req
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            args.push("--model".to_string());
            args.push(model.to_string());
        }

        if let Some(system) = req
            .system
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if system.len() > ARGV_TEXT_BUDGET {
                return Err(CliError::Config(format!(
                    "{} bytes of system prompt do not fit on the command line (limit {ARGV_TEXT_BUDGET})",
                    system.len()
                )));
            }
            args.push("--rules".to_string());
            args.push(system.to_string());
        }

        if let Some(turns) = req.max_turns {
            args.push("--max-turns".to_string());
            args.push(turns.to_string());
        }

        if let Some(key) = req
            .resume_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            && self.resume_flag_available(probe)
        {
            args.push("--resume".to_string());
            args.push(key.to_string());
        }

        // SECURITY. Same policy as the Claude backend: read-only unless the
        // user opted in with `allow_write`, and never `bypassPermissions` /
        // `--always-approve`. A headless `plan` run refuses every edit, and the
        // mutating tools are removed on top of it.
        if req.write {
            args.push("--permission-mode".to_string());
            args.push("acceptEdits".to_string());
            for rule in WRITE_ALLOW {
                args.push("--allow".to_string());
                args.push(rule.to_string());
            }
        } else {
            args.push("--permission-mode".to_string());
            args.push("plan".to_string());
            args.push("--disallowed-tools".to_string());
            args.push(format!("{MUTATING_TOOLS},{MCP_META_TOOLS}"));
        }
        if req.write {
            args.push("--disallowed-tools".to_string());
            args.push(MCP_META_TOOLS.to_string());
        }

        args.extend(spec.args.iter().cloned());
        Ok(("grok".to_string(), args))
    }

    /// Grok reports its own model ids (`grok-4.7`); map to the xAI catalog.
    fn catalog_model(&self, cli_model: &str, init_model: Option<&str>) -> Option<String> {
        let known = bebok_llm::ModelCatalog::global().provider_models("xai");
        [init_model, Some(cli_model)]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .find(|c| known.iter().any(|k| k.eq_ignore_ascii_case(c)))
            .map(|c| format!("xai/{c}"))
    }

    fn parse_line(
        &self,
        line: &str,
        st: &mut CliStreamState,
    ) -> Result<Option<CliEvent>, CliError> {
        ClaudeBackend.parse_line(line, st)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn spec() -> CliSpec {
        CliSpec::from_extra(
            serde_json::json!({ "backend": "grok" })
                .as_object()
                .unwrap(),
        )
        .unwrap()
    }

    fn request(write: bool) -> CliRequest {
        CliRequest {
            prompt: "hello".into(),
            blocks: Vec::new(),
            system: Some("be terse".into()),
            model: Some("grok-4.7".into()),
            cwd: PathBuf::from("/tmp"),
            resume_key: Some("sess-1".into()),
            write,
            max_turns: Some(5),
            timeout: Duration::from_secs(60),
            extra_env: Vec::new(),
        }
    }

    fn args(write: bool, help: &str) -> Vec<String> {
        let mut scratch = TurnScratch::default();
        GrokBackend
            .build_command(
                &spec(),
                &request(write),
                &CliProbe::new(help.into()),
                &mut scratch,
            )
            .unwrap()
            .1
    }

    #[test]
    fn read_only_is_plan_mode_without_mutating_tools() {
        let a = args(
            false,
            "  -r, --resume [<ID>]\n      --include-partial-messages\n",
        );
        assert!(a.windows(2).any(|w| w == ["--permission-mode", "plan"]));
        assert!(
            a.iter()
                .any(|x| x.starts_with(MUTATING_TOOLS) && x.contains("use_tool"))
        );
        assert!(
            !a.iter()
                .any(|x| x.contains("bypass") || x == "--always-approve")
        );
        assert!(a.windows(2).any(|w| w == ["--resume", "sess-1"]));
        assert!(a.windows(2).any(|w| w == ["--rules", "be terse"]));
        assert!(a.contains(&"--include-partial-messages".to_string()));
    }

    #[test]
    fn write_mode_accepts_edits_and_never_bypasses() {
        let a = args(true, "");
        assert!(
            a.windows(2)
                .any(|w| w == ["--permission-mode", "acceptEdits"])
        );
        assert!(a.windows(2).any(|w| w == ["--allow", "Bash"]));
        assert!(
            !a.iter()
                .any(|x| x.contains("bypass") || x == "--always-approve")
        );
        // No `--resume` / partial messages when the binary does not advertise them.
        assert!(!a.contains(&"--resume".to_string()));
        assert!(!a.contains(&"--include-partial-messages".to_string()));
    }

    #[test]
    fn the_prompt_travels_as_a_file() {
        let a = args(false, "");
        let i = a.iter().position(|x| x == "--prompt-file").unwrap();
        assert!(a[i + 1].ends_with("prompt.txt"));
        assert!(!a.contains(&"hello".to_string()));
    }

    #[test]
    fn grok_stream_is_parsed_like_claude() {
        let mut st = CliStreamState::default();
        let init = r#"{"type":"system","subtype":"init","session_id":"abc","model":"grok-4.7"}"#;
        assert!(matches!(
            GrokBackend.parse_line(init, &mut st).unwrap(),
            Some(CliEvent::SessionKey(k)) if k == "abc"
        ));
        let msg = r#"{"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"Hi"}],"usage":{"input_tokens":1,"output_tokens":1}}}"#;
        assert!(matches!(
            GrokBackend.parse_line(msg, &mut st).unwrap(),
            Some(CliEvent::Text(t)) if t == "Hi"
        ));
    }
}
