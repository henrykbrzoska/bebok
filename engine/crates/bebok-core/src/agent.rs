//! Agent presets and the agent loop (SPEC §3.3).
//!
//! Agents are presets, not code: system prompt + tool whitelist + permission
//! overrides. Tool execution passes through the permission engine (M2): a call
//! resolves to `allow`, `deny` or `ask`; `ask` suspends the loop on a oneshot
//! until a client resolves it through the decision endpoint.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bebok_llm::{ChatMessage, ChatRequest, ChatRole, Provider, StreamEvent, Thinking, ToolDef, ToolResult};
use bebok_tools::{ToolCtx, ToolRegistry};
use futures::StreamExt;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::error::{CoreError, Result};
use crate::event::{Event, EventBus};
use crate::permission::{
    Action, CachedDecision, CompiledLayer, DecisionKey, Evaluation, PermissionEngine, Rule,
    Verdict,
};
use crate::plugin::{
    Hook, PermissionHook, PluginHost, RequestHook, RequestMessage, ToolCallHook, ToolResultHook,
    TurnHook,
};
use crate::session::{Message, Role, Session, ToolState};
use crate::store::SessionState;
use crate::util::{now_ms, truncate_output};

const CODE_PROMPT: &str = r#"You are Bebok, a local-first coding agent.

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

const ASK_PROMPT: &str = r#"You are Bebok in "ask" mode: a read-only assistant.
Answer questions about the codebase. You may read, search and run read-only
shell commands, but you must never modify files. Prefer quoting the relevant
code over describing it; cite file paths.
"#;

const PLAN_PROMPT: &str = r#"You are Bebok in "plan" mode.
Produce a clear, step-by-step implementation plan for the user's goal. Read and
search the codebase to ground the plan in the actual code. Do not modify files;
write any plan documents under the project's `.bebok/plans/` directory.
"#;

const DEBUG_PROMPT: &str = r#"You are Bebok in "debug" mode.
Diagnose the reported problem methodically: reproduce it, gather evidence
(logs, tests, git status), form and test a hypothesis, and fix the root cause.
Explain the cause and the fix clearly.
"#;

const ORCHESTRATOR_PROMPT: &str = r#"You are Bebok in "orchestrator" mode.
Break the user's goal into subtasks, delegate work in a sensible order, and
coordinate the results into a coherent outcome. Plan first, identify which
subtasks are independent (parallel) vs sequential, then drive each one to
completion using the available tools. Summarize what was accomplished at the end.
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
                Rule { pattern: "write_file(*)".to_string(), action: Action::Deny },
                Rule { pattern: "edit(*)".to_string(), action: Action::Deny },
                Rule { pattern: "mcp__*".to_string(), action: Action::Ask },
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
                Rule { pattern: "write_file(*)".to_string(), action: Action::Deny },
                Rule { pattern: "mcp__*".to_string(), action: Action::Ask },
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
}

/// Summary of an agent preset exposed to the GUI (`GET /agent`).
#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub name: String,
    pub description: Option<String>,
    pub model: Option<String>,
    pub builtin: bool,
    pub source: Option<String>,
}

/// A resolved catalog of agents for one instance: built-ins + file presets
/// from `~/.config/bebok/agent/*.md` and `<project>/.bebok/agent/*.md`
/// (project overrides global, both override built-ins by name).
#[derive(Debug, Clone, Default)]
pub struct AgentCatalog {
    builtins: Vec<Agent>,
    custom: BTreeMap<String, Agent>,
}

impl AgentCatalog {
    /// Load built-ins + file presets for a project directory.
    pub fn load(project: &Path) -> Self {
        let mut catalog = Self {
            builtins: Agent::builtins(),
            custom: BTreeMap::new(),
        };
        if let Some(dir) = dirs::config_dir().map(|d| d.join("bebok").join("agent")) {
            catalog.load_dir(&dir);
        }
        catalog.load_dir(&project.join(".bebok").join("agent"));
        catalog
    }

    fn load_dir(&mut self, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e != "md").unwrap_or(true) {
                continue;
            }
            if let Some(agent) = Agent::from_file(&path) {
                self.custom.insert(agent.name.clone(), agent);
            }
        }
    }

    /// Resolve an agent by name. Unknown names fall back to `code`.
    pub fn resolve(&self, name: &str) -> Agent {
        if let Some(a) = self.custom.get(name) {
            return a.clone();
        }
        if let Some(a) = self.builtins.iter().find(|a| a.name == name) {
            return a.clone();
        }
        tracing::warn!("unknown agent '{name}', falling back to 'code'");
        Agent::code()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.custom.contains_key(name) || self.builtins.iter().any(|a| a.name == name)
    }

    /// All agents (custom first, then built-ins), as GUI summaries.
    pub fn list(&self) -> Vec<AgentInfo> {
        let mut out: Vec<AgentInfo> = self
            .custom
            .values()
            .map(|a| AgentInfo {
                name: a.name.clone(),
                description: a.description.clone(),
                model: a.model.clone(),
                builtin: false,
                source: a.source.as_ref().map(|p| p.display().to_string()),
            })
            .collect();
        for a in &self.builtins {
            // A custom file can shadow a built-in name.
            if self.custom.contains_key(&a.name) {
                continue;
            }
            out.push(AgentInfo {
                name: a.name.clone(),
                description: a.description.clone(),
                model: a.model.clone(),
                builtin: true,
                source: None,
            });
        }
        out
    }
}

impl Agent {
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

/// Spawn a filesystem watcher that reloads the agent catalog and emits
/// `agent.list.changed` whenever an agent file changes (debounced).
///
/// Watches `~/.config/bebok/agent` and `<project>/.bebok/agent`. The returned
/// task lives for the process lifetime (matching the engine's "hot reload"
/// semantics); it keeps the `notify` watcher alive internally.
pub fn spawn_agent_watcher(
    project: PathBuf,
    catalog: Arc<std::sync::RwLock<AgentCatalog>>,
    bus: EventBus,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    use notify::{RecursiveMode, Watcher};

    let global_dir = dirs::config_dir().map(|d| d.join("bebok").join("agent"));
    let project_dir = project.join(".bebok").join("agent");
    // The project agent dir is created eagerly so it can be watched.
    if !project_dir.exists() {
        std::fs::create_dir_all(&project_dir)?;
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res
            && (ev.kind.is_create() || ev.kind.is_modify() || ev.kind.is_remove())
        {
            let _ = tx.send(());
        }
    })
    .map_err(|e| std::io::Error::other(e.to_string()))?;

    for dir in [global_dir.as_deref(), Some(project_dir.as_path())].into_iter().flatten() {
        if dir.exists() {
            let _ = watcher.watch(dir, RecursiveMode::NonRecursive);
        }
    }

