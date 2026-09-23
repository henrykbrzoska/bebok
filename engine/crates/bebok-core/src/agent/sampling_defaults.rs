//! Hardcoded per-agent sampling defaults (code, not config — like
//! `provider_prompt`). Deterministic agents get low temperature, the
//! orchestrator gets room for creative task splitting. Config
//! (`sampling` / `sampling.<agent>`) and explicit `task`-call overrides
//! merge on top per-field via `Sampling::merge`.

use bebok_llm::Sampling;

/// Default sampling params for an agent type.
pub fn default_sampling(agent_name: &str) -> Sampling {
    match agent_name {
        "code" | "debug" => Sampling {
            temperature: Some(0.2),
            ..Sampling::default()
        },
        "ask" | "plan" => Sampling {
            temperature: Some(0.4),
            ..Sampling::default()
        },
        "orchestrator" => Sampling {
            temperature: Some(0.7),
            ..Sampling::default()
        },
        _ => Sampling::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_agent_defaults() {
        assert_eq!(
            default_sampling("code").temperature,
            Some(0.2),
            "code must be deterministic"
        );
        assert_eq!(
            default_sampling("debug").temperature,
            Some(0.2),
            "debug must be deterministic"
        );
        assert_eq!(default_sampling("ask").temperature, Some(0.4));
        assert_eq!(default_sampling("plan").temperature, Some(0.4));
        assert_eq!(
            default_sampling("orchestrator").temperature,
            Some(0.7),
            "orchestrator needs room for creative splitting"
        );
        assert!(
            default_sampling("unknown-agent").is_empty(),
            "unknown agents get provider defaults"
        );
    }

    #[test]
    fn config_merge_wins_per_field() {
        let base = default_sampling("code");
        let over = Sampling {
            temperature: Some(0.9),
            ..Sampling::default()
        };
        let merged = base.merge(over);
        assert_eq!(merged.temperature, Some(0.9));
    }
}
