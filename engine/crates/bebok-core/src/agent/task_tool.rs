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
//! Cancelling a child task (via `POST /session/{id}/task/{taskID}/abort`) also
//! cancels the parent turn — the orchestrator model is asked what to do next.
//! If the child is aborted, the `task` tool returns a descriptive error so the
//! user sees *which* task was cancelled.

use std::sync::Weak;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use bebok_tools::{Tool, ToolCtx, ToolOutput};

use super::images::{AgentImageInput, validate_agent_images};
use super::turn::run_turn;
use crate::provider::build_provider;
use crate::session::Role;
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
    /// Current delegation depth (shared across concurrent sub-turns; a coarse
    /// but safe guard).
    depth: AtomicUsize,
    max_depth: usize,
}

impl TaskTool {
    pub fn new(store: Weak<InstanceStore>) -> Self {
        Self {
            store,
            depth: AtomicUsize::new(0),
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
    /// Optional explicit model override for the sub-agent. Wins over the
    /// preset's own `model` and the per-agent-type default.
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
         sub-agent needs (raw base64, max 5)."
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
                    "description": "Optional model override for the sub-agent (e.g. provider/model-id). Overrides the agent-type default model."
                },
                "name": {
                    "type": "string",
                    "description": "Short kebab-case name for this subtask (e.g. auth-flow-audit). Engine guarantees uniqueness within the session; if omitted or taken the engine assigns <agent>-<n>."
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

        if self.depth.load(Ordering::Acquire) >= self.max_depth {
            return ToolOutput::new(
                format!(
                    "task: maximum delegation depth ({}) reached; complete the work directly",
                    self.max_depth
                ),
                "task",
            );
        }

        let Some(store) = self.store.upgrade() else {
            return ToolOutput::new("task: engine store is gone", "task");
        };
        let _guard = DepthGuard::enter(&self.depth);

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
            .unwrap_or("code");
        let mut agent = instance.resolve_agent(agent_name);
        let cfg = instance.config_snapshot();

        // Effective model: explicit task arg, else preset override, else
        // `models.<agent>` / global model.
        let explicit_model = args
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty());
        let model = explicit_model
            .map(str::to_string)
            .or_else(|| agent.model.clone())
            .unwrap_or_else(|| cfg.model_for(&agent.name));

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

        // Fresh child session for the sub-agent (isolated transcript).
        let parent = match store.open_session(parse_uuid(&ctx.session_id)).await {
            Ok(p) => p,
            Err(_) => {
                return ToolOutput::new("task: parent session not found", "task");
            }
        };

        // Allocate a unique human-readable name for this subtask.
        let name = parent
            .allocate_child_name(args.name.as_deref(), &agent.name)
            .await;

        let child = match store
            .create_subagent_session(&parent, &agent.name, Some(&model), Some(&name))
            .await
        {
            Ok(c) => c,
            Err(e) => {
                return ToolOutput::new(format!("task: cannot create sub-session: {e}"), "task");
            }
        };

        if let Err(e) = child
            .append_user_message_with_images(&prompt, image_parts)
            .await
        {
            return ToolOutput::new(format!("task: cannot record subtask: {e}"), "task");
        }

        // The sub-turn runs under the parent turn's abort tree and claims the
        // child's turn slot so it is cancellable via the session endpoints.
        if !child.try_begin_turn() {
            return ToolOutput::new("task: sub-session is already busy", "task");
        }
        let abort = ctx.abort.child_token();
        child.set_abort(abort.clone()).await;

        // Generate a task ID and register the child in the parent's task map.
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

        // Emit task.started so the client can render the sub-task with an
        // individual abort button.
        bus.publish(
            crate::event::Event::new("task.started", &directory, &ctx.session_id)
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

        // Determine the final status and clean up.
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
        // reads it back once the task is no longer in the live map).
        child.set_task_status(&status, error_msg.as_deref()).await;

        // Unregister and emit task.ended.
        parent.unregister_child_task(&task_id).await;
        bus.publish(
            crate::event::Event::new("task.ended", &directory, &ctx.session_id).with_properties(
                json!({
                    "taskID": task_id,
                    "status": status,
                    "error": error_msg,
                    "childSessionID": child_session_id,
                    "name": name,
                }),
            ),
        );

        // If the child was aborted, the abort token was already cancelled.
        // The parent turn loop will pick this up and break out, persisting
        // a "Turn aborted" message. We return an error here so the tool
        // output clearly tells the orchestrator which task was cancelled.
        if abort.is_cancelled() {
            return ToolOutput::new(
                format!(
                    "task [{agent_name}] (taskID={task_id}): sub-task was cancelled by the user. \
                     The entire orchestrator turn has been aborted — what would you like to do next?"
                ),
                format!("task: {name}"),
            );
        }

        if let Err(e) = result {
            return ToolOutput::new(
                format!("task [{agent_name}] (taskID={task_id}): subtask failed: {e}"),
                format!("task: {name}"),
            );
        }

        // The sub-agent's final assistant text is the delegation result.
        let messages = child.messages_snapshot().await;
        let text = messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .map(|m| m.text_content())
            .unwrap_or_default();
        if text.trim().is_empty() {
            let mut out = ToolOutput::new(
                format!("task [{agent_name}] (taskID={task_id}): sub-agent produced no output"),
                format!("task: {name}"),
            );
            out.structured = Some(json!({
                "taskID": task_id,
                "name": name,
                "agent": agent_name,
                "childSessionID": child_session_id,
            }));
            return out;
        }
        let mut out = ToolOutput::new(text, format!("task: {name}"));
        out.structured = Some(json!({
            "taskID": task_id,
            "name": name,
            "agent": agent_name,
            "childSessionID": child_session_id,
        }));
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
            depth: AtomicUsize::new(0),
            max_depth: 3,
        };
        let schema = Tool::parameters_schema(&tool);
        assert!(schema["properties"]["images"].is_object());
        assert!(tool.description().contains("images"));
    }
}