    Ok(tokio::spawn(async move {
        let _watcher = watcher;
        loop {
            if rx.recv().await.is_none() {
                break;
            }
            // Debounce a burst of editor saves into a single reload.
            loop {
                tokio::select! {
                    _ = rx.recv() => continue,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => break,
                }
            }
            {
                let mut guard = catalog.write().unwrap();
                *guard = AgentCatalog::load(&project);
            }
            bus.publish(Event::new(
                "agent.list.changed",
                &project.to_string_lossy(),
                "",
            ));
        }
    }))
}

/// Build the provider request from the session transcript.
pub async fn build_request(
    state: &SessionState,
    agent: &Agent,
    tools: &ToolRegistry,
    model: &str,
    max_tokens: u32,
    thinking: Thinking,
) -> Result<ChatRequest> {
    let messages = state.messages_snapshot().await;
    let mut chat: Vec<ChatMessage> = Vec::new();

    for msg in messages.iter() {
        match msg.role {
            Role::User => {
                let text = msg.text_content();
                // A user message may carry tool results produced right before
                // it in the same turn; normally results are attached below.
                chat.push(ChatMessage {
                    role: ChatRole::User,
                    content: text,
                    tool_calls: Vec::new(),
                    tool_results: Vec::new(),
                });
            }
            Role::Assistant => {
                let content = msg.text_content();
                let mut tool_calls = Vec::new();
                let mut tool_results = Vec::new();
                for part in &msg.parts {
                    if let crate::session::Part::Tool { id, name, state } = part {
                        tool_calls.push(bebok_llm::ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            input: state.input().clone(),
                        });
                        match state {
                            ToolState::Completed { output, .. } => {
                                tool_results.push(ToolResult {
                                    tool_use_id: id.clone(),
                                    content: output.clone(),
                                    is_error: false,
                                });
                            }
                            ToolState::Error { error, .. } => {
                                tool_results.push(ToolResult {
                                    tool_use_id: id.clone(),
                                    content: error.clone(),
                                    is_error: true,
                                });
                            }
                            _ => {}
                        }
                    }
                }
                chat.push(ChatMessage {
                    role: ChatRole::Assistant,
                    content,
                    tool_calls,
                    tool_results: Vec::new(),
                });
                if !tool_results.is_empty() {
                    chat.push(ChatMessage {
                        role: ChatRole::User,
                        content: String::new(),
                        tool_calls: Vec::new(),
                        tool_results,
                    });
                }
            }
        }
    }

    if chat.is_empty() {
        return Err(CoreError::Other("empty conversation".to_string()));
    }

    let tool_defs: Vec<ToolDef> = tools
        .list()
        .iter()
        .filter(|t| agent.tools.is_empty() || agent.tools.iter().any(|n| n == t.name()))
        .map(|t| ToolDef {
            name: t.name().to_string(),
            description: t.description().to_string(),
            input_schema: t.parameters_schema(),
        })
        .collect();

    // Context management (§3.9): if the transcript exceeds the token budget,
    // prune old tool outputs down to a `[truncated]` marker (non-destructive:
    // the full history stays on disk).
    let budget = state.config_snapshot().context_budget;
    let chat = prune_for_budget(chat, &agent.prompt, budget);

    Ok(ChatRequest {
        model: model.to_string(),
        system: agent.prompt.clone(),
        messages: chat,
        tools: tool_defs,
        max_tokens,
        thinking,
    })
}

/// Replace old tool results with `[truncated]` (oldest first) until the built
/// request fits the token budget. Never mutates the persisted transcript.
fn prune_for_budget(
    mut chat: Vec<ChatMessage>,
    system: &str,
    budget: usize,
) -> Vec<ChatMessage> {
    let truncated_tokens = crate::context::estimate_tokens("[truncated]");
    let mut tokens = crate::context::estimate_chat(&chat, system);
    if tokens <= budget {
        return chat;
    }
    'outer: for i in 0..chat.len() {
        for j in 0..chat[i].tool_results.len() {
            if chat[i].tool_results[j].content == "[truncated]" {
                continue;
            }
            let before = crate::context::estimate_tokens(&chat[i].tool_results[j].content);
            chat[i].tool_results[j].content = "[truncated]".to_string();
            tokens = tokens.saturating_sub(before) + truncated_tokens;
            if tokens <= budget {
                break 'outer;
            }
        }
    }
    chat
}

/// Convert a provider message into the (lightweight) plugin request view.
fn hook_request_message(m: &ChatMessage) -> RequestMessage {
    RequestMessage {
        role: m.role.as_str().to_string(),
        text: m.content.clone(),
        tool_calls: m.tool_calls.len(),
        tool_results: m.tool_results.len(),
    }
}

/// Fire the `permission.resolved` hook (no-op when no plugin is registered).
async fn fire_permission_hook(tool: &str, pattern: &str, decision: &str) {
    let hooks = PluginHost::global();
    if hooks.has_plugins().await {
        let mut payload = PermissionHook {
            tool: tool.to_string(),
            decision: decision.to_string(),
            pattern: pattern.to_string(),
        };
        hooks.run_hook(Hook::PERMISSION_RESOLVED, &mut payload).await;
    }
}

