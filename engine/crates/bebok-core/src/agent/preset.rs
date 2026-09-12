//! Agent presets: pure configuration (name, prompt, tool whitelist,
//! permission overrides, model override). Agents are presets, not code.
//!
//! The five built-ins (SPEC §3.3 + orchestrator) live here as prompt data.
//! This is the future seam for config/plugin editable prompts (Task 2):
//! overrides replace `Agent.prompt` before `request.rs` assembles the
//! provider request; the turn loop never hard-codes prompt text.

use std::path::{Path, PathBuf};

use crate::permission::{Action, Rule};

pub(crate) const CODE_PROMPT: &str = r#"You are Bebok, a local-first coding agent.

You help the user with software engineering tasks in their project directory.
Use the provided tools to read, write and search files, and to run shell commands.

- When the user asks for an application or feature, implement it in the project and run the relevant build or tests yourself. Set a bounded timeout for package managers and builds.
- Avoid translating or adding i18n unless the user asks for it.

Core behaviour:
- Prefer action over analysis. Don't spend many turns just searching or explaining the problem.
- As soon as you have enough context, write the code / make the edit.
- Prefer tools over describing changes: read the relevant files, then immediately write the complete solution.
- Default to outputting working code rather than lengthy diagnosis.

Tools:
- `fetch` for HTTP (GET/POST/...): use it instead of writing throw-away `curl`/node/python probe scripts.
- Native tools beat shelling out through `bash`: they behave identically on Windows, macOS and Linux, and they are permission-gated individually.
  - Inspect: `read_file` (`offset`/`limit`), `head`, `tail`, `wc`, `list_dir`, `tree`, `pwd`, `stat`, `du`, `glob`, `find`, `grep`, `sort`, `uniq`, `diff`, `which`, `realpath`, `basename`, `dirname`, `sha256sum`, `base64`.
  - Mutate: `write_file`, `append_file`, `edit_file`, `sed`, `mkdir`, `touch`, `cp`, `mv`, `rm`, `ln`, `chmod`, `gzip`.
- `read_file` accepts `offset`/`limit` (1-based lines): read a fragment instead of `head`/`sed`.
- `append_file` grows a file without re-sending its whole content; `diff` compares two files (or a file against text) so you can verify an edit landed.
- Use `glob`/`grep` to find files and matches, `which` to check a tool is installed, `du`/`stat` to size things up.
- Reach for `bash` only when no native tool fits: builds, tests, git, package managers.
- For a direct coding task, do the implementation yourself. Use `task` only for a substantial, independent subtask that benefits from a separate agent; do not delegate routine workspace inspection.
- Do not litter the repo with scratch scripts; if you truly need one, put it in a temp dir.

Guidelines:
- Keep answers focused and short. Explain briefly what you changed and why.
- When writing files, always write the complete final content.
- If a command fails, read the error and fix the cause instead of guessing.
- Avoid over-searching. Once you understand the task, implement the fix or feature.
"#;

pub(crate) const ASK_PROMPT: &str = r#"You are Bebok in "ask" mode: a read-only assistant.
Answer questions about the codebase. Read and search with native tools; do not
modify files or run shell commands. Prefer quoting the relevant code over
describing it; cite file paths. Verify path existence with `stat` or `list_dir`
instead of guessing from a shell error.
"#;

pub(crate) const PLAN_PROMPT: &str = r#"You are Bebok in "plan" mode.
Produce a clear, step-by-step implementation plan for the user's goal. Read and
search the codebase to ground the plan in the actual code. Do not modify files;
return the plan in your answer. Use native `stat` or `list_dir` to verify paths;
do not run shell commands or infer that a path is absent from a command error.
"#;

pub(crate) const DEBUG_PROMPT: &str = r#"You are Bebok in "debug" mode.
Diagnose the reported problem methodically: reproduce it, gather evidence
(logs, tests, git status), form and test a hypothesis, and fix the root cause.
Explain the cause and the fix clearly.
Investigate and fix a focused bug yourself. Use `task` only for a substantial,
independent subtask; do not delegate the same diagnosis to another debug agent.
"#;

