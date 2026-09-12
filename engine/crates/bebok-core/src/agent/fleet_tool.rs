//! `fleet` tool: run prompts across selected configured fleet members concurrently.
//!
//! Two modes:
//!
//! * Broadcast mode (default): one `prompt` is broadcast to all selected
//!   members (`names`/`agents` filters). Each member gets its own isolated
//!   child session (own transcript), runs its own turn loop with the same
//!   tool registry and permission engine, and returns only its final text.
//!   The parent transcript stays clean — the combined per-member sections
//!   are the single tool output.
//!
//! * Heterogeneous mode: `tasks` carries per-task prompts
//!   (`[{"prompt": "research auth", "agent": "ask"},
//!     {"prompt": "research db", "agent": "ask"}]`). Each task gets its own
//!   isolated child session via the same `run_member` path (synthetic
//!   `FleetMember { name, agent, model: "" }`) and all tasks run
//!   concurrently. Use this when there is more than one task AND the tasks
//!   do not affect each other, and fleet members are available.
//!
//! Orchestrator-only (see `request.rs`: the tool definition is withheld from
//! every other agent).
//!
//! Lifecycle mirrors the `task` tool: `task.started` per member when spawned,
//! `task.ended` per member when finished, plus one `fleet.started` on the
//! parent. Cancelling the parent turn propagates into every child via
//! `ctx.abort.child_token()`.
//!
//! Member selection (broadcast): `names` selects by exact member name,
//! `agents` selects by agent preset type (case-insensitive, e.g. `["ask"]`
//! runs only members whose `member.agent` is `ask`). When both are present
//! they intersect (AND). No filters (or only empty arrays) runs every
//! configured member.
//!
//! Heterogeneous selection: each `tasks` entry is
//! `{ prompt, agent?, name?, member? }` (all fields trimmed; `prompt` must
//! be non-empty). Execution agent resolves as `task.agent` if given, else
//! the configured member's agent when `task.member` matches a configured
//! member, else `"code"`. Display name resolves as `task.member` (when it
//! matches) else `task.name` else the agent name. A `task.member` that
//! matches no configured member is an error listing valid members.

use std::sync::Weak;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use bebok_tools::{Tool, ToolCtx, ToolOutput};

use super::images::{AgentImageInput, validate_agent_images};
use super::task_tool::MAX_TASK_DEPTH;
use super::turn::run_turn;
use crate::provider::build_provider;
use crate::session::Role;
use crate::store::InstanceStore;

/// The `fleet` tool. One instance is registered per directory `Instance`.
pub struct FleetTool {
    /// Weak back-reference to the store (same ownership reasoning as `TaskTool`).
    store: Weak<InstanceStore>,
    /// Delegation depth guard (shared; a fleet fan-out counts as one level).
    depth: AtomicUsize,
    max_depth: usize,
}

