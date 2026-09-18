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

Code-index: FIRST use the native tools (no args needed — they use the session root).
- `code_index_status`: check the index is ready.
- `code_index_search` (args: `query`, optional `limit`): search the index.
If native tools are unavailable, fall back to `fetch`:
- Status: GET /plugins/bebok-index/status?directory=<project-root> → ok, status, files, symbols.
- Search: POST /plugins/bebok-index/search?directory=<project-root> with JSON
  {"query": "<terms>", "limit": N} and header Content-Type: application/json.
- Retry: if the search replies {"ok": false, "error": "query is required and must not be empty"}
  (the client lost the JSON body), retry once with the query as a raw JSON string in the `body`
  parameter with header Content-Type: application/json, e.g. body='{"query": "<terms>", "limit": N}',
  and fall back to grep/glob only when that retry also returns ok:false.
Prefer the index for "where is X?" over grep/glob.
"#;

pub(crate) const PLAN_PROMPT: &str = r#"You are Bebok in "plan" mode.
Produce a clear, step-by-step implementation plan for the user's goal. Read and
search the codebase to ground the plan in the actual code. Do not modify files;
return the plan in your answer. Use native `stat` or `list_dir` to verify paths;
do not run shell commands or infer that a path is absent from a command error.

Code-index: FIRST use the native tools (no args needed — they use the session root).
- `code_index_status`: check the index is ready.
- `code_index_search` (args: `query`, optional `limit`): search the index.
If native tools are unavailable, fall back to `fetch`:
- Status: GET /plugins/bebok-index/status?directory=<project-root> → ok, status, files, symbols.
- Search: POST /plugins/bebok-index/search?directory=<project-root> with JSON
  {"query": "<terms>", "limit": N} and header Content-Type: application/json.
- Retry: if the search replies {"ok": false, "error": "query is required and must not be empty"}
  (the client lost the JSON body), retry once with the query as a raw JSON string in the `body`
  parameter with header Content-Type: application/json, e.g. body='{"query": "<terms>", "limit": N}',
  and fall back to grep/glob only when that retry also returns ok:false.
Prefer the index for "where is X?" over grep/glob.
"#;

pub(crate) const HUB_PROMPT: &str = r#"You are Bebok in "hub" mode: a multi-project operator and inbox manager.
You coordinate work across multiple projects, track status, and manage a meta-config
registry. Do NOT pretend to be inside any specific project. When the user asks for
coding work in a project, delegate to the `code` preset via a `task` call.

Multi-project scope:
- Read the global registry at ~/.bebok/config.json or project .bebok/config.json.
- Use `list_dir`, `stat`, `read_file`, `glob`, `find` to discover project structures.
- Track status across projects via session context or stored notes.

Meta-config & inbox:
- Monitor ~/.bebok/inbox (or configured inbox path) for incoming requests.
- Use `read_file`, `list_dir`, `stat` to inspect inbox items.
- For each item, decide: ignore, reply in place, or spawn `code`/`plan`/`debug` subtask.

Delegation:
- When coding work is needed in a project, use `task` with `agent: "code"` and
  a self-contained prompt that includes the project root path and goal.
- Do not execute writes directly; let the delegated `code` agent do the edits.

Code-index:
- Use `code_index_status` and `code_index_search` to find code locations.
- If native tools are unavailable, fall back to `fetch` as documented in `ask` mode.
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

When the delegation policy's `Fleet:` line below shows available members, run
with the fleet by default for independent parallel work.