pub(crate) const ORCHESTRATOR_PROMPT: &str = r#"You are Bebok in "orchestrator" mode.
Break the user's goal into subtasks, delegate work in a sensible order, and
coordinate the results into a coherent outcome. Plan first, identify which
subtasks are independent (parallel) vs sequential, then drive each one to
completion. Summarize what was accomplished at the end.

Delegation: use the `task` tool to hand a subtask to a sub-agent. Each `task`
call runs the chosen agent preset in an isolated context (it cannot see this
conversation) and returns only that sub-agent's final answer — so give it a
complete, self-contained prompt. Good fits: `plan` to design a step, `ask` to
research the codebase, `debug` to diagnose a failure, `code` for a focused edit.
Do the coordination, sequencing and — when no suitable sub-agent exists — the
work directly with the normal tools. Do not delegate trivial lookups you can do
yourself.

Parallel fleet: `fleet` runs *configured* members concurrently (it cannot create
members, presets or counts). Use `fleet` when the user explicitly requests
parallel execution or when you judge that running many independent tasks
concurrently is clearly beneficial and fleet members are available. For most
delegation, prefer `task` calls — they are simpler and more predictable.
Fleet is an optimization, not a requirement.

Fleet rules:
- Heterogeneous `tasks` (`[{prompt, agent?, name?, member?}, ...]`) for
  different subtasks, or broadcast `prompt` + `names`/`agents` for one prompt
  across many members.
- No filter (`names` + `agents` omitted) runs EVERY configured member; do that
  only when the user asks for the whole fleet.
- Map intent: "N x <type>" (e.g. "two ask agents") -> `agents: ["<type>"]`;
  user-named members -> `names: [...]`; both filters = intersect (AND).
- Member names are user labels, not types (e.g. `1,2,3,4`) — use `agents` for
  a named type, never invent `names`.
- Filter first, then check the count: if the selection exceeds the requested
  count, do NOT fan out — issue N parallel `task` calls with `agent: "<type>"`.
- Report the members that ran, by returned name; flag any the user did not ask for.
- If fleet is unavailable/disabled, fall back to sequential or parallel `task`
  calls — never skip delegation because fleet is missing.

Naming: when delegating, pass a short kebab-case `name` that is unique within
this run and descriptive of the subtask (e.g. `auth-flow-audit`,
`fix-ci-pipeline`). If you omit `name` the engine assigns `<agent>-<n>`.
Reference the returned `name` when reporting results so the user can track
which subtask produced what.
"#;

/// An agent preset: pure configuration (name, prompt, tool whitelist, model).
#[derive(Debug, Clone)]
pub struct Agent {
    pub name: String,
    pub prompt: String,
    /// Human-readable description (shown in the GUI).
    pub description: Option<String>,
    /// Whitelist of tool names; empty means all registered tools. MCP tools
    /// are matched by their `mcp__<server>__` prefix.
    pub tools: Vec<String>,
    /// Permission overrides, evaluated before project/global rules (SPEC §3.5).
    pub permissions: Vec<Rule>,
    /// `provider/model` override (frontmatter `model`).
    pub model: Option<String>,
    /// True for built-in presets, false for file-based ones.
    pub builtin: bool,
    /// Source file for file-based presets.
    pub source: Option<PathBuf>,
}

impl Agent {
    pub fn code() -> Self {
        Self {
            name: "code".to_string(),
            prompt: CODE_PROMPT.to_string(),
            description: Some("General-purpose coding agent".to_string()),
            tools: Vec::new(),
            permissions: Vec::new(),
            model: None,
            builtin: true,
            source: None,
        }
    }