impl FleetTool {
    pub fn new(store: Weak<InstanceStore>) -> Self {
        Self {
            store,
            depth: AtomicUsize::new(0),
            max_depth: MAX_TASK_DEPTH,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
struct FleetTask {
    /// Standalone instruction for this task (must be non-empty after trim).
    prompt: String,
    /// Optional agent preset override for this task (e.g. "ask").
    #[serde(default)]
    agent: Option<String>,
    /// Optional display-name override for this task's output section.
    #[serde(default)]
    name: Option<String>,
    /// Optional configured fleet member name this task targets.
    #[serde(default)]
    member: Option<String>,
    /// Optional image attachments for this task (raw base64, max 5, max 5 MB each).
    #[serde(default)]
    images: Option<Vec<AgentImageInput>>,
}

#[derive(Debug, Deserialize)]
struct FleetArgs {
    /// Standalone instruction broadcast to every selected member.
    /// Optional when `tasks` carries per-task prompts (heterogeneous mode).
    #[serde(default)]
    prompt: String,
    /// Optional subset of configured member names. Defaults to all members.
    #[serde(default)]
    names: Option<Vec<String>>,
    /// Optional subset by agent preset type (e.g. ["ask"] runs only members
    /// whose `member.agent` is "ask", case-insensitive). Defaults to all.
    #[serde(default)]
    agents: Option<Vec<String>>,
    /// Optional per-task prompts for heterogeneous mode: each entry runs
    /// concurrently in its own isolated child session with its own prompt.
    /// When present with >=1 non-empty-prompt entry, `tasks` wins over the
    /// broadcast `prompt`/`names`/`agents` selection.
    #[serde(default)]
    tasks: Option<Vec<FleetTask>>,
    /// Optional image attachments broadcast to every selected member
    /// (raw base64, max 5). Ignored in heterogeneous mode (per-task `images` win).
    #[serde(default)]
    images: Option<Vec<AgentImageInput>>,
}

/// A `tasks` entry with all fields trimmed; `prompt` guaranteed non-empty.
struct CleanTask {
    prompt: String,
    agent: Option<String>,
    name: Option<String>,
    member: Option<String>,
    images: Vec<AgentImageInput>,
}

/// Clean `tasks` entries (trim every field; drop entries with an empty
/// prompt). Non-empty output means heterogeneous mode.
fn clean_tasks(raw: Option<Vec<FleetTask>>) -> Vec<CleanTask> {
    raw.unwrap_or_default()
        .into_iter()
        .filter_map(|t| {
            let prompt = t.prompt.trim().to_string();
            if prompt.is_empty() {
                return None;
            }
            let opt = |v: Option<String>| {
                let s = v.unwrap_or_default().trim().to_string();
                if s.is_empty() { None } else { Some(s) }
            };
            Some(CleanTask {
                prompt,
                agent: opt(t.agent),
                name: opt(t.name),
                member: opt(t.member),
                images: t.images.unwrap_or_default(),
            })
        })
        .collect()
}

/// Resolve heterogeneous tasks to `(synthetic member, prompt)` pairs.
///
/// Execution agent: `task.agent` if given, else the configured member's
/// agent when `task.member` matches, else `"code"`. Display name:
/// `task.member` (when matched) else `task.name` else the agent name.
/// A `task.member` matching no configured member is an error listing the
/// valid members (same style as the unknown-`names` error).
fn resolve_hetero_members(
    configured: &[crate::config::FleetMember],
    tasks: &[CleanTask],
) -> Result<Vec<(crate::config::FleetMember, String, Vec<AgentImageInput>)>, String> {
    let mut out = Vec::with_capacity(tasks.len());
    for t in tasks {
        let matched = t
            .member
            .as_ref()
            .and_then(|m| configured.iter().find(|c| &c.name == m));
        if let Some(want) = t.member.as_ref()
            && matched.is_none()
        {
            let valid: Vec<&str> = configured.iter().map(|m| m.name.as_str()).collect();
            return Err(format!(
                "fleet: unknown member(s) {} (configured: {})",
                want,
                valid.join(", ")
            ));
        }
        let agent = t
            .agent
            .clone()
            .or_else(|| {
                matched
                    .map(|m| m.agent.trim().to_string())
                    .filter(|a| !a.is_empty())
            })
            .unwrap_or_else(|| "code".to_string());
        let display = t
            .member
            .clone()
            .or_else(|| t.name.clone())
            .unwrap_or_else(|| agent.clone());
        out.push((
            crate::config::FleetMember {
                name: display,
                agent,
                model: String::new(),
            },
            t.prompt.clone(),
            t.images.clone(),
        ));
    }
    Ok(out)
}

/// Cleaned `names`/`agents` filter values (trimmed; agents lowercased;
/// empty entries dropped). Empty output means "no filter".
fn clean_names(raw: Option<Vec<String>>) -> Vec<String> {
    raw.unwrap_or_default()
        .into_iter()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .collect()
}

/// Cleaned `agents` filter values (trimmed + lowercased for
/// case-insensitive matching; empty entries dropped).
fn clean_agents(raw: Option<Vec<String>>) -> Vec<String> {
    raw.unwrap_or_default()
        .into_iter()
        .map(|a| a.trim().to_lowercase())
        .filter(|a| !a.is_empty())
        .collect()
}

/// Select fleet members: `names` is an exact member-name match, `agents` is a
/// case-insensitive `member.agent` match; both intersect (AND). Empty inputs
/// mean "no filter" (all members). Returns `Err(message)` for unknown names
/// or an empty intersection.
fn select_members(
    configured: &[crate::config::FleetMember],
    wanted_names: &[String],
    wanted_agents: &[String],
) -> Result<Vec<crate::config::FleetMember>, String> {
    if wanted_names.is_empty() && wanted_agents.is_empty() {
        return Ok(configured.to_vec());
    }
    if !wanted_names.is_empty() {
        let mut unknown = Vec::new();
        for w in wanted_names {
            if !configured.iter().any(|m| &m.name == w) {
                unknown.push(w.clone());
            }
        }
        if !unknown.is_empty() {
            let valid: Vec<&str> = configured.iter().map(|m| m.name.as_str()).collect();
            return Err(format!(
                "fleet: unknown member(s) {} (configured: {})",
                unknown.join(", "),
                valid.join(", ")
            ));
        }
    }
    let filtered: Vec<crate::config::FleetMember> = configured
        .iter()
        .filter(|m| {
            (wanted_names.is_empty() || wanted_names.contains(&m.name))
                && (wanted_agents.is_empty()
                    || wanted_agents.contains(&m.agent.trim().to_lowercase()))
        })
        .cloned()
        .collect();
    if filtered.is_empty() {
        let configured_list: Vec<String> = configured
            .iter()
            .map(|m| format!("{}({})", m.name, m.agent))
            .collect();
        // Unknown/unpresent agent types get the helpful message; a valid
        // agent type whose intersection with `names` is empty falls through
        // to the generic filters message below.
        let agents_match_any = wanted_agents.is_empty()
            || configured
                .iter()
                .any(|m| wanted_agents.contains(&m.agent.trim().to_lowercase()));
        if !wanted_agents.is_empty() && !agents_match_any {
            // Unknown agent types are not a hard error; report what
            // agent types are actually present.
            let mut available: Vec<String> = configured
                .iter()
                .map(|m| m.agent.trim().to_lowercase())
                .filter(|a| !a.is_empty())
                .collect();
            available.sort();
            available.dedup();
            return Err(format!(
                "fleet: no members with agent(s) {} (available agents: {}; members: {})",
                wanted_agents.join(", "),
                if available.is_empty() {
                    "(none)".to_string()
                } else {
                    available.join(", ")
                },
                configured_list.join(", ")
            ));
        }
        return Err(format!(
            "fleet: no members match filters (names=[{}] agents=[{}]; configured: {})",
            wanted_names.join(", "),
            wanted_agents.join(", "),
            configured_list.join(", ")
        ));
    }
    Ok(filtered)
}

/// RAII depth guard: decrements the shared counter on drop.
struct DepthGuard<'a>(&'a AtomicUsize);

impl<'a> DepthGuard<'a> {
    fn enter(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(counter)
    }
}

impl Drop for DepthGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Per-member outcome collected from the concurrent fan-out.
struct MemberResult {
    name: String,
    agent: String,
    child_session_id: String,
    ok: bool,
    text: String,
}

#[async_trait]
impl Tool for FleetTool {
    fn name(&self) -> &str {
        "fleet"
    }

    fn description(&self) -> &str {
        "Run one prompt across the configured parallel-fleet members concurrently; each member \
         works in an isolated context and returns only its final answer, combined into per-member \
         sections. Members are fixed and user-named — this tool cannot create members or target \
         arbitrary presets or counts. Optional `names` selects by exact member name; optional \
         `agents` selects by agent preset type, case-insensitive (e.g. {\"agents\":[\"ask\"]} runs \
         only ask members); both intersect (AND). WARNING: omitting both filters runs EVERY \
         configured member — never do that when the user asked for a specific type or count. \
         If the filtered selection is bigger than the count the user asked for, use N parallel \
         `task` calls with agent=<type> instead. Heterogeneous mode: pass `tasks` with per-task \
         prompts for independent tasks, e.g. {\"tasks\": [{\"prompt\": \"research auth\", \
         \"agent\": \"ask\"}, {\"prompt\": \"research db\", \"agent\": \"ask\"}]} — each entry \
         is {prompt (required), agent?, name?, member?, images?} and runs concurrently in its own \
         isolated session; `task.member` must match a configured member name. Use fleet when \
         the user explicitly requests parallel execution or when running many independent tasks \
         concurrently is clearly beneficial and fleet members are available. For most delegation, \
         prefer `task` calls. Returns member names; report which ran."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "A complete, standalone instruction broadcast to every selected fleet member (they cannot see this conversation)."
                },
                "names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Exact configured member names to run (only names the user actually used, never agent types). Empty/omitted = no restriction."
                },
                "agents": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Agent preset types to run, case-insensitive (e.g. [\"ask\"] runs only ask members). Prefer this over `names` when the user asked for a type or a count of a type. Intersects with `names`. Empty/omitted = no restriction (with no `names` either, runs all members)."
                },
                "images": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "media_type": { "type": "string" },
                            "data": { "type": "string" },
                            "name": { "type": "string" }
                        },
                        "required": ["media_type", "data"]
                    },
                    "description": "Optional image attachments broadcast to every selected member (raw base64, max 5, max 5 MB each). Ignored in heterogeneous mode (use per-task images)."
                },
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "prompt": { "type": "string", "description": "Standalone instruction for this task (required, non-empty)." },
                            "agent": { "type": "string", "description": "Agent preset override for this task (e.g. \"ask\")." },
                            "name": { "type": "string", "description": "Display-name override for this task's output section." },
                            "member": { "type": "string", "description": "Configured fleet member name this task targets (must match)." },
                            "images": { "type": "array", "description": "Optional image attachments for this task (raw base64, max 5, max 5 MB each)." }
                        },
                        "required": ["prompt"]
                    },
                    "description": "Heterogeneous mode: per-task prompts run concurrently, each in its own isolated session (e.g. {\"tasks\": [{\"prompt\": \"research auth\", \"agent\": \"ask\"}, {\"prompt\": \"research db\", \"agent\": \"ask\"}]}). When present with at least one non-empty prompt, `tasks` wins over broadcast `prompt`/`names`/`agents`."
                }
            },
            "required": ["prompt"]
        })
    }

    /// Spawning only — every effect a member makes goes through the same
    /// permission gate. Marking it read-only means the fan-out itself is not
    /// an extra approval prompt.
    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let args: FleetArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutput::new(format!("fleet: invalid arguments: {e}"), "fleet"),
        };
        // Mode selection: `tasks` with >=1 non-empty prompt wins
        // (heterogeneous); otherwise broadcast (unchanged behavior).
        let hetero_tasks = clean_tasks(args.tasks);
        let hetero_mode = !hetero_tasks.is_empty();
        // Broadcast still requires a non-empty prompt. Heterogeneous mode
        // carries per-task prompts, so the top-level `prompt` may be empty.
        let prompt = args.prompt.trim().to_string();
        if !hetero_mode && prompt.is_empty() {
            return ToolOutput::new("fleet: `prompt` must not be empty", "fleet");
        }

        if self.depth.load(Ordering::Acquire) >= self.max_depth {
            return ToolOutput::new(
                format!(
                    "fleet: maximum delegation depth ({}) reached; complete the work directly",
                    self.max_depth
                ),
                "fleet",
            );
        }

        let Some(store) = self.store.upgrade() else {
            return ToolOutput::new("fleet: engine store is gone", "fleet");
        };
        let _guard = DepthGuard::enter(&self.depth);

        let directory = ctx.root.to_string_lossy().to_string();
        let instance = match store.get_or_create_instance(&directory).await {
            Ok(i) => i,
            Err(e) => return ToolOutput::new(format!("fleet: cannot open instance: {e}"), "fleet"),
        };
        let cfg = instance.config_snapshot();

        if !cfg.is_fleet_enabled() {
            return ToolOutput::new(
                "fleet: fleet is not enabled (set `fleet.enabled` and configure `fleet.members`)",
                "fleet",
            );
        }
        let configured = cfg.fleet_members();
        if configured.is_empty() {
            return ToolOutput::new(
                "fleet: fleet is enabled but no members are configured",
                "fleet",
            );
        }

        // Resolve work items: (member, per-task prompt, images) triples.
        // Heterogeneous mode ignores broadcast `prompt`/`names`/`agents`;
        // broadcast mode fans one prompt out to the filtered selection.
        // Returns (work, fleet.started properties): broadcast keeps its
        // existing event shape; heterogeneous reports mode + task names.
        let wanted_names = clean_names(args.names);
        let wanted_agents = clean_agents(args.agents);
        let (work, event_props): (
            Vec<(
                crate::config::FleetMember,
                String,
                Vec<crate::session::Part>,
            )>,
            Value,
        ) = if hetero_mode {
            let resolved = match resolve_hetero_members(configured, &hetero_tasks) {
                Ok(w) => w,
                Err(msg) => return ToolOutput::new(msg, "fleet"),
            };
            let count = resolved.len();
            let mut work_items = Vec::with_capacity(count);
            for (m, p, imgs) in resolved {
                let parts = match validate_agent_images(imgs) {
                    Ok(p) => p,
                    Err(e) => return ToolOutput::new(format!("fleet: {e}"), "fleet"),
                };
                work_items.push((m, p, parts));
            }
            let task_names: Vec<String> =
                work_items.iter().map(|(m, _, _)| m.name.clone()).collect();
            let props = json!({
                "mode": "heterogeneous",
                "members": task_names.clone(),
                "tasks": task_names,
                "count": count,
            });
            (work_items, props)
        } else {
            // Optional subset selection: `names` (exact member name)
            // and/or `agents` (case-insensitive agent preset type). Both
            // intersect (AND); no filters (or only empty arrays) selects
            // all members.
            let selected: Vec<crate::config::FleetMember> =
                match select_members(configured, &wanted_names, &wanted_agents) {
                    Ok(s) => s,
                    Err(msg) => return ToolOutput::new(msg, "fleet"),
                };
            if selected.is_empty() {
                return ToolOutput::new("fleet: no members selected", "fleet");
            }
            let props = json!({
                "members": selected.iter().map(|m| m.name.clone()).collect::<Vec<_>>(),
                "count": selected.len(),
                "names": wanted_names,
                "agents": wanted_agents,
            });
            let broadcast_parts = match validate_agent_images(args.images.unwrap_or_default()) {
                Ok(p) => p,
                Err(e) => return ToolOutput::new(format!("fleet: {e}"), "fleet"),
            };
            let items = selected
                .into_iter()
                .map(|m| (m, prompt.clone(), broadcast_parts.clone()))
                .collect();
            (items, props)
        };
        if work.is_empty() {
            return ToolOutput::new("fleet: no members selected", "fleet");
        }

        let parent = match store.open_session(parse_uuid(&ctx.session_id)).await {
            Ok(p) => p,
            Err(_) => {
                return ToolOutput::new("fleet: parent session not found", "fleet");
            }
        };

        let bus = store.bus();
        bus.publish(
            crate::event::Event::new("fleet.started", &directory, &ctx.session_id)
                .with_properties(event_props),
        );

        // Fan out concurrently; each task owns an isolated child session.
        let futures: Vec<_> = work
            .iter()
            .map(|(member, task_prompt, images)| {
                run_member(
                    &store,
                    &instance,
                    &parent,
                    &cfg,
                    member,
                    task_prompt,
                    images.clone(),
                    &ctx,
                )
            })
            .collect();
        let results: Vec<MemberResult> = futures::future::join_all(futures).await;
        render_results(&results)
    }
}

