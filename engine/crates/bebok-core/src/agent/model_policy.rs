//! Sub-agent model resolution: which model a delegated child runs on.
//!
//! Precedence: an explicit per-call model wins outright (the `task` `model`
//! argument, or the fleet `member.model`); otherwise `models.<child-agent>`
//! when set, otherwise the parent's effective model. `"heavy"` means the
//! parent's own model.

use crate::config::DelegationConfig;

/// The `task` `model` argument value that means "use the parent's model".
pub const HEAVY: &str = "heavy";

/// The model a sub-agent gets: an explicit per-call request wins outright
/// (the fleet member's configured `model`, or `task.model`); otherwise what
/// the config says for the child agent (`models.<agent>` via
/// `model_for_child`, e.g. `cfg.model_for`); otherwise the parent's model.
/// `child_agent` selects the `models.<agent>` config entry consulted in the
/// middle step. `cfg` is the delegation section (kept so callers pass the
/// cap source explicitly; only used for future knobs — currently unused).
pub fn resolve_subagent_model(
    _cfg: &DelegationConfig,
    parent_model: &str,
    child_agent: &str,
    model_for_child: impl Fn(&str) -> String,
    requested: Option<&str>,
) -> String {
    let requested = requested.map(str::trim).filter(|m| !m.is_empty());
    match requested {
        Some(r) if r.eq_ignore_ascii_case(HEAVY) => return parent_model.to_string(),
        Some(r) => return r.to_string(),
        None => {}
    }
    let configured = model_for_child(child_agent);
    if configured.trim().is_empty() {
        parent_model.to_string()
    } else {
        configured
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inherit(agent: &str) -> String {
        match agent {
            "ask" => "x/cp".to_string(),
            _ => "x/parent".to_string(),
        }
    }

    #[test]
    fn resolve_honours_fallback_chain_and_heavy() {
        let cfg = DelegationConfig::default();
        // `models.<child>` wins over the parent model.
        assert_eq!(
            resolve_subagent_model(&cfg, "x/parent", "ask", inherit, None),
            "x/cp"
        );
        // Empty `models.<child>` falls back to the parent model.
        assert_eq!(
            resolve_subagent_model(&cfg, "x/parent", "code", inherit, None),
            "x/parent"
        );
        // `heavy` -> the parent's model.
        assert_eq!(
            resolve_subagent_model(
                &cfg,
                "anthropic/claude-opus-5",
                "code",
                inherit,
                Some("heavy")
            ),
            "anthropic/claude-opus-5"
        );
        // An explicit per-call request wins outright.
        assert_eq!(
            resolve_subagent_model(
                &cfg,
                "anthropic/claude-opus-5",
                "code",
                inherit,
                Some("openai/o3")
            ),
            "openai/o3"
        );
        // Empty/blank requests are ignored.
        assert_eq!(
            resolve_subagent_model(&cfg, "x/parent", "ask", inherit, Some("  ")),
            "x/cp"
        );
    }
}