/// Run a full turn: build request -> stream SSE -> append parts -> pending
/// tool calls -> permission gate (allow/ask/deny, M2) -> execute -> events.
///
/// The turn lock must already be held by the caller (409 otherwise).
pub async fn run_turn(
    state: Arc<SessionState>,
    agent: Agent,
    tools: Arc<ToolRegistry>,
    provider: Arc<dyn Provider>,
    permission: Arc<PermissionEngine>,
    bus: EventBus,
    abort: CancellationToken,
    model: &str,
) -> Result<()> {
    let config = state.config_snapshot();
    let hooks = PluginHost::global();

    // Agent permission overrides are evaluated before project/global rules.
    let agent_layer = if agent.permissions.is_empty() {
        None
    } else {
        Some(CompiledLayer::compile(&agent.permissions))
    };

    loop {
        if abort.is_cancelled() {
            break;
        }

        let mut req = build_request(&state, &agent, &tools, model, config.max_tokens, config.thinking).await?;

        // Plugin hook: inspect / mutate the request before it is sent. Only the
        // system prompt is applied back (messages stay canonical on disk).
        if hooks.has_plugins().await {
            let mut payload = RequestHook::new(
                model,
                req.system.clone(),
                req.messages.iter().map(hook_request_message).collect(),
            );
            hooks.run_hook(Hook::BEFORE_REQUEST, &mut payload).await;
            req.system = payload.system;
        }

        bus.publish(
            Event::new("debug.log", state.directory(), &state.id().to_string()).with_properties(
                serde_json::json!({
                    "source": "llm",
                    "kind": "request",
                    "title": format!("llm {model}"),
                    "detail": format!(
                        "messages={} tools={} system_chars={} max_tokens={} thinking={}",
                        req.messages.len(),
                        req.tools.len(),
                        req.system.chars().count(),
                        req.max_tokens,
                        req.thinking.as_str()
                    ),
                }),
            ),
        );
        let mut stream = match provider.stream(req).await {
            Ok(s) => s,
            Err(err) => {
                // A provider-level failure (e.g. a model that refuses tool
                // use, invalid request, 5xx): note it in the project config so
                // repeat offenders can be handled/filtered later.
                crate::config::record_llm_error(Path::new(state.directory()), model, &err.to_string());
                bus.publish(
                    Event::new("debug.log", state.directory(), &state.id().to_string())
                        .with_properties(serde_json::json!({
                            "source": "llm",
                            "kind": "error",
                            "title": format!("llm {model}"),
                            "detail": err.to_string(),
                        })),
                );
                return Err(err.into());
            }
        };

        // Stream into a new assistant message (kept in the session so GET
        // /message reflects live state; persisted on part completion).
        let (assistant_idx, _assistant_id) = {
            let mut messages = state.messages.write().await;
            let msg = Message::assistant_with(&agent.name, model);
            let id = msg.id;
            messages.push(msg);
            (messages.len() - 1, id)
        };
        state.note_message_index(assistant_idx);

        let mut saw_any = false;
        while let Some(ev) = stream.next().await {
            if abort.is_cancelled() {
                break;
            }
            match ev? {
                StreamEvent::Text(delta) => {
                    saw_any = true;
                    state.append_to_part(assistant_idx, |m| m.append_text(&delta)).await;
                    emit_part(&bus, &state, "message.part.updated", assistant_idx).await;
                }
                StreamEvent::Thinking(delta) => {
                    saw_any = true;
                    state.append_to_part(assistant_idx, |m| m.append_thinking(&delta)).await;
                    emit_part(&bus, &state, "message.part.updated", assistant_idx).await;
                }
                StreamEvent::ToolCall(call) => {
                    saw_any = true;
                    state
                        .append_to_part(assistant_idx, |m| {
                            m.add_tool_call(call.id.clone(), call.name.clone(), call.input.clone())
                        })
                        .await;
                    emit_part(&bus, &state, "message.part.updated", assistant_idx).await;
                }
                StreamEvent::Done(usage) => {
                    let cost = bebok_llm::compute_cost(
                        model,
                        usage.input_tokens,
                        usage.output_tokens,
                        usage.cache_read_input_tokens.unwrap_or(0),
                        usage.cache_creation_input_tokens.unwrap_or(0),
                    )
                    .or(usage.cost);
                    let cache_read = usage.cache_read_input_tokens;
                    let cache_write = usage.cache_creation_input_tokens;
                    state
                        .append_to_part(assistant_idx, |m| {
                            m.set_usage(crate::session::UsageTotals {
                                input_tokens: usage.input_tokens,
                                output_tokens: usage.output_tokens,
                                cost,
                                cache_read_input_tokens: cache_read,
                                cache_creation_input_tokens: cache_write,
                            })
                        })
                        .await;
                    state
                        .add_usage(usage.input_tokens, usage.output_tokens, cost, cache_read, cache_write)
                        .await;
                    emit_part(&bus, &state, "message.part.updated", assistant_idx).await;
                    bus.publish(
                        Event::new("debug.log", state.directory(), &state.id().to_string())
                            .with_properties(serde_json::json!({
                                "source": "llm",
                                "kind": "response",
                                "title": format!("llm {model}"),
                                "detail": format!(
                                    "in={} out={} cache_read={} cache_write={}",
                                    usage.input_tokens,
                                    usage.output_tokens,
                                    cache_read.unwrap_or(0),
                                    cache_write.unwrap_or(0)
                                ),
                            })),
                    );
                }
            }
            emit_message(&bus, &state, "message.updated", assistant_idx);
        }

        // Drop the stream so the HTTP connection closes promptly.
        drop(stream);

        if abort.is_cancelled() {
            // Persist whatever streamed so far (crash-safe transcript).
            state.persist_message_at(assistant_idx).await;
            break;
        }

        if !saw_any {
            state.persist_message_at(assistant_idx).await;
            break;
        }

        // Persist the assistant message (parts completed -> flush to disk).
        state.persist_message_at(assistant_idx).await;
        emit_message(&bus, &state, "message.updated", assistant_idx);

        // Pending tool calls.
        let pending = {
            let messages = state.messages.read().await;
            messages
                .get(assistant_idx)
                .map(|m| m.pending_tool_calls())
                .unwrap_or_default()
        };

        if pending.is_empty() {
            break; // final answer
        }

        for (call_id, tool_name, input) in pending {
            if abort.is_cancelled() {
                break;
            }

            // Permission gate (M2): allow / deny / ask. `ask` may suspend the
            // loop until a client resolves the request (oneshot, no polling).
            let gate = GateCtx {
                state: &state,
                bus: &bus,
                permission: &permission,
                agent_layer: agent_layer.as_ref(),
                tools: &tools,
                abort: &abort,
                agent_name: &agent.name,
                assistant_idx,
            };
            match resolve_permission(&gate, &tool_name, &input).await {
                ToolOutcome::Denied(message) => {
                    fail_tool(&state, &bus, assistant_idx, &call_id, message).await;
                    continue;
                }
                ToolOutcome::Aborted => {
                    fail_tool(&state, &bus, assistant_idx, &call_id, "aborted").await;
                    break;
                }
                ToolOutcome::Run => {}
            }

            // Plugin hook: a registered plugin may veto an allowed call.
            if hooks.has_plugins().await {
                let mut payload = ToolCallHook {
                    tool: tool_name.clone(),
                    input: input.clone(),
                    allowed: true,
                };
                hooks.run_hook(Hook::BEFORE_TOOL, &mut payload).await;
                if !payload.allowed {
                    fail_tool(&state, &bus, assistant_idx, &call_id, "denied by plugin").await;
                    continue;
                }
            }

            // Allowed: mark running, persist, execute, complete.
            let Some(tool) = tools.get(&tool_name) else {
                // The model called a tool that is not registered: note it in the
                // project config (de-duplicated) so it can be implemented later.
                crate::config::record_unknown_tool(Path::new(state.directory()), &tool_name);
                fail_tool(
                    &state,
                    &bus,
                    assistant_idx,
                    &call_id,
                    &format!("tool not found: {tool_name}"),
                )
                .await;
                continue;
            };

            state
                .update_tool_state(assistant_idx, &call_id, |m, _| {
                    m.mark_tool_running(&call_id, now_ms())
                })
                .await;
            state.persist_message_at(assistant_idx).await;
            emit_part(&bus, &state, "message.part.updated", assistant_idx).await;

            let ctx = ToolCtx {
                root: state.directory().into(),
                session_id: state.id().to_string(),
                abort: abort.clone(),
            };
            let output = tool.execute(ctx, input).await;
            let text = truncate_output(&output.text, config.tool_output_cap);
            let ok = state
                .update_tool_state(assistant_idx, &call_id, |m, name| {
                    m.mark_tool_completed(&call_id, text.clone(), name.to_string())
                })
                .await;
            if !ok {
                tracing::warn!("tool part {call_id} vanished during execution");
            }

            // Plugin hook: observe the completed tool call (output is the
            // truncated text that is persisted into the transcript).
            if hooks.has_plugins().await {
                let mut payload = ToolResultHook {
                    tool: tool_name.clone(),
                    ok,
                    output: text.clone(),
                };
                hooks.run_hook(Hook::AFTER_TOOL, &mut payload).await;
            }

            state.persist_message_at(assistant_idx).await;
            emit_part(&bus, &state, "message.part.updated", assistant_idx).await;
            emit_message(&bus, &state, "message.updated", assistant_idx);
        }
    }

    state.touch().await;

    // Plugin hook: turn finished successfully (errors return early above and
    // surface through `session.updated` events to the observers).
    if hooks.has_plugins().await {
        let mut payload = TurnHook {
            ok: true,
            messages: state.messages_snapshot().await.len(),
        };
        hooks.run_hook(Hook::TURN_END, &mut payload).await;
    }

    emit_session(&bus, &state, "session.updated");
    Ok(())
}

