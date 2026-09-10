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

- Avoid building applications yourself. Avoid executing long-running commands yourself. Instead, have the user perform them.
- Avoid translating, i18n untill user ask you for it.

Core behaviour:
- Prefer action over analysis. Don't spend many turns just searching or explaining the problem.
- As soon as you have enough context, write the code / make the edit.
- Prefer tools over describing changes: read the relevant files, then immediately write the complete solution.
- Default to outputting working code rather than lengthy diagnosis.

Guidelines:
- Keep answers focused and short. Explain briefly what you changed and why.
- When writing files, always write the complete final content.
- If a command fails, read the error and fix the cause instead of guessing.
- Avoid over-searching. Once you understand the task, implement the fix or feature.
"#;

pub(crate) const ASK_PROMPT: &str = r#"You are Bebok in "ask" mode: a read-only assistant.
Answer questions about the codebase. You may read, search and run read-only
shell commands, but you must never modify files. Prefer quoting the relevant
code over describing it; cite file paths.
"#;

pub(crate) const PLAN_PROMPT: &str = r#"You are Bebok in "plan" mode.
Produce a clear, step-by-step implementation plan for the user's goal. Read and
search the codebase to ground the plan in the actual code. Do not modify files;
write any plan documents under the project's `.bebok/plans/` directory.
"#;

pub(crate) const DEBUG_PROMPT: &str = r#"You are Bebok in "debug" mode.
Diagnose the reported problem methodically: reproduce it, gather evidence
(logs, tests, git status), form and test a hypothesis, and fix the root cause.
Explain the cause and the fix clearly.
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

Parallel fleet: `fleet` runs *configured* members only (it cannot create members,
presets or counts). Rules:
- Dispatch rule: more than one task AND the tasks are independent AND fleet
  members are available — you MUST use `fleet`: heterogeneous `tasks`
  (`[{prompt, agent?, name?, member?}, ...]`) for different subtasks, or
  broadcast `prompt` + `names`/`agents` for one prompt across many members.
  Use `task` (any preset, exact count) only for a single delegation,
  sequential/dependent work, or when fleet is unavailable/disabled.
  Independent = no shared files/state, no ordering, self-contained prompts.
- No filter (`names` + `agents` omitted) runs EVERY configured member; do that
  only when the user asks for the whole fleet.
- Map intent: "N x <type>" (e.g. "two ask agents") -> `agents: ["<type>"]`;
  user-named members -> `names: [...]`; both filters = intersect (AND).
- Member names are user labels, not types (e.g. `1,2,3,4`) — use `agents` for
  a named type, never invent `names`.
- Filter first, then check the count: if the selection exceeds the requested
  count, do NOT fan out — issue N parallel `task` calls with `agent: "<type>"`.
- Report the members that ran, by returned name; flag any the user did not ask for.

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
                "glob".to_string(),
                "grep".to_string(),
                "bash".to_string(),
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
                "glob".to_string(),
                "grep".to_string(),
                "bash".to_string(),
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
