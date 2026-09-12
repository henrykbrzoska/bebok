//! Tool execution: permission-gated execution of one pending call +
//! `fail_tool`.
//!
//! The turn loop collects pending calls, gates each through `gate.rs`
//! (Strategy) and the `before.tool` plugin veto, then delegates here for the
//! mark-running -> execute -> mark-completed/failed -> persist -> emit
//! sequence plus the `after.tool` hook.

use std::sync::Arc;

use bebok_llm::Provider;
use bebok_tools::{ToolCtx, ToolRegistry};
use tokio_util::sync::CancellationToken;

use super::observe::{emit_message, emit_part};
use super::preset::Agent;
use crate::event::{Event, EventBus};
use crate::permission::PermissionEngine;
use crate::plugin::{Hook, PluginHost, ToolCallHook, ToolResultHook};
use crate::store::SessionState;
use crate::util::{compress_tool_output, now_ms, truncate_output};

/// What the permission gate decided for one tool call.
pub enum ToolOutcome {
    /// Execute the tool.
    Run,
    /// Do not execute; fail the tool part with this message.
    Denied(&'static str),
    /// The turn is being aborted; stop processing further calls.
    Aborted,
}

/// Context needed to execute one gated tool call.
pub struct ExecCtx<'a> {
    pub state: &'a Arc<SessionState>,
    pub bus: &'a EventBus,
    pub tools: &'a Arc<ToolRegistry>,
    pub permission: &'a Arc<PermissionEngine>,
    pub provider: &'a Arc<dyn Provider>,
    pub agent: &'a Agent,
    pub abort: &'a CancellationToken,
    pub assistant_idx: usize,
    pub tool_output_cap: usize,
}

/// Execute one permission-gated tool call (the `Run` arm of the turn loop).
/// Returns `false` when the turn loop should stop processing further calls
/// (abort); `true` to continue with the next pending call.
pub async fn exec_gated_call(
    ctx: &ExecCtx<'_>,
    call_id: &str,
    tool_name: &str,
    input: &serde_json::Value,
) -> bool {
    // Gating already happened in `turn.rs` (`resolve_permission`); this entry
    // point handles the plugin veto + execution. The `ToolOutcome` import is
    // kept for the gate contract documentation.
    let _ = ToolOutcome::Run;

    let hooks = PluginHost::global();

    // Plugin hook: a registered plugin may veto an allowed call.
    if hooks.has_plugins().await {
        let mut payload = ToolCallHook {
            tool: tool_name.to_string(),
            input: input.clone(),
            allowed: true,
        };
        hooks.run_hook(Hook::BEFORE_TOOL, &mut payload).await;
        if !payload.allowed {
            fail_tool(
                ctx.state,
                ctx.bus,
                ctx.assistant_idx,
                call_id,
                "denied by plugin",
            )
            .await;
            return true;
        }
    }

    // Allowed: mark running, persist, execute, complete.
    let Some(tool) = ctx.tools.get(tool_name) else {
        // The model called a tool that is not registered: note it in the
        // project config (de-duplicated) so it can be implemented later.
        crate::config::record_unknown_tool(std::path::Path::new(ctx.state.directory()), tool_name);
        fail_tool(
            ctx.state,
            ctx.bus,
            ctx.assistant_idx,
            call_id,
            &format!("tool not found: {tool_name}"),
        )
        .await;
        return true;
    };

    ctx.state
        .update_tool_state(ctx.assistant_idx, call_id, |m, _| {
            m.mark_tool_running(call_id, now_ms())
        })
        .await;
    ctx.state.persist_message_at(ctx.assistant_idx).await;
    emit_part(
        ctx.bus,
        ctx.state,
        "message.part.updated",
        ctx.assistant_idx,
    )
    .await;

    // WP-CHANGES (F6-8): capture the pre-write baseline of the target file
    // the first time a file-mutating tool touches it in this session. Tools
    // have no access to the session directory, so the hook lives here.
    if crate::change_tracking::is_tracked_tool(tool_name)
        && let Some(path) = input.get("path").and_then(|v| v.as_str())
    {
        let tracker =
            crate::change_tracking::Tracker::new(ctx.state.directory(), ctx.state.disk_dir());
        let path = path.to_string();
        let result = tokio::task::spawn_blocking(move || tracker.snapshot_before_write(&path))
            .await
            .unwrap_or_else(|e| Err(format!("snapshot task failed: {e}")));
        if let Err(e) = result {
            tracing::warn!("change tracking: {tool_name}: {e}");
        }
    }

    let tool_ctx = ToolCtx {
        root: ctx.state.directory().into(),
        session_id: ctx.state.id().to_string(),
        abort: ctx.abort.clone(),
    };
    let output = tool.execute(tool_ctx, input.clone()).await;

    // Token-saving pipeline: compress first (strip ANSI, collapse whitespace),
    // then truncate against the configured static cap. The full output is
    // persisted at this size; request-time pruning (prune_for_budget) handles
    // further reduction when the transcript exceeds context_budget.
    let compressed = compress_tool_output(&output.text);
    let text = truncate_output(&compressed, ctx.tool_output_cap);

    let ok = ctx
        .state
        .update_tool_state(ctx.assistant_idx, call_id, |m, name| {
            m.mark_tool_completed(
                call_id,
                text.clone(),
                name.to_string(),
                output.structured.clone(),
            )
        })
        .await;
    if !ok {
        tracing::warn!("tool part {call_id} vanished during execution");
    }

    // Plugin hook: observe the completed tool call (output is the
    // truncated text that is persisted into the transcript).
    if hooks.has_plugins().await {
        let mut payload = ToolResultHook {
            tool: tool_name.to_string(),
            ok,
            output: text.clone(),
        };
        hooks.run_hook(Hook::AFTER_TOOL, &mut payload).await;
    }

    ctx.state.persist_message_at(ctx.assistant_idx).await;
    emit_part(
        ctx.bus,
        ctx.state,
        "message.part.updated",
        ctx.assistant_idx,
    )
    .await;
    emit_message(ctx.bus, ctx.state, "message.updated", ctx.assistant_idx);
    true
}

/// Fail a tool part (mark `ToolState::Error`), persist and emit events.
pub async fn fail_tool(
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

/// Publish a `debug.log` response event (LLM usage line) for one `Done`.
pub fn emit_llm_response(bus: &EventBus, state: &SessionState, model: &str, detail: String) {
    bus.publish(
        Event::new("debug.log", state.directory(), &state.id().to_string()).with_properties(
            serde_json::json!({
                "source": "llm",
                "kind": "response",
                "title": format!("llm {model}"),
                "detail": detail,
            }),
        ),
    );
}
