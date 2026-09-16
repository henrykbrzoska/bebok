//! `task` tool: delegate a self-contained subtask to a sub-agent.
//!
//! The orchestrator preset (and any other agent) can call `task` to run
//! another preset — `plan`, `code`, `debug`, `ask`, or any file-based agent —
//! in an *isolated context*: the sub-agent gets a fresh child session (its own
//! transcript), runs its own turn loop with the same tool registry and
//! permission engine, and returns only its final text as the tool output. The
//! parent transcript stays clean (no sub-agent tool chatter).
//!
//! Child sessions are registered in the store (with `parent` set), so the GUI
//! can show them and answer any permission prompts they raise. Mutating tool
//! calls made *inside* a sub-turn still go through the permission gate — the
//! `task` call itself is read-only (it only spawns), so delegation is not
//! itself an approval prompt.
//!
//! Recursion is bounded by [`TaskTool::MAX_DEPTH`]: a sub-agent that delegates
//! to a sub-agent is refused past the limit.
//!
//! ## Task lifecycle events
//!
//! When a child is spawned, a `task.started` SSE event is emitted with
//! `{ taskID, description, childSessionID }`. When it finishes (or is aborted),
//! a `task.ended` event is emitted with `{ taskID, status: "completed"|"aborted"|"error", error? }`.
//!
//! ## Abort propagation
//!
//! The parent's abort token cascades downward to all children via tokio's
//! `CancellationToken` tree, so cancelling the parent turn stops every child.
//! When a single child is aborted externally (via
//! `POST /session/{id}/task/{taskID}/abort`), the route handler also cancels
//! the parent's abort token, stopping the entire orchestrator turn. If the
//! child is aborted, the `task` tool returns a descriptive error so the
//! orchestrator knows *which* task was cancelled.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use bebok_tools::{Tool, ToolCtx, ToolOutput};

use super::delegation::{ChildSpec, delegation_depth, prepare_child, run_child};
use super::images::{AgentImageInput, validate_agent_images};
use crate::provider::build_provider;
use crate::store::InstanceStore;

/// Maximum sub-agent nesting depth (a sub-agent delegating further counts as
/// depth 2, etc.). Guards against runaway recursive delegation.
pub const MAX_TASK_DEPTH: usize = 3;

/// The `task` tool. One instance is registered per directory `Instance`.
pub struct TaskTool {
    /// Weak back-reference to the store. Deliberately weak: the store owns the
    /// instance, the instance's tool registry owns this tool, so a strong
    /// reference here would leak the whole graph.
    store: Weak<InstanceStore>,
    max_depth: usize,
}

impl TaskTool {
    pub fn new(store: Weak<InstanceStore>) -> Self {
        Self {
            store,
            max_depth: MAX_TASK_DEPTH,
        }
    }
}