/// Combine per-member outcomes into `# <name> (<agent>):\n<text>` sections
/// plus structured members (shared by broadcast + heterogeneous modes).
fn render_results(results: &[MemberResult]) -> ToolOutput {
    let succeeded = results.iter().filter(|r| r.ok).count();
    if succeeded == 0 {
        let details: Vec<String> = results
            .iter()
            .map(|r| format!("{}: {}", r.name, r.text))
            .collect();
        return ToolOutput::new(
            format!(
                "fleet: all {} member(s) failed:\n{}",
                results.len(),
                details.join("\n")
            ),
            "fleet",
        );
    }

    let mut sections = Vec::with_capacity(results.len());
    let mut structured = Vec::with_capacity(results.len());
    for r in results {
        let header = if r.ok {
            format!("# {} ({})", r.name, r.agent)
        } else {
            format!("# {} ({}) [failed]", r.name, r.agent)
        };
        sections.push(format!("{header}:\n{}", r.text));
        structured.push(json!({
            "name": r.name,
            "agent": r.agent,
            "childSessionID": r.child_session_id,
            "ok": r.ok,
        }));
    }
    let mut out = ToolOutput::new(sections.join("\n\n---\n\n"), "fleet");
    out.structured = Some(json!({ "members": structured }));
    out
}