/// What the permission gate decided for one tool call.
enum ToolOutcome {
    /// Execute the tool.
    Run,
    /// Do not execute; fail the tool part with this message.
    Denied(&'static str),
    /// The turn is being aborted; stop processing further calls.
    Aborted,
}

/// Everything the permission gate needs for one tool call.
struct GateCtx<'a> {
    state: &'a Arc<SessionState>,
    bus: &'a EventBus,
    permission: &'a PermissionEngine,
    agent_layer: Option<&'a CompiledLayer>,
    tools: &'a ToolRegistry,
    abort: &'a CancellationToken,
    agent_name: &'a str,
    assistant_idx: usize,
}

/// Evaluate one tool call against the permission engine, falling back to the
/// session decision cache and - for `Ask` - to the user decision flow.
async fn resolve_permission(ctx: &GateCtx<'_>, tool_name: &str, input: &serde_json::Value) -> ToolOutcome {
    let read_only = ctx
        .tools
        .get(tool_name)
        .map(|t| t.is_read_only())
        .unwrap_or(false);
    let evaluation = ctx
        .permission
        .evaluate(ctx.agent_layer, tool_name, input, read_only);
    match evaluation.verdict {
        Verdict::Allow => ToolOutcome::Run,
        Verdict::Deny => ToolOutcome::Denied("denied by policy"),
        Verdict::Ask => {
            // Session decision cache: an identical (tool, pattern) is not asked
            // again within this session.
            let key = DecisionKey(tool_name.to_string(), evaluation.pattern.clone());
            match ctx.state.cached_decision(&key).await {
                Some(CachedDecision::Allow) => {
                    fire_permission_hook(tool_name, &evaluation.pattern, "allow").await;
                    ToolOutcome::Run
                }
                Some(CachedDecision::Deny) => {
                    fire_permission_hook(tool_name, &evaluation.pattern, "deny").await;
                    ToolOutcome::Denied("denied by user")
                }
                None => ask_for_permission(ctx, tool_name, input, &evaluation).await,
            }
        }
    }
}

/// The `ask` flow: register a oneshot under a fresh request id, emit
/// `permission.asked`, and wait for the client decision (or the abort token).
async fn ask_for_permission(
    ctx: &GateCtx<'_>,
    tool_name: &str,
    input: &serde_json::Value,
    evaluation: &Evaluation,
) -> ToolOutcome {
    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();
    ctx.state.register_permission_request(&request_id, tx).await;

    ctx.bus.publish(
        Event::new("permission.asked", ctx.state.directory(), &ctx.state.id().to_string())
            .with_properties(serde_json::json!({
                "requestID": request_id,
                "messageIndex": ctx.assistant_idx,
                "toolName": tool_name,
                "agent": ctx.agent_name,
                "input": input.clone(),
                "pattern": evaluation.pattern.clone(),
            })),
    );

    // Wait without polling. The decision endpoint answers the oneshot; the
    // abort token lets the user cancel a pending consent request.
    let answer = tokio::select! {
        biased;
        _ = ctx.abort.cancelled() => None,
        answer = rx => answer.ok(),
    };
    ctx.state.unregister_permission_request(&request_id).await;

    let Some(answer) = answer else {
        return ToolOutcome::Aborted;
    };

    // Remember the decision for the rest of the session.
    let key = DecisionKey(tool_name.to_string(), evaluation.pattern.clone());
    let cached = if answer.allow {
        CachedDecision::Allow
    } else {
        CachedDecision::Deny
    };
    ctx.state.remember_decision(key, cached).await;

    // `always` persists `ask -> allow` for the matched pattern to the project
    // config and recompiles the engine's project layer in memory.
    if answer.always
        && answer.allow
        && let Err(e) = ctx.permission.always_allow(&evaluation.pattern)
    {
        tracing::error!(
            "failed to persist always-allow rule for {}: {e}",
            evaluation.pattern
        );
    }

    ctx.bus.publish(
        Event::new("permission.resolved", ctx.state.directory(), &ctx.state.id().to_string())
            .with_properties(serde_json::json!({
                "requestID": request_id,
                "messageIndex": ctx.assistant_idx,
                "toolName": tool_name,
                "agent": ctx.agent_name,
                "pattern": evaluation.pattern.clone(),
                "decision": if answer.allow { "allow" } else { "deny" },
                "always": answer.always,
                "allowed": answer.allow,
            })),
    );

    fire_permission_hook(
        tool_name,
        &evaluation.pattern,
        if answer.allow { "allow" } else { "deny" },
    )
    .await;

    if answer.allow {
        ToolOutcome::Run
    } else {
        ToolOutcome::Denied("denied by user")
    }
}

/// Fail a tool part (mark `ToolState::Error`), persist and emit events.
async fn fail_tool(
    state: &Arc<SessionState>,
    bus: &EventBus,
    assistant_idx: usize,
    call_id: &str,
    message: &str,
) {
    state
        .update_tool_state(assistant_idx, call_id, |m, _| {
            m.mark_tool_error(call_id, message.to_string())
        })
        .await;
    state.persist_message_at(assistant_idx).await;
    emit_part(bus, state, "message.part.updated", assistant_idx).await;
    emit_message(bus, state, "message.updated", assistant_idx);
}