#[derive(Debug, Deserialize)]
struct TaskArgs {
    /// The self-contained instruction for the sub-agent.
    prompt: String,
    /// Agent preset to use (defaults to `code`; unknown names fall back too).
    #[serde(default)]
    agent: Option<String>,
    /// Optional model override for the sub-agent: `"heavy"` lifts this
    /// subtask onto the parent's own model.
    #[serde(default)]
    model: Option<String>,
    /// Short, kebab-case name for this subtask (e.g. `auth-flow-audit`).
    /// Engine guarantees uniqueness within the session; if omitted or taken
    /// the engine assigns `<agent>-<n>`.
    #[serde(default)]
    name: Option<String>,
    /// Optional image attachments for the sub-agent (raw base64, max 5,
    /// png/jpeg/webp/gif, max 5 MB each). Forwards parent images the
    /// sub-agent needs.
    images: Option<Vec<AgentImageInput>>,
    /// WP-DELEGATION: `true` returns immediately with the task id; collect
    /// the result later with `task_wait`. `false` (default) blocks until the
    /// sub-agent finishes and returns its report directly.
    #[serde(default)]
    background: bool,
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Delegate a self-contained subtask to a sub-agent. The sub-agent runs in \
         an isolated context with its own transcript and returns only its final \
         answer. Use for independent subtasks (e.g. research with `ask`, planning \
         with `plan`, or a focused edit with `code`); sequence dependent work \
         yourself. Provide a complete, standalone `prompt` — the sub-agent cannot \
         see this conversation. Optional `images` forwards parent images the \
         sub-agent needs (raw base64, max 5). Set `background: true` to spawn the \
         sub-agent and return at once with its `taskID` (spawn several in one turn \
         for parallel work, then `task_wait` for the results; `task_status` shows \
         live progress, `task_cancel` stops one)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "A complete, standalone instruction for the sub-agent (it cannot see the parent conversation)."
                },
                "agent": {
                    "type": "string",
                    "description": "Agent preset to run (e.g. plan, code, debug, ask). Defaults to code."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override for the sub-agent: \"heavy\" = your own (parent) model for a demanding part (large refactor, architecture, multi-file debugging). Omit for the configured model (`models.<agent>` when set, else the parent's model)."
                },
                "name": {
                    "type": "string",
                    "description": "Short kebab-case name for this subtask (e.g. auth-flow-audit). Engine guarantees uniqueness within the session; if omitted or taken the engine assigns <agent>-<n>."
                },
                "background": {
                    "type": "boolean",
                    "description": "true: return immediately with the taskID and let the sub-agent run in the background (collect with task_wait). false (default): block until it finishes and return its report."
                },
                "images": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "media_type": { "type": "string", "description": "Image MIME type (image/png, image/jpeg, image/webp, image/gif)." },
                            "data": { "type": "string", "description": "Raw base64 payload (data: URL prefix also accepted)." },
                            "name": { "type": "string", "description": "Optional file name." }
                        },
                        "required": ["media_type", "data"]
                    },
                    "description": "Optional image attachments forwarded to the sub-agent (raw base64, max 5, max 5 MB each)."
                }
            },
            "required": ["prompt"]
        })
    }

    /// The `task` tool only spawns a sub-turn; every effect the sub-agent makes
    /// goes through the same permission gate. Marking it read-only means the
    /// delegation itself is not an extra approval prompt.
    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let args: TaskArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutput::new(format!("task: invalid arguments: {e}"), "task"),
        };
        let prompt = args.prompt.trim().to_string();
        if prompt.is_empty() {
            return ToolOutput::new("task: `prompt` must not be empty", "task");
        }
        let image_parts = match validate_agent_images(args.images.unwrap_or_default()) {
            Ok(p) => p,
            Err(e) => return ToolOutput::new(format!("task: {e}"), "task"),
        };

        let Some(store) = self.store.upgrade() else {
            return ToolOutput::new("task: engine store is gone", "task");
        };

        // Nesting guard: depth is derived from the session's parent chain, so
        // siblings running concurrently never count against it.
        let parent_uuid = parse_uuid(&ctx.session_id);
        if delegation_depth(&store, parent_uuid).await >= self.max_depth {
            return ToolOutput::new(
                format!(
                    "task: maximum delegation depth ({}) reached; complete the work directly",
                    self.max_depth
                ),
                "task",
            );
        }

        let directory = ctx.root.to_string_lossy().to_string();
        let instance = match store.get_or_create_instance(&directory).await {
            Ok(i) => i,
            Err(e) => return ToolOutput::new(format!("task: cannot open instance: {e}"), "task"),
        };

        let agent_name = args
            .agent
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .unwrap_or("code")
            .to_string();
        let mut agent = instance.resolve_agent(&agent_name);
        let cfg = instance.config_snapshot();

        // Fresh child session for the sub-agent (isolated transcript).
        let parent = match store.open_session(parent_uuid).await {
            Ok(p) => p,
            Err(_) => {
                return ToolOutput::new("task: parent session not found", "task");
            }
        };

        // The sub-agent model follows the fleet fallback chain: an explicit
        // per-call `model` wins, else `models.<agent>` when set, else the
        // parent's effective model. `model: "heavy"` lifts a heavy part onto
        // the parent's own model. A live child with the same prompt+agent refuses
        // the duplicate (collect it with `task_wait` instead).
        let parent_model = crate::store::parent_model_for_delegation(
            &parent.meta_snapshot().await,
            &instance,
            &cfg,
        );
        if let Some(dup) = parent.find_live_child_by_prompt(&prompt, &agent_name).await {
            return ToolOutput::new(
                format!(
                    "task: a {} child task already covers this (name `{}` taskID `{}` status `{}`); collect it with `task_wait` instead of spawning a duplicate",
                    dup.agent, dup.name, dup.task_id, dup.status,
                ),
                "task",
            );
        }
        let model = crate::agent::resolve_subagent_model(
            &cfg.delegation,
            &parent_model,
            &agent_name,
            |a| cfg.model_for(a),
            args.model.as_deref(),
        );

        // Ground the sub-agent in AGENTS.md + enabled skills (same as the
        // server's prompt assembly, minus the parent-only context notes).
        assemble_prompt(&instance, &mut agent, &cfg);

        let provider = match build_provider(&cfg, &model) {
            Ok(p) => p,
            Err(e) => {
                return ToolOutput::new(
                    format!("task: cannot build provider for '{model}': {e}"),
                    "task",
                );
            }
        };

        // Allocate a unique human-readable name for this subtask.
        let name = parent
            .allocate_child_name(args.name.as_deref(), &agent.name)
            .await;

        let spec = ChildSpec {
            store: store.clone(),
            instance: instance.clone(),
            parent: parent.clone(),
            agent,
            agent_name: agent_name.clone(),
            model: model.clone(),
            provider,
            name: name.clone(),
            prompt,
            images: image_parts,
            parent_abort: ctx.abort.clone(),
            parent_session_id: ctx.session_id.clone(),
            directory,
            max_concurrent: cfg.delegation.effective_max_concurrent(),
            background: args.background,
            origin: "task",
        };

        let prepared = match prepare_child(spec).await {
            Ok(p) => p,
            Err(e) => return ToolOutput::new(format!("task: {e}"), "task"),
        };
        let task_id = prepared.info.task_id.clone();
        let child_session_id = prepared.info.child_session_id.clone();
        let queued = prepared.info.status == "queued";

        if args.background {
            // Detach: the child runs on its own; `task_wait` collects it. The
            // strong store reference keeps the engine graph alive for the
            // child's lifetime.
            let keep_store: Arc<InstanceStore> = store.clone();
            tokio::spawn(async move {
                let _ = run_child(prepared).await;
                drop(keep_store);
            });
            let status = if queued { "queued" } else { "running" };
            let mut out = ToolOutput::new(
                format!(
                    "task [{agent_name}] '{name}' spawned in the background (taskID={task_id}, \
                     status={status}). Call `task_wait` to collect its result; `task_status` \
                     shows progress."
                ),
                format!("task: {name}"),
            );
            out.structured = Some(json!({
                "taskID": task_id,
                "name": name,
                "agent": agent_name,
                "model": model,
                "childSessionID": child_session_id,
                "background": true,
                "status": status,
            }));
            return out;
        }

        let outcome = run_child(prepared).await;

        // If the child was aborted, the abort token was already cancelled.
        // The parent turn loop will pick this up and break out, persisting
        // a "Turn aborted" message. We return an error here so the tool
        // output clearly tells the orchestrator which task was cancelled.
        if outcome.status == "aborted" {
            return ToolOutput::new(
                format!(
                    "task [{agent_name}] (taskID={task_id}): sub-task was cancelled by the user. \
                     The entire orchestrator turn has been aborted — what would you like to do next?"
                ),
                format!("task: {name}"),
            );
        }
        if outcome.status == "error" {
            return ToolOutput::new(
                format!(
                    "task [{agent_name}] (taskID={task_id}): subtask failed: {}",
                    outcome.error.unwrap_or_default()
                ),
                format!("task: {name}"),
            );
        }

        let structured = json!({
            "taskID": task_id,
            "name": name,
            "agent": agent_name,
            "model": model,
            "childSessionID": child_session_id,
            "tokens": { "input": outcome.input_tokens, "output": outcome.output_tokens },
        });
        // The sub-agent's final assistant text is the delegation result.
        if outcome.text.trim().is_empty() {
            let mut out = ToolOutput::new(
                format!("task [{agent_name}] (taskID={task_id}): sub-agent produced no output"),
                format!("task: {name}"),
            );
            out.structured = Some(structured);
            return out;
        }
        let mut out = ToolOutput::new(outcome.text, format!("task: {name}"));
        out.structured = Some(structured);
        out
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

    // Sub-agents need the host-OS/shell note too: without it they emit POSIX
    // pipelines on Windows that `cmd /C` cannot run, so the delegated task
    // fails on Windows but succeeds on Linux.
    agent.prompt = format!("{}\n\n{}", agent.prompt, super::prompt_env::host_os_note());
    // WP-AUTOVERIFY (F8-1): browser + dev-server capabilities and the
    // `verify.frontend` policy (skipped for presets without browser tools).
    if let Some(section) = super::verify_prompt::verification_section(cfg, agent) {
        agent.prompt = format!("{}\n\n{section}", agent.prompt);
    }
    // WP-DELEGATION: a worker's brief, not the main thread's policy.
    agent.prompt = format!(
        "{}\n\n{}",
        agent.prompt,
        super::delegation_policy::subagent_note()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::images::{AgentImageInput, fixtures::PNG_1X1, validate_agent_images};

    #[test]
    fn task_args_deserialize_images() {
        let args: TaskArgs = serde_json::from_value(json!({
            "prompt": "look at this",
            "images": [{"media_type": "image/png", "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=", "name": "a.png"}],
        }))
        .unwrap();
        let imgs = args.images.unwrap();
        assert_eq!(imgs.len(), 1);
        let parts = validate_agent_images(imgs).unwrap();
        assert_eq!(parts.len(), 1);
        match &parts[0] {
            crate::session::Part::Image {
                media_type,
                data,
                name,
            } => {
                assert_eq!(media_type, "image/png");
                assert_eq!(data, PNG_1X1);
                assert_eq!(name.as_deref(), Some("a.png"));
            }
            other => panic!("expected image part, got {other:?}"),
        }
    }

    #[test]
    fn task_images_validated_not_appended_raw() {
        // 6 images -> clear error, no panic.
        let many: Vec<AgentImageInput> = (0..6)
            .map(|_| AgentImageInput {
                media_type: "image/png".into(),
                data: PNG_1X1.into(),
                name: None,
            })
            .collect();
        let err = validate_agent_images(many).unwrap_err();
        assert!(err.contains("max 5 images"), "{err}");
        // bad MIME -> clear error.
        let bad = vec![AgentImageInput {
            media_type: "image/tiff".into(),
            data: PNG_1X1.into(),
            name: None,
        }];
        let err = validate_agent_images(bad).unwrap_err();
        assert!(err.contains("unsupported image media_type"), "{err}");
    }

    #[test]
    fn task_schema_documents_images() {
        let tool = TaskTool {
            store: Weak::new(),
            max_depth: 3,
        };
        let schema = Tool::parameters_schema(&tool);
        assert!(schema["properties"]["images"].is_object());
        assert!(tool.description().contains("images"));
    }
}
