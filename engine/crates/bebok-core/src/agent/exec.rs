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
            let ok = m.mark_tool_completed(
                call_id,
                text.clone(),
                name.to_string(),
                output.structured.clone(),
            );
            // WP-BROWSER: a tool result may carry an image (e.g. a
            // `browser_screenshot`). It becomes an image part right after the
            // tool part so the request builder can deliver it to the model
            // alongside the tool result, and the client can render it.
            if ok && let Some(image) = &output.image {
                attach_tool_image(m, call_id, name, &image.media_type, &image.data);
            }
            ok
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

/// Insert an image part produced by tool `name` (call `call_id`) directly
/// after its tool part. Replaces a previous image of the same call (a retried
/// completion must not stack duplicates).
pub fn attach_tool_image(
    m: &mut crate::session::Message,
    call_id: &str,
    name: &str,
    media_type: &str,
    data: &str,
) {
    use crate::session::Part;
    let Some(idx) = m.tool_part_index(call_id) else {
        return;
    };
    let image_name = Some(format!("{name}:{call_id}"));
    if let Some(Part::Image { name: existing, .. }) = m.parts.get(idx + 1)
        && *existing == image_name
    {
        m.parts.remove(idx + 1);
    }
    m.parts.insert(
        idx + 1,
        Part::Image {
            media_type: media_type.to_string(),
            data: data.to_string(),
            name: image_name,
        },
    );
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

#[cfg(test)]
mod tests {
    use super::attach_tool_image;
    use crate::session::{Message, Part};

    fn message_with_tool(call_id: &str) -> Message {
        let mut m = Message::assistant_with("code", "m");
        m.add_tool_call(
            call_id.to_string(),
            "browser_screenshot".to_string(),
            serde_json::json!({}),
        );
        assert!(m.mark_tool_completed(
            call_id,
            "shot".to_string(),
            "browser_screenshot".to_string(),
            None
        ));
        m
    }

    /// WP-BROWSER: the image lands directly after its tool part, tagged with
    /// the producing call so the client can pair them.
    #[test]
    fn tool_image_is_inserted_after_the_tool_part() {
        let mut m = message_with_tool("c1");
        m.append_text("done");
        attach_tool_image(&mut m, "c1", "browser_screenshot", "image/png", "aGVsbG8=");
        assert_eq!(m.parts.len(), 3);
        assert!(matches!(&m.parts[0], Part::Tool { id, .. } if id == "c1"));
        match &m.parts[1] {
            Part::Image {
                media_type,
                data,
                name,
            } => {
                assert_eq!(media_type, "image/png");
                assert_eq!(data, "aGVsbG8=");
                assert_eq!(name.as_deref(), Some("browser_screenshot:c1"));
            }
            other => panic!("expected image part, got {other:?}"),
        }
        assert!(matches!(&m.parts[2], Part::Text { text } if text == "done"));
        assert_eq!(m.image_parts().len(), 1);
    }

    #[test]
    fn tool_image_replaces_a_previous_image_of_the_same_call() {
        let mut m = message_with_tool("c1");
        attach_tool_image(&mut m, "c1", "browser_screenshot", "image/png", "AAAA");
        attach_tool_image(&mut m, "c1", "browser_screenshot", "image/jpeg", "BBBB");
        assert_eq!(m.image_parts().len(), 1);
        assert_eq!(m.image_parts()[0].0, "image/jpeg");
    }

    #[test]
    fn tool_image_for_unknown_call_is_ignored() {
        let mut m = message_with_tool("c1");
        attach_tool_image(&mut m, "nope", "browser_screenshot", "image/png", "AAAA");
        assert_eq!(m.parts.len(), 1);
    }
}