/// Run one fleet member to completion: child session + turn + result text.
#[allow(clippy::too_many_arguments)]
async fn run_member(
    store: &InstanceStore,
    instance: &crate::store::Instance,
    parent: &crate::store::SessionState,
    cfg: &crate::config::ResolvedConfig,
    member: &crate::config::FleetMember,
    prompt: &str,
    images: Vec<crate::session::Part>,
    ctx: &ToolCtx,
) -> MemberResult {
    let member_name = member.name.trim();
    let display = if member_name.is_empty() {
        member.agent.clone()
    } else {
        member_name.to_string()
    };

    let agent_name = member.agent.trim();
    let agent_name = if agent_name.is_empty() {
        "code"
    } else {
        agent_name
    };
    let mut agent = instance.resolve_agent(agent_name);

    // Effective model: member override, else preset override, else
    // `models.<agent>` / global model.
    let explicit = member.model.trim();
    let model = if explicit.is_empty() {
        agent
            .model
            .clone()
            .unwrap_or_else(|| cfg.model_for(&agent.name))
    } else {
        explicit.to_string()
    };

    assemble_prompt(instance, &mut agent, cfg);

    let provider = match build_provider(cfg, &model) {
        Ok(p) => p,
        Err(e) => {
            return MemberResult {
                name: display,
                agent: agent_name.to_string(),
                child_session_id: String::new(),
                ok: false,
                text: format!("cannot build provider for '{model}': {e}"),
            };
        }
    };

    let name = parent
        .allocate_child_name(
            if member_name.is_empty() {
                None
            } else {
                Some(member_name)
            },
            &agent.name,
        )
        .await;

    let child = match store
        .create_subagent_session(parent, &agent.name, Some(&model), Some(&name))
        .await
    {
        Ok(c) => c,
        Err(e) => {
            return MemberResult {
                name,
                agent: agent_name.to_string(),
                child_session_id: String::new(),
                ok: false,
                text: format!("cannot create sub-session: {e}"),
            };
        }
    };

    if let Err(e) = child.append_user_message_with_images(prompt, images).await {
        return MemberResult {
            name,
            agent: agent_name.to_string(),
            child_session_id: child.id().to_string(),
            ok: false,
            text: format!("cannot record subtask: {e}"),
        };
    }

    if !child.try_begin_turn() {
        return MemberResult {
            name,
            agent: agent_name.to_string(),
            child_session_id: child.id().to_string(),
            ok: false,
            text: "sub-session is already busy".to_string(),
        };
    }
    let abort = ctx.abort.child_token();
    child.set_abort(abort.clone()).await;

    let task_id = uuid::Uuid::new_v4().to_string();
    let description: String = prompt.chars().take(80).collect();
    let child_session_id = child.id().to_string();
    let bus = store.bus();

    let child_info = parent
        .register_child_task_with_model(
            &task_id,
            &description,
            &child_session_id,
            &name,
            agent_name,
            Some(&model),
            abort.clone(),
        )
        .await;
    bus.publish(
        crate::event::Event::new("task.started", child.directory(), &ctx.session_id)
            .with_properties(serde_json::to_value(&child_info).unwrap_or_default()),
    );

    let result = run_turn(
        child.clone(),
        agent,
        instance.tools.clone(),
        provider,
        instance.permission.clone(),
        store.bus(),
        abort.clone(),
        &model,
    )
    .await;

    child.clear_abort().await;
    child.end_turn();

    let (status, error_msg) = match &result {
        Ok(()) => {
            if abort.is_cancelled() {
                (
                    "aborted".to_string(),
                    Some("sub-task was cancelled".to_string()),
                )
            } else {
                ("completed".to_string(), None)
            }
        }
        Err(e) => {
            let msg = e.to_string();
            if abort.is_cancelled() {
                (
                    "aborted".to_string(),
                    Some(format!("sub-task was cancelled: {msg}")),
                )
            } else {
                ("error".to_string(), Some(msg))
            }
        }
    };

    // Persist the outcome on the child (F6-12: `GET /session/{id}/agents`
    // reads it back once the member is no longer in the live map).
    child.set_task_status(&status, error_msg.as_deref()).await;

    parent.unregister_child_task(&task_id).await;
    bus.publish(
        crate::event::Event::new("task.ended", child.directory(), &ctx.session_id).with_properties(
            json!({
                "taskID": task_id,
                "status": status,
                "error": error_msg,
                "childSessionID": child_session_id,
                "name": name,
            }),
        ),
    );

    if abort.is_cancelled() {
        return MemberResult {
            name,
            agent: agent_name.to_string(),
            child_session_id,
            ok: false,
            text: "[aborted] sub-task was cancelled".to_string(),
        };
    }

    if let Err(e) = result {
        return MemberResult {
            name,
            agent: agent_name.to_string(),
            child_session_id,
            ok: false,
            text: format!("subtask failed: {e}"),
        };
    }

    let messages = child.messages_snapshot().await;
    let text = messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant)
        .map(|m| m.text_content())
        .unwrap_or_default();
    if text.trim().is_empty() {
        return MemberResult {
            name,
            agent: agent_name.to_string(),
            child_session_id,
            ok: false,
            text: "sub-agent produced no output".to_string(),
        };
    }
    MemberResult {
        name,
        agent: agent_name.to_string(),
        child_session_id,
        ok: true,
        text,
    }
}