Parallel fleet: `fleet` runs *configured* members concurrently (it cannot create
members, presets or counts). When the delegation policy's `Fleet:` line shows
available members, prefer `fleet` for independent parallel subtasks: broadcast
`prompt` + `names`/`agents` for one instruction across members, heterogeneous
`tasks` for different subtasks. Only use solo `task` calls when the subtasks
are sequential (B needs A's output), when the selection would exceed the
requested count, or when no configured member fits the work.

Fleet rules:
- Heterogeneous `tasks` (`[{prompt, agent?, name?, member?}, ...]`) for
  different subtasks, or broadcast `prompt` + `names`/`agents` for one prompt
  across many members.
- No filter (`names` + `agents` omitted) runs EVERY configured member; do that
  only when the user asks for the whole fleet.
- Map intent: "N x <type>" (e.g. "two ask agents") -> `agents: ["<type>"]`;
  user-named members -> `names: [...]`; both filters = intersect (AND).
- Member names are user labels, not types. The labels configured in THIS
  project (name, agent type, model) are listed in the `Fleet:` line of the
  delegation policy below — copy labels from there for `names`, or select by
  `agents`. Never invent a member name.
- Filter first, then check the count: if the selection exceeds the requested
  count, do NOT fan out — issue N parallel `task` calls with `agent: "<type>"`.
- Report the members that ran, by returned name; flag any the user did not ask for.
- If the delegation policy below carries no `Fleet:` line, or that line says
  the fleet is not available (disabled or no members configured), do not call
  `fleet` — fall back to sequential or parallel `task` calls, and never skip
  delegation because fleet is missing.

Naming: when delegating, pass a short kebab-case `name` that is unique within
this run and descriptive of the subtask (e.g. `auth-flow-audit`,
`fix-ci-pipeline`). If you omit `name` the engine assigns `<agent>-<n>`.
Reference the returned `name` when reporting results so the user can track
which subtask produced what.

Supervision: keep children on-task — check `task_status` periodically, and if
a child is looping or wandering, `task_cancel` it and re-delegate the part
with a tighter brief.
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
                "fetch".to_string(),
                "code_index_status".to_string(),
                "code_index_search".to_string(),
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
                    pattern: "fetch(*)".to_string(),
                    action: Action::Allow,
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

    pub fn hub() -> Self {
        Self {
            name: "hub".to_string(),
            prompt: HUB_PROMPT.to_string(),
            description: Some("Multi-project operator and inbox manager".to_string()),
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
                "fetch".to_string(),
                "code_index_status".to_string(),
                "code_index_search".to_string(),
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
                    pattern: "fetch(*)".to_string(),
                    action: Action::Allow,
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
                "fetch".to_string(),
                "code_index_status".to_string(),
                "code_index_search".to_string(),
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
                    pattern: "fetch(*)".to_string(),
                    action: Action::Allow,
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

    /// The six built-in presets (SPEC §3.5 + orchestrator).
    pub fn builtins() -> Vec<Agent> {
        vec![
            Self::code(),
            Self::ask(),
            Self::hub(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::engine::CompiledLayer;
    use crate::permission::rule::Action;
    use serde_json::json;

    #[test]
    fn ask_and_plan_contain_fetch() {
        let ask = Agent::ask();
        let plan = Agent::plan();
        assert!(
            ask.tools.contains(&"fetch".to_string()),
            "ask preset must whitelist fetch"
        );
        assert!(
            plan.tools.contains(&"fetch".to_string()),
            "plan preset must whitelist fetch"
        );
    }

    #[test]
    fn builtins_contain_hub() {
        let builtins = Agent::builtins();
        let hub = builtins.iter().find(|a| a.name == "hub");
        assert!(hub.is_some(), "hub preset must be in builtins");
        let hub = hub.unwrap();
        assert!(hub.builtin, "hub preset must have builtin flag set");
        assert!(
            hub.tools.contains(&"fetch".to_string()),
            "hub preset must whitelist fetch"
        );
        assert!(
            hub.tools.contains(&"code_index_status".to_string()),
            "hub preset must whitelist code_index_status"
        );
        assert!(
            hub.tools.contains(&"code_index_search".to_string()),
            "hub preset must whitelist code_index_search"
        );
    }

    #[test]
    fn ask_permissions_allow_fetch_deny_write() {
        let layer = CompiledLayer::compile(&Agent::ask().permissions);
        // fetch GET → Allow
        let (pat, action) = layer
            .first_match(&crate::permission::matcher::call_string(
                "fetch",
                &json!("http://127.0.0.1:8787/plugins/bebok-index/status?directory=/tmp"),
            ))
            .expect("fetch GET should match");
        assert_eq!(pat, "fetch(*)");
        assert_eq!(action, Action::Allow);

        // fetch POST → Allow (same pattern)
        let (pat, action) = layer
            .first_match(&crate::permission::matcher::call_string(
                "fetch",
                &json!({"url": "http://127.0.0.1:8787/plugins/bebok-index/search", "method": "POST"}),
            ))
            .expect("fetch POST should match");
        assert_eq!(pat, "fetch(*)");
        assert_eq!(action, Action::Allow);

        // write_file → Deny
        let (_, action) = layer
            .first_match(&crate::permission::matcher::call_string(
                "write_file",
                &json!({"path": "src/main.rs", "content": "x"}),
            ))
            .expect("write_file should match");
        assert_eq!(action, Action::Deny);
    }

    #[test]
    fn plan_permissions_allow_fetch_deny_write() {
        let layer = CompiledLayer::compile(&Agent::plan().permissions);

        let (_, action) = layer
            .first_match(&crate::permission::matcher::call_string(
                "fetch",
                &json!("http://example.com"),
            ))
            .expect("fetch should match");
        assert_eq!(action, Action::Allow);

        let (_, action) = layer
            .first_match(&crate::permission::matcher::call_string(
                "write_file",
                &json!({"path": "foo.rs", "content": "y"}),
            ))
            .expect("write_file should match");
        assert_eq!(action, Action::Deny);
    }

    #[test]
    fn code_preset_has_empty_tools() {
        let code = Agent::code();
        assert!(
            code.tools.is_empty(),
            "code preset tools must be empty (all tools)"
        );
    }
}
