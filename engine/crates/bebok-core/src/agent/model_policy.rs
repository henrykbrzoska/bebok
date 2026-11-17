//! Sub-agent model resolution: which model a delegated child runs on.
//!
//! Precedence: `"heavy"` means the parent's own model; otherwise
//! `models.<child-agent>` when set wins over any per-call request; otherwise
//! the per-call model (`task` `model` / fleet `member.model`) when set;
//! otherwise the parent's effective model. Models containing "xiaomi" are
//! never used for sub-agents (treated as if not requested).

use crate::config::DelegationConfig;

/// The `task` `model` argument value that means "use the parent's model".
pub const HEAVY: &str = "heavy";

fn is_xiaomi(model: &str) -> bool {
    model.to_ascii_lowercase().contains("xiaomi")
}

/// The model a sub-agent gets: `"heavy"` lifts to the parent model;
/// otherwise `models.<agent>` (`model_for_child`, e.g. `cfg.model_for`) wins
/// over any per-call `requested` model; otherwise `requested` (fleet
/// member's `model` or `task.model`) when set and not xiaomi; otherwise the
/// parent's model. `child_agent` selects the `models.<agent>` entry.
/// `cfg` is the delegation section (kept so callers pass the cap source
/// explicitly; only used for future knobs — currently unused).
pub fn resolve_subagent_model(
    _cfg: &DelegationConfig,
    parent_model: &str,
    child_agent: &str,
    model_for_child: impl Fn(&str) -> String,
    requested: Option<&str>,
) -> String {
    let requested = requested.map(str::trim).filter(|m| !m.is_empty());
    // "heavy" always wins.
    if let Some(r) = requested {
        if r.eq_ignore_ascii_case(HEAVY) {
            return parent_model.to_string();
        }
        // Xiaomi is never used for sub-agents — drop it as if not requested.
        if is_xiaomi(r) {
            tracing::warn!(
                "resolve_subagent_model: ignoring xiaomi model '{r}' for sub-agent '{child_agent}'; using config/parent instead"
            );
        } else {
            // Config wins over per-call request when set.
            let configured = model_for_child(child_agent);
            if !configured.trim().is_empty() {
                if configured.trim() != r.trim() {
                    tracing::warn!(
                        "resolve_subagent_model: per-call model '{r}' overridden by models.{child_agent}='{}'",
                        configured.trim()
                    );
                }
                return configured.trim().to_string();
            }
            // No config — honour the per-call request.
            return r.to_string();
        }
    }
    let configured = model_for_child(child_agent);
    if configured.trim().is_empty() {
        parent_model.to_string()
    } else {
        configured.trim().to_string()
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
        // Explicit per-call request wins only when models.<agent> is empty.
        assert_eq!(
            resolve_subagent_model(
                &cfg,
                "anthropic/claude-opus-5",
                "code",
                |_| String::new(),
                Some("openai/o3")
            ),
            "openai/o3"
        );
        // When models.<agent> is set, it wins over the per-call request.
        assert_eq!(
            resolve_subagent_model(&cfg, "x/parent", "ask", inherit, Some("openai/o3")),
            "x/cp"
        );
        // Xiaomi is never used for sub-agents — falls back to config/parent.
        assert_eq!(
            resolve_subagent_model(
                &cfg,
                "x/parent",
                "code",
                |_| String::new(),
                Some("openrouter/xiaomi/mimo-v2.6-flash")
            ),
            "x/parent"
        );
        assert_eq!(
            resolve_subagent_model(&cfg, "x/parent", "ask", inherit, Some("xiaomi/mimo")),
            "x/cp"
        );
        // Empty/blank requests are ignored.
        assert_eq!(
            resolve_subagent_model(&cfg, "x/parent", "ask", inherit, Some("  ")),
            "x/cp"
        );
    }
}