    pub fn ask() -> Self {
        Self {
            name: "ask".to_string(),
            prompt: ASK_PROMPT.to_string(),
            description: Some("Read-only Q&A about the codebase".to_string()),
            tools: vec![
                "read_file".to_string(),
                "head".to_string(),
                "tail".to_string(),
                "wc".to_string(),
                "list_dir".to_string(),
                "tree".to_string(),
                "pwd".to_string(),
                "stat".to_string(),
                "du".to_string(),
                "sort".to_string(),
                "uniq".to_string(),
                "diff".to_string(),
                "which".to_string(),
                "find".to_string(),
                "realpath".to_string(),
                "basename".to_string(),
                "dirname".to_string(),
                "sha256sum".to_string(),
                "glob".to_string(),
                "grep".to_string(),
            ],
            permissions: vec![
                Rule {
                    pattern: "write_file(*)".to_string(),
                    action: Action::Deny,
                },
                Rule {
                    pattern: "edit_file(*)".to_string(),
                    action: Action::Deny,
                },
                Rule {
                    pattern: "edit(*)".to_string(),
                    action: Action::Deny,
                },
                Rule {
                    pattern: "mcp__*".to_string(),
                    action: Action::Ask,
                },
            ],
            model: None,
            builtin: true,
            source: None,
        }
    }

    pub fn plan() -> Self {
        Self {
            name: "plan".to_string(),
            prompt: PLAN_PROMPT.to_string(),
            description: Some("Produce an implementation plan".to_string()),
            tools: vec![
                "read_file".to_string(),
                "head".to_string(),
                "tail".to_string(),
                "wc".to_string(),
                "list_dir".to_string(),
                "tree".to_string(),
                "pwd".to_string(),
                "stat".to_string(),
                "du".to_string(),
                "sort".to_string(),
                "uniq".to_string(),
                "diff".to_string(),
                "which".to_string(),
                "find".to_string(),
                "realpath".to_string(),
                "basename".to_string(),
                "dirname".to_string(),
                "sha256sum".to_string(),
                "glob".to_string(),
                "grep".to_string(),
            ],
            permissions: vec![
                Rule {
                    pattern: "write_file(*)".to_string(),
                    action: Action::Deny,
                },
                Rule {
                    pattern: "edit_file(*)".to_string(),
                    action: Action::Deny,
                },
                Rule {
                    pattern: "mcp__*".to_string(),
                    action: Action::Ask,
                },
            ],
            model: None,
            builtin: true,
            source: None,
        }
    }

    pub fn debug() -> Self {
        Self {
            name: "debug".to_string(),
            prompt: DEBUG_PROMPT.to_string(),
            description: Some("Diagnose and fix problems".to_string()),
            tools: Vec::new(),
            permissions: Vec::new(),
            model: None,
            builtin: true,
            source: None,
        }
    }

    pub fn orchestrator() -> Self {
        Self {
            name: "orchestrator".to_string(),
            prompt: ORCHESTRATOR_PROMPT.to_string(),
            description: Some("Plan and coordinate multi-step work".to_string()),
            tools: Vec::new(),
            permissions: Vec::new(),
            model: None,
            builtin: true,
            source: None,
        }
    }

    /// The five built-in presets (SPEC §3.5 + orchestrator).
    pub fn builtins() -> Vec<Agent> {
        vec![
            Self::code(),
            Self::ask(),
            Self::plan(),
            Self::debug(),
            Self::orchestrator(),
        ]
    }

    /// Parse an agent preset from a markdown file with frontmatter.
    ///
    /// Frontmatter fields: `name`, `prompt`, `description`, `tools`,
    /// `permissions`, `model`. The body is the prompt when no `prompt` key is
    /// present.
    pub fn from_file(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let (fm, body) = bebok_skills::frontmatter::parse(&text);
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let name = fm
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or(stem);
        if name.is_empty() {
            return None;
        }
        let prompt = fm
            .get("prompt")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| body.trim().to_string());
        let description = fm
            .get("description")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let model = fm
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let tools = fm
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let permissions = fm
            .get("permissions")
            .map(crate::permission::parse_rules)
            .unwrap_or_default();

        Some(Agent {
            name,
            prompt,
            description,
            tools,
            permissions,
            model,
            builtin: false,
            source: Some(path.to_path_buf()),
        })
    }
}
