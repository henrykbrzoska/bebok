//! `bebok-jev` permission auto-decider bridge.
//!
//! When the permission gate says "Ask", the gate calls
//! [`query_jev_plugin`] before showing the user popup. This lazily
//! registers the `bebok-jev` dynamic plugin from its on-disk declaration +
//! slot dir (mirrors `ensure_index_plugin` in `code_index_tools.rs`) and
//! invokes its `decide` action with the [`PermissionAskHook`] JSON.
//!
//! Contract: the plugin answers `{"decision": "allow"|"deny"}` when
//! confident, `{"decision": null}` (or any error / timeout / missing
//! binary) when uncertain — every failure mode falls through to the user
//! popup, never blocks.

use crate::plugin::PermissionAskHook;

/// Plugin id (also the slot dir name).
pub const JEV_PLUGIN_NAME: &str = "bebok-jev";

/// Ask the `bebok-jev` plugin for an override decision.
/// Returns `Some("allow"|"deny")` on a confident answer, `None` when the
/// popup should be shown (disabled / undeclared / slot missing / uncertain
/// / error / timeout).
pub async fn query_jev_plugin(
    state: &crate::store::SessionState,
    ask: &PermissionAskHook,
) -> Option<String> {
    use std::path::Path;
    let root = Path::new(state.directory());
    // Fail closed like the rest of the stack: a broken declaration counts
    // as disabled.
    if crate::plugin_decl::is_disabled(root, JEV_PLUGIN_NAME) {
        return None;
    }
    // No declaration at all → nothing to register from.
    if !crate::plugin_decl::decl_path(root, JEV_PLUGIN_NAME).is_file() {
        return None;
    }
    let host = crate::plugin::PluginHost::global();
    if !host.names().await.iter().any(|n| n == JEV_PLUGIN_NAME) {
        let slot_dir = crate::plugin_decl::install_dir(root, JEV_PLUGIN_NAME);
        if !slot_dir.is_dir() {
            return None;
        }
        match crate::plugin_process::load_dynamic_plugin(&slot_dir) {
            Ok(dyn_plugin) => {
                tracing::info!(
                    "lazy-registering plugin '{JEV_PLUGIN_NAME}' for permission gate from {}",
                    slot_dir.display()
                );
                host.register(std::sync::Arc::new(dyn_plugin)).await;
            }
            Err(e) => {
                tracing::warn!(
                    "cannot load plugin '{JEV_PLUGIN_NAME}' from {}: {e}",
                    slot_dir.display()
                );
                return None;
            }
        }
    }
    let input = serde_json::to_value(ask).ok()?;
    let resp = host.invoke(JEV_PLUGIN_NAME, "decide", &input).await?;
    let decision = resp.get("decision")?.as_str()?;
    match decision {
        "allow" | "deny" => Some(decision.to_string()),
        _ => None,
    }
}