/// Best-effort parse of the parent session id (a UUID string in `ToolCtx`).
fn parse_uuid(s: &str) -> uuid::Uuid {
    uuid::Uuid::parse_str(s).unwrap_or_else(|_| uuid::Uuid::nil())
}

/// Assemble AGENTS.md + enabled skills into the sub-agent's system prompt.
fn assemble_prompt(
    instance: &crate::store::Instance,
    agent: &mut super::preset::Agent,
    cfg: &crate::config::ResolvedConfig,
) {
    let mut discovered = crate::skills::discover(&instance.root);
    crate::skills::apply_toggles(&mut discovered, Some(&cfg.skills));
    let instructions = crate::skills::assemble_prompt(&discovered);
    if !instructions.is_empty() {
        agent.prompt = format!("{}\n\n{instructions}", agent.prompt);
    }

    // Fleet members are sub-agents too: they need the host-OS/shell note, or on
    // Windows they emit POSIX pipelines `cmd /C` cannot run.
    agent.prompt = format!("{}\n\n{}", agent.prompt, super::prompt_env::host_os_note());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FleetMember;

    fn members() -> Vec<FleetMember> {
        vec![
            FleetMember {
                name: "1".into(),
                agent: "ask".into(),
                model: String::new(),
            },
            FleetMember {
                name: "2".into(),
                agent: "ask".into(),
                model: String::new(),
            },
            FleetMember {
                name: "3".into(),
                agent: "code".into(),
                model: String::new(),
            },
            FleetMember {
                name: "4".into(),
                agent: "code".into(),
                model: String::new(),
            },
        ]
    }

    fn names(v: &[FleetMember]) -> Vec<String> {
        v.iter().map(|m| m.name.clone()).collect()
    }

    #[test]
    fn no_filters_selects_all() {
        let out = select_members(&members(), &[], &[]).unwrap();
        assert_eq!(names(&out), vec!["1", "2", "3", "4"]);
    }

    #[test]
    fn empty_arrays_select_all() {
        let n = clean_names(Some(vec![" ".into(), String::new()]));
        let a = clean_agents(Some(vec!["  ".into()]));
        let out = select_members(&members(), &n, &a).unwrap();
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn agents_filter_case_insensitive() {
        let a = clean_agents(Some(vec!["ASK".into(), " ask ".into()]));
        let out = select_members(&members(), &[], &a).unwrap();
        assert_eq!(names(&out), vec!["1", "2"]);
    }

    #[test]
    fn names_filter_exact_match() {
        let n = clean_names(Some(vec!["3".into()]));
        let out = select_members(&members(), &n, &[]).unwrap();
        assert_eq!(names(&out), vec!["3"]);
    }

    #[test]
    fn both_filters_intersect() {
        let n = clean_names(Some(vec!["1".into(), "3".into()]));
        let a = clean_agents(Some(vec!["ask".into()]));
        let out = select_members(&members(), &n, &a).unwrap();
        assert_eq!(names(&out), vec!["1"]);
    }

    #[test]
    fn unknown_names_hard_error() {
        let n = clean_names(Some(vec!["nope".into()]));
        let err = select_members(&members(), &n, &[]).unwrap_err();
        assert!(err.contains("unknown member(s)"), "{err}");
    }

    #[test]
    fn unknown_agents_helpful_message() {
        let a = clean_agents(Some(vec!["foo".into()]));
        let err = select_members(&members(), &[], &a).unwrap_err();
        assert!(err.contains("no members with agent(s) foo"), "{err}");
        assert!(err.contains("available agents: ask, code"), "{err}");
        assert!(err.contains("1(ask)"), "{err}");
    }

    #[test]
    fn empty_intersection_reports_filters() {
        let n = clean_names(Some(vec!["1".into()]));
        let a = clean_agents(Some(vec!["code".into()]));
        let err = select_members(&members(), &n, &a).unwrap_err();
        assert!(err.contains("no members match filters"), "{err}");
        assert!(err.contains("names=[1]"), "{err}");
        assert!(err.contains("agents=[code]"), "{err}");
    }

    #[test]
    fn args_deserialize_agents() {
        let args: FleetArgs = serde_json::from_value(json!({
            "prompt": "hi",
            "agents": ["ask"],
        }))
        .unwrap();
        assert_eq!(args.agents.unwrap(), vec!["ask"]);
    }

    #[test]
    fn schema_documents_agents() {
        // Schema check needs a FleetTool instance; verify via a throwaway
        // Weak store: schema/description don't touch the store.
        let tool = FleetTool {
            store: Weak::new(),
            depth: AtomicUsize::new(0),
            max_depth: 3,
        };
        let schema = Tool::parameters_schema(&tool);
        assert!(schema["properties"]["agents"].is_object());
        assert!(tool.description().contains("agents"));
    }

    fn hetero_tasks() -> Option<Vec<FleetTask>> {
        Some(vec![
            FleetTask {
                prompt: " research auth ".into(),
                agent: Some(" ask ".into()),
                name: None,
                member: None,
                images: None,
            },
            FleetTask {
                prompt: "research db".into(),
                agent: Some("ask".into()),
                name: Some(" db-task ".into()),
                member: None,
                images: None,
            },
        ])
    }

    #[test]
    fn hetero_deserialize_and_resolve() {
        let args: FleetArgs = serde_json::from_value(json!({
            "prompt": "",
            "tasks": [
                {"prompt": "research auth", "agent": "ask"},
                {"prompt": "research db", "agent": "ask", "name": " db-task "}
            ],
        }))
        .unwrap();
        let cleaned = clean_tasks(args.tasks);
        assert_eq!(cleaned.len(), 2);
        let resolved = resolve_hetero_members(&members(), &cleaned).unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].0.agent, "ask");
        assert_eq!(resolved[0].0.name, "ask");
        assert_eq!(resolved[0].1, "research auth");
        assert_eq!(resolved[1].0.agent, "ask");
        assert_eq!(resolved[1].0.name, "db-task");
        assert_eq!(resolved[1].1, "research db");
    }

    #[test]
    fn hetero_member_match_uses_config_agent_and_name() {
        let raw = Some(vec![FleetTask {
            prompt: " do thing ".into(),
            agent: None,
            name: None,
            member: Some("3".into()),
            images: None,
        }]);
        let cleaned = clean_tasks(raw);
        assert_eq!(cleaned.len(), 1);
        let resolved = resolve_hetero_members(&members(), &cleaned).unwrap();
        assert_eq!(resolved[0].0.name, "3");
        assert_eq!(resolved[0].0.agent, "code");
        assert_eq!(resolved[0].1, "do thing");
    }

    #[test]
    fn hetero_task_agent_overrides_member_agent() {
        let raw = Some(vec![FleetTask {
            prompt: "x".into(),
            agent: Some("ask".into()),
            name: None,
            member: Some("3".into()),
            images: None,
        }]);
        let cleaned = clean_tasks(raw);
        let resolved = resolve_hetero_members(&members(), &cleaned).unwrap();
        assert_eq!(resolved[0].0.agent, "ask");
        assert_eq!(resolved[0].0.name, "3");
    }

    #[test]
    fn hetero_empty_prompts_fall_back_to_broadcast() {
        let raw = Some(vec![
            FleetTask {
                prompt: "   ".into(),
                agent: Some("ask".into()),
                name: None,
                member: None,
                images: None,
            },
            FleetTask {
                prompt: String::new(),
                agent: None,
                name: None,
                member: None,
                images: None,
            },
        ]);
        let cleaned = clean_tasks(raw);
        assert!(cleaned.is_empty());
        // Empty `tasks` also means broadcast.
        assert!(clean_tasks(None).is_empty());
        assert!(clean_tasks(Some(vec![])).is_empty());
    }

    #[test]
    fn hetero_unknown_member_error_lists_valid() {
        let raw = Some(vec![FleetTask {
            prompt: "x".into(),
            agent: None,
            name: None,
            member: Some("nope".into()),
            images: None,
        }]);
        let cleaned = clean_tasks(raw);
        // Sanity: the task above is well-formed so cleaning keeps it.
        assert_eq!(cleaned.len(), 1);
        let err = resolve_hetero_members(&members(), &cleaned).unwrap_err();
        assert!(err.contains("unknown member(s) nope"), "{err}");
        assert!(err.contains("1, 2, 3, 4"), "{err}");
    }

    #[test]
    fn hetero_default_agent_is_code() {
        let _ = hetero_tasks();
        let raw = Some(vec![FleetTask {
            prompt: "x".into(),
            agent: None,
            name: None,
            member: None,
            images: None,
        }]);
        let cleaned = clean_tasks(raw);
        let resolved = resolve_hetero_members(&members(), &cleaned).unwrap();
        assert_eq!(resolved[0].0.agent, "code");
        assert_eq!(resolved[0].0.name, "code");
    }

    #[test]
    fn schema_documents_tasks() {
        let tool = FleetTool {
            store: Weak::new(),
            depth: AtomicUsize::new(0),
            max_depth: 3,
        };
        let schema = Tool::parameters_schema(&tool);
        let tasks = &schema["properties"]["tasks"];
        assert!(tasks.is_object(), "{schema}");
        let desc = tasks["description"].as_str().unwrap_or_default();
        assert!(desc.contains("research auth"), "{desc}");
        assert!(desc.contains("research db"), "{desc}");
        assert!(tool.description().contains("research auth"));
        assert!(tool.description().contains("research db"));
    }

    #[test]
    fn hetero_images_preserved_through_clean_and_resolve() {
        let raw = Some(vec![FleetTask {
            prompt: "look".into(),
            agent: None,
            name: None,
            member: None,
            images: Some(vec![crate::agent::images::AgentImageInput {
                media_type: "image/png".into(),
                data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=".into(),
                name: None,
            }]),
        }]);
        let cleaned = clean_tasks(raw);
        assert_eq!(cleaned.len(), 1);
        assert_eq!(cleaned[0].images.len(), 1);
        let resolved = resolve_hetero_members(&members(), &cleaned).unwrap();
        assert_eq!(resolved[0].2.len(), 1);
        let parts = crate::agent::images::validate_agent_images(resolved[0].2.clone()).unwrap();
        assert_eq!(parts.len(), 1);
        assert!(matches!(parts[0], crate::session::Part::Image { .. }));
    }

    #[test]
    fn hetero_too_many_images_rejected() {
        let many: Vec<crate::agent::images::AgentImageInput> = (0..6)
            .map(|_| crate::agent::images::AgentImageInput {
                media_type: "image/png".into(),
                data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=".into(),
                name: None,
            })
            .collect();
        let err = crate::agent::images::validate_agent_images(many).unwrap_err();
        assert!(err.contains("max 5 images"), "{err}");
    }

    #[test]
    fn hetero_bad_mime_rejected() {
        let bad = vec![crate::agent::images::AgentImageInput {
            media_type: "image/bmp".into(),
            data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=".into(),
            name: None,
        }];
        let err = crate::agent::images::validate_agent_images(bad).unwrap_err();
        assert!(err.contains("unsupported image media_type"), "{err}");
    }

    #[test]
    fn schema_documents_images() {
        let tool = FleetTool {
            store: Weak::new(),
            depth: AtomicUsize::new(0),
            max_depth: 3,
        };
        let schema = Tool::parameters_schema(&tool);
        assert!(schema["properties"]["images"].is_object());
        assert!(schema["properties"]["tasks"]["items"]["properties"]["images"].is_object());
    }
}