fn emit_message(bus: &EventBus, state: &SessionState, kind: &str, idx: usize) {
    bus.publish(
        Event::new(kind, state.directory(), &state.id().to_string()).with_properties(
            serde_json::json!({
                "messageIndex": idx,
            }),
        ),
    );
}

async fn emit_part(bus: &EventBus, state: &SessionState, kind: &str, idx: usize) {
    let snapshot = {
        let messages = state.messages.read().await;
        messages
            .get(idx)
            .and_then(|m| serde_json::to_value(m).ok())
    };
    let properties = match snapshot {
        Some(message) => serde_json::json!({ "messageIndex": idx, "message": message }),
        None => serde_json::json!({ "messageIndex": idx }),
    };
    bus.publish(
        Event::new(kind, state.directory(), &state.id().to_string()).with_properties(properties),
    );
}

fn emit_session(bus: &EventBus, state: &SessionState, kind: &str) {
    bus.publish(Event::new(kind, state.directory(), &state.id().to_string()));
}

/// Summarize a session into a title (first user text) - M1 heuristic.
pub fn title_from(session: &Session, first_prompt: &str) -> Option<String> {
    if session.title.is_some() {
        return None;
    }
    let mut title: String = first_prompt.chars().take(60).collect();
    if title.is_empty() {
        return None;
    }
    if first_prompt.chars().count() > 60 {
        title.push('…');
    }
    Some(title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use crate::permission::{PermissionAnswer, ResolveOutcome};
    use crate::session::Part;
    use crate::store::InstanceStore;
    use bebok_llm::{ChatRequest, Provider, StreamEvent, ToolCall, Usage};
    use bebok_mcp::{McpManager, McpServerSpec, McpTransport};
    use bebok_tools::{Runtimes, ToolRegistry, builtin_tools};
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::broadcast;

    /// Minimal stdio MCP server (JSON-RPC over stdin/stdout) used by the
    /// permission-gate acceptance test.
    const MCP_SERVER_PY: &str = r#"import sys, json

def respond(req_id, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": req_id, "result": result}) + "\n")
    sys.stdout.flush()

def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except Exception:
            continue
        method = msg.get("method")
        req_id = msg.get("id")
        if method == "initialize":
            params = msg.get("params", {})
            respond(req_id, {"protocolVersion": params.get("protocolVersion", "2024-11-05"),
                             "capabilities": {"tools": {}},
                             "serverInfo": {"name": "test-server", "version": "1.0.0"}})
        elif method == "tools/list":
            respond(req_id, {"tools": [
                {"name": "echo", "description": "Echo text",
                 "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}},
                {"name": "mutate", "description": "Pretend to mutate",
                 "inputSchema": {"type": "object", "properties": {"target": {"type": "string"}}}},
            ]})
        elif method == "tools/call":
            params = msg.get("params", {})
            args = params.get("arguments", {}) or {}
            if params.get("name") == "mutate":
                respond(req_id, {"content": [{"type": "text", "text": "mutated " + str(args.get("target", ""))}]})
            else:
                respond(req_id, {"content": [{"type": "text", "text": "echo: " + str(args.get("text", ""))}]})

if __name__ == "__main__":
    main()
"#;

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// A provider that replays a fixed script of event groups; group n is
    /// streamed on the n-th `stream` request.
    struct ScriptProvider {
        calls: AtomicUsize,
        script: Vec<Vec<StreamEvent>>,
    }

    impl ScriptProvider {
        fn new(script: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                script,
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for ScriptProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn stream(
            &self,
            _req: ChatRequest,
        ) -> bebok_llm::StreamResult<
            futures::stream::BoxStream<'static, bebok_llm::StreamResult<StreamEvent>>,
        > {
            let n = self.calls.fetch_add(1, Ordering::Relaxed);
            let events: Vec<bebok_llm::StreamResult<StreamEvent>> = self
                .script
                .get(n)
                .cloned()
                .unwrap_or_else(|| text_step("(no more steps)"))
                .into_iter()
                .map(Ok)
                .collect();
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    fn tool_step(name: &str, input: serde_json::Value, id: u32) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ToolCall(ToolCall {
                id: format!("toolu_{id}"),
                name: name.to_string(),
                input,
            }),
            StreamEvent::Done(Usage {
                input_tokens: 1,
                output_tokens: 1,
                cost: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            }),
        ]
    }

    fn text_step(text: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::Text(text.to_string()),
            StreamEvent::Done(Usage {
                input_tokens: 1,
                output_tokens: 1,
                cost: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            }),
        ]
    }

    /// Write a `permission` section to `<project>/.bebok/config.json`.
    fn write_project_permission(project: &Path, permission_json: &str) {
        let dir = project.join(".bebok");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            format!("{{ \"permission\": {permission_json} }}\n"),
        )
        .unwrap();
    }

    /// Drain a broadcast receiver for `quiet` ms (used to collect events).
    async fn drain_events(rx: &mut broadcast::Receiver<Event>, quiet_ms: u64) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(ev) = tokio::time::timeout(Duration::from_millis(quiet_ms), rx.recv()).await {
            if let Ok(ev) = ev {
                out.push(ev);
            }
        }
        out
    }

    /// Resolve the first `permission.asked` seen on `rx`.
    async fn resolve_first_ask(
        session: Arc<crate::store::SessionState>,
        mut rx: broadcast::Receiver<Event>,
        answer: PermissionAnswer,
    ) {
        loop {
            let ev = rx
                .recv()
                .await
                .expect("stream closed before a permission.asked event");
            if ev.kind == "permission.asked" {
                let request_id = ev.properties["requestID"]
                    .as_str()
                    .expect("requestID must be a string")
                    .to_string();
                let outcome = session
                    .resolve_permission_request(&request_id, answer)
                    .await;
                assert_eq!(
                    outcome,
                    ResolveOutcome::Resolved,
                    "first ask must resolve"
                );
                return;
            }
        }
    }

    // ------------------------------------------------------------------
    // M1 baseline
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn runs_tool_turn_end_to_end() {
        // temp project + data dirs
        let base = std::env::temp_dir().join(format!("bebok-test-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        // write_file is mutating -> would Ask by default; allow it via config.
        write_project_permission(
            &project,
            r#"{ "rules": [ { "pattern": "write_file(*)", "action": "allow" } ] }"#,
        );
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();

        // Append the prompt (like the server does) before running the turn.
        session.append_user_message("create a file hello.txt with the content hello").await.unwrap();
        session.set_title_if_empty("create a file hello.txt").await;

        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step(
                "write_file",
                serde_json::json!({ "path": "hello.txt", "content": "hello" }),
                1,
            ),
            text_step("hello.txt created"),
        ]));
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();
        let abort = CancellationToken::new();

        // subscribe before the turn to capture events
        let mut events = bus.subscribe();

        session.try_begin_turn();
        run_turn(
            session.clone(),
            Agent::code(),
            tools,
            provider,
            permission,
            bus,
            abort,
            crate::config::DEFAULT_MODEL,
        )
        .await
        .unwrap();
        session.end_turn();

        // hello.txt must exist with content "hello"
        let content = tokio::fs::read_to_string(project.join("hello.txt")).await.unwrap();
        assert_eq!(content, "hello", "write_file tool must create hello.txt");

        // Transcript: [user, assistant(write_file tool), assistant(final text)].
        // The tool result is carried by the Tool part (Completed), not by a
        // separate persisted message.
        let messages = session.messages_snapshot().await;
        assert_eq!(messages.len(), 3, "expected 3 messages, got {}", messages.len());
        assert!(messages[0].parts.iter().any(|p| matches!(p, Part::Text { text } if text == "create a file hello.txt with the content hello")));

        // Tool part must be Completed.
        let tool_completed = messages[1].parts.iter().any(|p| {
            matches!(p, Part::Tool { name, state: ToolState::Completed { .. }, .. } if name == "write_file")
        });
        assert!(tool_completed, "tool part must be in Completed state");

        // Final assistant text present.
        let final_text = messages[2].text_content();
        assert!(final_text.contains("hello.txt created"), "got: {final_text}");

        // Events observed.
        let kinds: std::collections::HashSet<String> = drain_events(&mut events, 300)
            .await
            .into_iter()
            .map(|ev| ev.kind)
            .collect();
        assert!(kinds.contains("message.updated"));
        assert!(kinds.contains("message.part.updated"));
        assert!(kinds.contains("session.updated"));
        assert!(!kinds.contains("permission.asked"), "allow rule must avoid ask");

        // Disk: msg files + session.json + index.jsonl.
        let disk = session.disk_dir().to_path_buf();
        assert!(disk.join("msg-000000.json").exists());
        assert!(disk.join("msg-000002.json").exists());
        assert!(disk.join("session.json").exists());
        let idx = data.join("instances").read_dir().unwrap().next().unwrap().unwrap().path().join("sessions/index.jsonl");
        assert!(idx.exists(), "index.jsonl missing: {idx:?}");

        // Cleanup
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn restart_reloads_sessions_from_disk() {
        let base = std::env::temp_dir().join(format!("bebok-restart-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let data = base.join("data");

        let id = {
            let store = InstanceStore::with_data_dir(data.clone());
            let s = store
                .create_session(project.to_str().unwrap(), "code", None)
                .await
                .unwrap();
            s.id()
        };

        // Simulate restart: a fresh store over the same data dir.
        let store = InstanceStore::with_data_dir(data.clone());
        let list = store.list_sessions(project.to_str().unwrap()).await;
        assert_eq!(list.len(), 1, "session must survive restart");
        assert_eq!(list[0].id, id);

        // And the transcript opens.
        let s = store.open_session(id).await.unwrap();
        assert_eq!(s.directory(), project.to_str().unwrap());

        let _ = std::fs::remove_dir_all(&base);
    }

    // ------------------------------------------------------------------
    // M2 permission gate
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn deny_rule_blocks_bash_without_executing() {
        let base = std::env::temp_dir().join(format!("bebok-deny-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let sentinel = project.join("must-survive.txt");
        tokio::fs::write(&sentinel, "do not delete").await.unwrap();
        write_project_permission(
            &project,
            r#"{ "rules": [ { "pattern": "bash(rm *)", "action": "deny" } ] }"#,
        );
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session.append_user_message("delete must-survive.txt").await.unwrap();

        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step(
                "bash",
                serde_json::json!({ "command": "rm -f must-survive.txt" }),
                1,
            ),
            text_step("rm is blocked"),
        ]));
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();
        let mut events = bus.subscribe();
        let abort = CancellationToken::new();

        session.try_begin_turn();
        run_turn(
            session.clone(),
            Agent::code(),
            tools,
            provider,
            permission,
            bus,
            abort,
            crate::config::DEFAULT_MODEL,
        )
        .await
        .unwrap();
        session.end_turn();

        // The command must never have run.
        assert!(
            tokio::fs::read_to_string(&sentinel).await.is_ok(),
            "rm must not execute when denied by policy"
        );

        let messages = session.messages_snapshot().await;
        let tool_err = messages[1].parts.iter().any(|p| {
            matches!(p, Part::Tool { name, state: ToolState::Error { error, .. }, .. }
                if name == "bash" && error == "denied by policy")
        });
        assert!(tool_err, "tool must be Error with 'denied by policy'");

        let kinds: std::collections::HashSet<String> = drain_events(&mut events, 300)
            .await
            .into_iter()
            .map(|ev| ev.kind)
            .collect();
        assert!(!kinds.contains("permission.asked"), "a deny rule must not ask");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn unknown_bash_asks_then_executes_when_allowed() {
        let base = std::env::temp_dir().join(format!("bebok-ask-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session.append_user_message("print the working directory").await.unwrap();

        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step("bash", serde_json::json!({ "command": "pwd" }), 1),
            text_step("done"),
        ]));
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();
        let mut events = bus.subscribe();

        // A second, already-subscribed receiver answers the ask.
        let resolver = tokio::spawn(resolve_first_ask(
            session.clone(),
            bus.subscribe(),
            PermissionAnswer {
                allow: true,
                always: false,
            },
        ));

        let abort = CancellationToken::new();
        session.try_begin_turn();
        tokio::time::timeout(
            Duration::from_secs(10),
            run_turn(
                session.clone(),
                Agent::code(),
                tools,
                provider,
                permission,
                bus,
                abort,
                crate::config::DEFAULT_MODEL,
            ),
        )
        .await
        .expect("turn must finish after the ask is resolved")
        .unwrap();
        session.end_turn();
        resolver.await.unwrap();

        // Tool executed successfully.
        let messages = session.messages_snapshot().await;
        let completed = messages[1].parts.iter().any(|p| {
            matches!(p, Part::Tool { name, state: ToolState::Completed { output, .. }, .. }
                if name == "bash" && output.contains("project"))
        });
        assert!(completed, "allowed bash call must execute");

        // Events: asked + resolved(allow).
        let evs = drain_events(&mut events, 300).await;
        assert_eq!(evs.iter().filter(|e| e.kind == "permission.asked").count(), 1);
        let resolved = evs
            .iter()
            .find(|e| e.kind == "permission.resolved")
            .expect("permission.resolved event");
        assert_eq!(resolved.properties["decision"], "allow");
        assert_eq!(resolved.properties["always"], false);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn ask_denied_by_user_marks_error() {
        let base = std::env::temp_dir().join(format!("bebok-askdeny-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session.append_user_message("print the working directory").await.unwrap();

        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step("bash", serde_json::json!({ "command": "pwd" }), 1),
            text_step("user denied"),
        ]));
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();

        let resolver = tokio::spawn(resolve_first_ask(
            session.clone(),
            bus.subscribe(),
            PermissionAnswer {
                allow: false,
                always: false,
            },
        ));

        let abort = CancellationToken::new();
        session.try_begin_turn();
        tokio::time::timeout(
            Duration::from_secs(10),
            run_turn(
                session.clone(),
                Agent::code(),
                tools,
                provider,
                permission,
                bus,
                abort,
                crate::config::DEFAULT_MODEL,
            ),
        )
        .await
        .expect("turn must finish after the ask is denied")
        .unwrap();
        session.end_turn();
        resolver.await.unwrap();

        let messages = session.messages_snapshot().await;
        let tool_err = messages[1].parts.iter().any(|p| {
            matches!(p, Part::Tool { name, state: ToolState::Error { error, .. }, .. }
                if name == "bash" && error == "denied by user")
        });
        assert!(tool_err, "user-denied call must be Error with 'denied by user'");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn always_allow_persists_rule_and_skips_repeat_ask() {
        let base = std::env::temp_dir().join(format!("bebok-always-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session.append_user_message("print the working directory twice").await.unwrap();

        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        // Two identical calls back to back; the second must not re-ask.
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step("bash", serde_json::json!({ "command": "pwd" }), 1),
            tool_step("bash", serde_json::json!({ "command": "pwd" }), 2),
            text_step("done twice"),
        ]));
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();
        let mut events = bus.subscribe();

        let resolver = tokio::spawn(resolve_first_ask(
            session.clone(),
            bus.subscribe(),
            PermissionAnswer {
                allow: true,
                always: true,
            },
        ));

        let abort = CancellationToken::new();
        session.try_begin_turn();
        tokio::time::timeout(
            Duration::from_secs(10),
            run_turn(
                session.clone(),
                Agent::code(),
                tools,
                provider,
                permission.clone(),
                bus,
                abort,
                crate::config::DEFAULT_MODEL,
            ),
        )
        .await
        .expect("turn must finish")
        .unwrap();
        session.end_turn();
        resolver.await.unwrap();

        // Only one ask for the identical (tool, pattern) within the session.
        let evs = drain_events(&mut events, 300).await;
        let asked = evs.iter().filter(|e| e.kind == "permission.asked").count();
        assert_eq!(asked, 1, "repeat call must go through without asking");
        let resolved = evs
            .iter()
            .filter(|e| e.kind == "permission.resolved")
            .count();
        assert_eq!(resolved, 1);

        // A rule `ask -> allow` was persisted to the project config.
        let config_text = tokio::fs::read_to_string(project.join(".bebok").join("config.json"))
            .await
            .unwrap();
        assert!(config_text.contains("bash(pwd)"), "rule persisted: {config_text}");
        assert!(config_text.contains("allow"), "rule persisted: {config_text}");

        // The engine's in-memory layer was recompiled, so the next identical
        // call resolves straight to Allow.
        let eval = permission.evaluate(
            None,
            "bash",
            &serde_json::json!({ "command": "pwd" }),
            false,
        );
        assert_eq!(eval.verdict, Verdict::Allow);

        // Both tool calls completed.
        let messages = session.messages_snapshot().await;
        let completed = messages
            .iter()
            .flat_map(|m| &m.parts)
            .filter(|p| matches!(p, Part::Tool { name, .. } if name == "bash"))
            .count();
        assert_eq!(completed, 2, "both identical calls must have executed");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn read_only_tool_runs_by_default_without_asking() {
        let base = std::env::temp_dir().join(format!("bebok-readonly-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        tokio::fs::write(project.join("data.txt"), "hello world").await.unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session.append_user_message("read data.txt").await.unwrap();

        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step("read_file", serde_json::json!({ "path": "data.txt" }), 1),
            text_step("read done"),
        ]));
        // No permission config at all.
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();
        let mut events = bus.subscribe();
        let abort = CancellationToken::new();

        session.try_begin_turn();
        run_turn(
            session.clone(),
            Agent::code(),
            tools,
            provider,
            permission,
            bus,
            abort,
            crate::config::DEFAULT_MODEL,
        )
        .await
        .unwrap();
        session.end_turn();

        let messages = session.messages_snapshot().await;
        let completed = messages[1].parts.iter().any(|p| {
            matches!(p, Part::Tool { name, state: ToolState::Completed { output, .. }, .. }
                if name == "read_file" && output.contains("hello world"))
        });
        assert!(completed, "read-only tool must run by default (Allow)");

        let kinds: std::collections::HashSet<String> = drain_events(&mut events, 300)
            .await
            .into_iter()
            .map(|ev| ev.kind)
            .collect();
        assert!(!kinds.contains("permission.asked"));

        let _ = std::fs::remove_dir_all(&base);
    }

    // ------------------------------------------------------------------
    // M4: agents, skills, MCP
    // ------------------------------------------------------------------

    #[test]
    fn agent_catalog_loads_file_presets() {
        let base = std::env::temp_dir().join(format!("bebok-agents-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        let agent_dir = project.join(".bebok").join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("my.md"),
            "---\nname: my\ndescription: custom\nmodel: zai/glm-5.3\ntools: [\"read_file\", \"grep\"]\npermissions:\n  - pattern: \"bash(git *)\"\n    action: \"allow\"\n---\nYou are my custom agent.\n",
        )
        .unwrap();

        let catalog = AgentCatalog::load(&project);
        let agent = catalog.resolve("my");
        assert_eq!(agent.name, "my");
        assert_eq!(agent.model.as_deref(), Some("zai/glm-5.3"));
        assert_eq!(agent.tools, vec!["read_file", "grep"]);
        assert_eq!(agent.permissions.len(), 1);
        assert_eq!(agent.permissions[0].pattern, "bash(git *)");
        assert!(agent.prompt.contains("custom agent"));
        assert!(!agent.builtin);

        // Built-ins resolve, and unknown names fall back to `code`.
        assert!(catalog.contains("code"));
        assert_eq!(catalog.resolve("ask").name, "ask");
        assert_eq!(catalog.resolve("nope").name, "code");

        let infos = catalog.list();
        assert!(infos.iter().any(|a| a.name == "my" && !a.builtin));
        assert!(infos.iter().any(|a| a.name == "code" && a.builtin));

        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn hot_reload_picks_up_new_agent_file() {
        let base = std::env::temp_dir().join(format!("bebok-hot-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();

        let catalog = Arc::new(std::sync::RwLock::new(AgentCatalog::load(&project)));
        let bus = EventBus::default();
        let mut rx = bus.subscribe();
        let _watcher = spawn_agent_watcher(project.clone(), catalog.clone(), bus.clone()).unwrap();

        // Give the watcher a moment to register before writing the file.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let agent_dir = project.join(".bebok").join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("my.md"), "---\nname: my\n---\nhello\n").unwrap();

        let mut saw_event = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let ev = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
            match ev {
                Ok(Ok(ev)) if ev.kind == "agent.list.changed" => {
                    saw_event = true;
                    break;
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(_) => {}
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
        }
        assert!(saw_event, "expected agent.list.changed event");
        assert!(catalog.read().unwrap().contains("my"));

        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn mcp_tool_goes_through_permission_gate() {
        let base = std::env::temp_dir().join(format!("bebok-mcp-gate-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let script = base.join("server.py");
        std::fs::write(&script, MCP_SERVER_PY).unwrap();

        let data = base.join("data");
        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session.append_user_message("mutate target.txt").await.unwrap();

        // Connect the MCP server and register its tools alongside built-ins.
        let manager = McpManager::new();
        let runtimes = Runtimes::default();
        let spec = McpServerSpec {
            name: "test".to_string(),
            transport: McpTransport::Stdio {
                command: runtimes.python3.clone(),
                args: vec![script.to_str().unwrap().to_string()],
                env: Default::default(),
            },
            enabled: true,
        };
        let mcp_tools = manager.sync(&[spec], &runtimes).await;
        assert_eq!(mcp_tools.len(), 2);
        let registry = ToolRegistry::new(builtin_tools());
        registry.set_mcp_tools(mcp_tools);
        assert!(registry.get("mcp__test__mutate").is_some());

        let tools = Arc::new(registry);
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step("mcp__test__mutate", serde_json::json!({ "target": "target.txt" }), 1),
            text_step("done"),
        ]));
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();
        let mut events = bus.subscribe();

        // Answer the resulting ask with allow.
        let resolver = tokio::spawn(resolve_first_ask(
            session.clone(),
            bus.subscribe(),
            PermissionAnswer {
                allow: true,
                always: false,
            },
        ));

        let abort = CancellationToken::new();
        session.try_begin_turn();
        tokio::time::timeout(
            Duration::from_secs(15),
            run_turn(
                session.clone(),
                Agent::code(),
                tools,
                provider,
                permission,
                bus,
                abort,
                crate::config::DEFAULT_MODEL,
            ),
        )
        .await
        .expect("turn must finish")
        .unwrap();
        session.end_turn();
        resolver.await.unwrap();

        // The MCP tool (non-read-only) must have been gated by `ask`, then run.
        let evs = drain_events(&mut events, 300).await;
        assert_eq!(
            evs.iter().filter(|e| e.kind == "permission.asked").count(),
            1,
            "MCP tool must go through the permission gate"
        );
        let messages = session.messages_snapshot().await;
        let completed = messages.iter().flat_map(|m| &m.parts).any(|p| {
            matches!(p, Part::Tool { name, state: ToolState::Completed { output, .. }, .. }
                if name == "mcp__test__mutate" && output.contains("mutated target.txt"))
        });
        assert!(completed, "MCP tool must execute after allow");

        std::fs::remove_dir_all(&base).ok();
    }

    // ------------------------------------------------------------------
    // Plugin hooks (event-observer extension points)
    // ------------------------------------------------------------------

    /// A plugin that vetoes every `bash` call at the `before.tool` hook.
    struct VetoBash;

    #[async_trait::async_trait]
    impl crate::plugin::BebokPlugin for VetoBash {
        fn name(&self) -> &str {
            "test-veto-bash"
        }
        async fn on_hook(
            &self,
            hook: crate::plugin::Hook,
            payload: &mut serde_json::Value,
        ) -> crate::plugin::HookResult {
            use crate::plugin::HookResult;
            if hook == crate::plugin::Hook::BEFORE_TOOL
                && payload.get("tool").and_then(|v| v.as_str()) == Some("bash")
            {
                if let Some(serde_json::Value::Bool(allowed)) = payload.get_mut("allowed") {
                    *allowed = false;
                }
                return HookResult::Changed;
            }
            HookResult::Continue
        }
    }

    #[tokio::test]
    async fn plugin_hook_vetoes_tool_during_turn() {
        let base = std::env::temp_dir().join(format!("bebok-plugin-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let sentinel = project.join("must-survive.txt");
        tokio::fs::write(&sentinel, "do not delete").await.unwrap();
        // The permission config *allows* bash; only the plugin veto stops it.
        write_project_permission(
            &project,
            r#"{ "rules": [ { "pattern": "bash(*)", "action": "allow" } ] }"#,
        );
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session
            .append_user_message("delete must-survive.txt")
            .await
            .unwrap();

        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step("bash", serde_json::json!({ "command": "rm -f must-survive.txt" }), 1),
            text_step("rm is vetoed"),
        ]));
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();
        let abort = CancellationToken::new();

        // Register the plugin on the global host (what run_turn reads), run the
        // turn, then unregister so no other test sees it.
        let host = PluginHost::global();
        host.register(Arc::new(VetoBash)).await;
        let result = {
            session.try_begin_turn();
            let out = tokio::time::timeout(
                Duration::from_secs(10),
                run_turn(
                    session.clone(),
                    Agent::code(),
                    tools,
                    provider,
                    permission,
                    bus,
                    abort,
                    crate::config::DEFAULT_MODEL,
                ),
            )
            .await
            .expect("turn must finish")
            .unwrap();
            session.end_turn();
            out
        };
        let _ = result;
        host.unregister("test-veto-bash").await;

        // The plugin vetoed the call before execution: file untouched, part is
        // an Error carrying "denied by plugin".
        assert!(
            tokio::fs::read_to_string(&sentinel).await.is_ok(),
            "plugin veto must prevent execution"
        );
        let messages = session.messages_snapshot().await;
        let vetoed = messages.iter().flat_map(|m| &m.parts).any(|p| {
            matches!(p, Part::Tool { name, state: ToolState::Error { error, .. }, .. }
                if name == "bash" && error == "denied by plugin")
        });
        assert!(vetoed, "tool part must be Error with 'denied by plugin'");

        std::fs::remove_dir_all(&base).ok();
    }

}
