//! Permission gate (Strategy): `GateCtx` + `resolve_permission` +
//! `ask_for_permission`.
//!
//! Evaluates one tool call against the permission engine (agent overrides
//! first), the session decision cache, and — for `Ask` — the user decision
//! flow (oneshot, no polling). Fires `permission.asked` /
//! `permission.resolved` / `permission.resolved`-hook events exactly as before.

use crate::event::{Event, EventBus};
use crate::permission::{CachedDecision, DecisionKey, Evaluation, PermissionEngine, Verdict};
use crate::plugin::{Hook, PermissionHook, PluginHost};
use crate::store::SessionState;
use bebok_tools::ToolRegistry;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::exec::ToolOutcome;
use crate::permission::CompiledLayer;

/// Everything the permission gate needs for one tool call.
pub struct GateCtx<'a> {
    pub state: &'a Arc<SessionState>,
    pub bus: &'a EventBus,
    pub permission: &'a PermissionEngine,
    pub agent_layer: Option<&'a CompiledLayer>,
    pub tools: &'a ToolRegistry,
    pub abort: &'a CancellationToken,
    pub agent_name: &'a str,
    pub assistant_idx: usize,
}

/// Evaluate one tool call against the permission engine, falling back to the
/// session decision cache and - for `Ask` - to the user decision flow.
pub async fn resolve_permission(
    ctx: &GateCtx<'_>,
    tool_name: &str,
    input: &serde_json::Value,
) -> ToolOutcome {
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
pub async fn ask_for_permission(
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

/// Fire the `permission.resolved` hook (no-op when no plugin is registered).
pub async fn fire_permission_hook(tool: &str, pattern: &str, decision: &str) {
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
