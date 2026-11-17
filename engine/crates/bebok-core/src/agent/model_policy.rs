//! Sub-agent model resolution: which model a delegated child runs on.
//!
//! Precedence: `"heavy"` means the parent's own model; otherwise
//! `models.<child-agent>` when set wins over any per-call request; otherwise
//! the per-call model (`task` `model` / fleet `member.model`) when set;
//! otherwise the parent's effective model.
//!
//! Invariant: the returned model is always one listed in the resolved config
//! (`providers[].models`). A `requested` model not present in config is
//! ignored as if not requested — so the orchestrator can never inject a
//! model that isn't configured. If `xiaomi` is in config, it is allowed.

use crate::config::ResolvedConfig;

/// The `task` `model` argument value that means "use the parent's model".
pub const HEAVY: &str = "heavy";

/// The model a sub-agent gets. `cfg` is the full resolved config — it is
/// used to check `is_model_available` so only models present in
/// `providers[].models` are ever returned.
pub fn resolve_subagent_model(
    cfg: &ResolvedConfig,
    parent_model: &str,
    child_agent: &str,
    model_for_child: impl Fn(&str) -> String,
    requested: Option<&str>,
) -> String {
    let is_available = |m: &str| cfg.is_model_available(m);
    let fallback = || {
        // Prefer parent when available, otherwise the global default.
        if is_available(parent_model) {
            parent_model.trim().to_string()
        } else if !parent_model.trim().is_empty() {
            tracing::warn!(
                "resolve_subagent_model: parent model '{}' not in config; falling back to '{}'",
                parent_model.trim(),
                cfg.model
            );
            cfg.model.clone()
        } else {
            cfg.model.clone()
        }
    };

    let requested = requested.map(str::trim).filter(|m| !m.is_empty());
    if let Some(r) = requested {
        if r.eq_ignore_ascii_case(HEAVY) {
            return fallback();
        }
        // Only models present in config are honoured.
        if !is_available(r) {
            tracing::warn!(
                "resolve_subagent_model: ignoring model '{r}' for sub-agent '{child_agent}': not in providers[].models; using config/parent instead"
            );
        } else {
            let configured = model_for_child(child_agent);
            let configured_trim = configured.trim();
            if !configured_trim.is_empty() {
                if !is_available(configured_trim) {
                    tracing::warn!(
                        "resolve_subagent_model: models.{child_agent}='{}' not in providers[].models; using requested '{r}' instead",
                        configured_trim
                    );
                    return r.to_string();
                }
                if configured_trim != r.trim() {
                    tracing::warn!(
                        "resolve_subagent_model: per-call model '{r}' overridden by models.{child_agent}='{}'",
                        configured_trim
                    );
                }
                return configured_trim.to_string();
            }
            return r.to_string();
        }
    }
    let configured = model_for_child(child_agent);
    let configured_trim = configured.trim();
    if configured_trim.is_empty() {
        return fallback();
    }
    if !is_available(configured_trim) {
        tracing::warn!(
            "resolve_subagent_model: models.{child_agent}='{}' not in providers[].models; falling back to parent/global",
            configured_trim
        );
        return fallback();
    }
    configured_trim.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ResolvedConfig;
    use bebok_llm::ProviderSpec;

    fn test_cfg(
        providers: Vec<ProviderSpec>,
        models: serde_json::Value,
        global_model: &str,
    ) -> ResolvedConfig {
        ResolvedConfig {
            model: global_model.to_string(),
            models,
            providers,
            ..Default::default()
        }
    }

    fn providers_with(models_by_provider: &[(&str, &[&str])]) -> Vec<ProviderSpec> {
        models_by_provider
            .iter()
            .map(|(name, models)| ProviderSpec {
                name: name.to_string(),
                kind: bebok_llm::ProviderKind::Openai,
                models: models.iter().map(|m| m.to_string()).collect(),
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn resolve_honours_fallback_chain_and_heavy() {
        let cfg = test_cfg(
            providers_with(&[
                ("x", &["cp", "parent"]),
                ("openai", &["o3"]),
                ("anthropic", &["claude-opus-5"]),
            ]),
            serde_json::json!({"ask": "x/cp"}),
            "x/parent",
        );
        let inherit = |agent: &str| match agent {
            "ask" => "x/cp".to_string(),
            _ => String::new(),
        };
        // `models.<child>` wins over the parent model.
        assert_eq!(
            resolve_subagent_model(&cfg, "x/parent", "ask", inherit, None),
            "x/cp"
        );
        // Empty `models.<child>` falls back to the parent model.
        assert_eq!(
            resolve_subagent_model(&cfg, "x/parent", "code", |_| String::new(), None),
            "x/parent"
        );
        // `heavy` -> the parent's model.
        assert_eq!(
            resolve_subagent_model(
                &cfg,
                "anthropic/claude-opus-5",
                "code",
                |_| String::new(),
                Some("heavy")
            ),
            "anthropic/claude-opus-5"
        );
        // Explicit per-call request wins only when models.<agent> is empty and available.
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
        // When models.<agent> is set and available, it wins over the per-call request.
        assert_eq!(
            resolve_subagent_model(&cfg, "x/parent", "ask", inherit, Some("openai/o3")),
            "x/cp"
        );
        // Empty/blank requests are ignored.
        assert_eq!(
            resolve_subagent_model(&cfg, "x/parent", "ask", inherit, Some("  ")),
            "x/cp"
        );
    }

    #[test]
    fn unavailable_model_is_ignored() {
        // Only openai/o3 is available; xiaomi not in config.
        let cfg = test_cfg(
            providers_with(&[("openai", &["o3"]), ("x", &["parent"])]),
            serde_json::json!({}),
            "x/parent",
        );
        // Requested model not in config -> fallback to parent.
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
            resolve_subagent_model(
                &cfg,
                "x/parent",
                "code",
                |_| String::new(),
                Some("xiaomi/mimo")
            ),
            "x/parent"
        );
        // Unknown provider -> fallback.
        assert_eq!(
            resolve_subagent_model(
                &cfg,
                "x/parent",
                "code",
                |_| String::new(),
                Some("unknown/model-x")
            ),
            "x/parent"
        );
    }

    #[test]
    fn available_xiaomi_is_allowed() {
        let cfg = test_cfg(
            providers_with(&[("xiaomi", &["mimo-v2.6-flash"]), ("openai", &["o3"])]),
            serde_json::json!({}),
            "openai/o3",
        );
        assert_eq!(
            resolve_subagent_model(
                &cfg,
                "openai/o3",
                "code",
                |_| String::new(),
                Some("xiaomi/mimo-v2.6-flash")
            ),
            "xiaomi/mimo-v2.6-flash"
        );
        // via openrouter prefix when openrouter lists it
        let cfg2 = test_cfg(
            providers_with(&[("openrouter", &["xiaomi/mimo-v2.6-flash"])]),
            serde_json::json!({}),
            "openai/o3",
        );
        assert_eq!(
            resolve_subagent_model(
                &cfg2,
                "openai/o3",
                "code",
                |_| String::new(),
                Some("openrouter/xiaomi/mimo-v2.6-flash")
            ),
            "openrouter/xiaomi/mimo-v2.6-flash"
        );
    }

    #[test]
    fn parent_not_available_falls_back_to_global() {
        let cfg = test_cfg(
            providers_with(&[("openai", &["o3"])]),
            serde_json::json!({}),
            "openai/o3",
        );
        // parent xiaomi not in config -> global model
        assert_eq!(
            resolve_subagent_model(
                &cfg,
                "xiaomi/mimo-v2.6-flash",
                "code",
                |_| String::new(),
                None
            ),
            "openai/o3"
        );
    }
}
