//! F9-10: `delegation.model_policy` — which model a sub-agent runs on.
//!
//! * `inherit` — the parent's model.
//! * `cheaper` (default) — a lighter sibling of the parent's model from the
//!   same provider, found in the model catalog: same family/version, tools
//!   supported, strictly cheaper (blended input+output list price), and the
//!   *closest* such price (opus → sonnet, not haiku; gpt-5.4 → gpt-5.4-mini,
//!   not nano). When the exact family has none, the same major version is
//!   tried (opus-4-7 → sonnet-4-6). No cheaper sibling → inherit.
//! * an explicit `provider/model` — always that model.
//!
//! The main agent can lift one heavy sub-task back onto its own model by
//! passing `model: "heavy"` in the `task` call (see `delegation_policy`).

use bebok_llm::ModelCatalog;

use crate::config::{DelegationConfig, DelegationModelPolicy};

/// The `task` `model` argument value that means "use the parent's model".
pub const HEAVY: &str = "heavy";

/// Tokens that mark a *tier* (or a naming flavour) rather than a family:
/// stripped when computing the family key so `gpt-5.4` and `gpt-5.4-mini`
/// compare equal. Version tokens (`5.4`, `4-6`, `v4`) are kept.
const TIER_TOKENS: &[&str] = &[
    "mini",
    "nano",
    "lite",
    "flash",
    "pro",
    "max",
    "ultra",
    "turbo",
    "air",
    "preview",
    "exp",
    "latest",
    "luna",
    "terra",
    "sol",
    "astra",
    "opus",
    "sonnet",
    "haiku",
    "fable",
    "codex",
    "spark",
    "chat",
    "reasoner",
    "reasoning",
    "non",
    "thinking",
    "instruct",
];

/// Tiers that are already the light end of a family: such a parent never
/// maps further down (a `mini` child of a `mini` parent buys nothing but
/// risk); the policy falls back to inherit.
const LIGHT_TIERS: &[&str] = &["mini", "nano", "lite", "flash", "haiku", "air", "luna"];

/// Tiers that mark the heavy end: only these get the cross-version
/// fallback (opus-4-7 -> sonnet-4-6); a plain id (`gpt-5.6`, `grok-4.6`)
/// only maps inside its own family.
const HEAVY_TIERS: &[&str] = &["pro", "max", "ultra", "opus", "fable"];

fn has_tier(model_id: &str, tiers: &[&str]) -> bool {
    model_id.split('-').any(|t| tiers.contains(&t))
}

/// Split `provider/model` (lower-cased). A bare model id has no provider.
fn split(model: &str) -> (Option<String>, String) {
    let lower = model.trim().to_ascii_lowercase();
    match lower.split_once('/') {
        Some((p, m)) if !p.is_empty() && !m.is_empty() => (Some(p.to_string()), m.to_string()),
        _ => (None, lower),
    }
}

/// Family key of a model id: every token that is not a tier token, joined
/// with `-` (`gpt-5.4-mini` -> `gpt-5.4`, `claude-opus-4-6` -> `claude-4-6`,
/// `gemini-3.1-pro-preview` -> `gemini-3.1`).
pub fn family_key(model_id: &str) -> String {
    model_id
        .split('-')
        .filter(|t| !t.is_empty() && !TIER_TOKENS.contains(t))
        .collect::<Vec<_>>()
        .join("-")
}

/// The series name (`gpt`, `claude`, `gemini`, `glm`, `o`) and the major
/// version number, when the id carries one (`gpt-5.4` -> ("gpt", 5),
/// `claude-opus-4-6` -> ("claude", 4), `o3-mini` -> ("o", 3),
/// `deepseek-v4-pro` -> ("deepseek", 4)).
pub fn series_and_major(model_id: &str) -> (String, Option<u32>) {
    let mut series = String::new();
    let mut major = None;
    for token in model_id.split('-') {
        let digits: String = token
            .trim_start_matches(|c: char| c.is_ascii_alphabetic())
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !digits.is_empty() && !TIER_TOKENS.contains(&token) {
            if series.is_empty() {
                series = token
                    .chars()
                    .take_while(|c| c.is_ascii_alphabetic())
                    .collect();
            }
            major = digits.parse().ok();
            break;
        }
        if series.is_empty() {
            series = token.to_string();
        }
    }
    (series, major)
}

/// Blended list price used to rank siblings (`None` = unknown, never picked).
fn blended_price(catalog: &ModelCatalog, model: &str) -> Option<f64> {
    catalog
        .pricing(model)
        .map(|p| p.input + p.output)
        .filter(|p| *p > 0.0)
}

/// The cheaper sibling of `model` (`provider/model-id`) per the rules in the
/// module docs, or `None` when there is none (or the model/provider is
/// unknown to the catalog).
pub fn cheaper_sibling(catalog: &ModelCatalog, model: &str) -> Option<String> {
    let (Some(provider), id) = split(model) else {
        return None;
    };
    if has_tier(&id, LIGHT_TIERS) {
        return None;
    }
    let full = format!("{provider}/{id}");
    let parent_price = blended_price(catalog, &full)?;
    let siblings = catalog.provider_models(&provider);
    if siblings.is_empty() {
        return None;
    }
    let family = family_key(&id);
    let (series, major) = series_and_major(&id);

    let candidates = |same_family: bool| -> Option<String> {
        let mut best: Option<(f64, String)> = None;
        for other in &siblings {
            let other_id = other.to_ascii_lowercase();
            if other_id == id {
                continue;
            }
            let matches = if same_family {
                family_key(&other_id) == family
            } else {
                let (s, m) = series_and_major(&other_id);
                s == series && m.is_some() && m == major
            };
            if !matches {
                continue;
            }
            let full_other = format!("{provider}/{other_id}");
            if !catalog.get(&full_other).supports_tools {
                continue;
            }
            let Some(price) = blended_price(catalog, &full_other) else {
                continue;
            };
            if price >= parent_price {
                continue;
            }
            // Closest cheaper price wins; ties go to the lexically later id
            // (newer version / plain name over dated variants).
            let better = match &best {
                None => true,
                Some((p, name)) => price > *p || (price == *p && other_id > *name),
            };
            if better {
                best = Some((price, other_id));
            }
        }
        best.map(|(_, name)| format!("{provider}/{name}"))
    };
    candidates(true).or_else(|| {
        if has_tier(&id, HEAVY_TIERS) {
            candidates(false)
        } else {
            None
        }
    })
}

/// The model a sub-agent gets, given the policy, the parent's model and the
/// `model` argument of the `task` call (`"heavy"` = the parent's model).
pub fn resolve_subagent_model(
    catalog: &ModelCatalog,
    cfg: &DelegationConfig,
    parent_model: &str,
    requested: Option<&str>,
) -> String {
    let requested = requested.map(str::trim).filter(|m| !m.is_empty());
    match requested {
        Some(r) if r.eq_ignore_ascii_case(HEAVY) => return parent_model.to_string(),
        Some(r) => return r.to_string(),
        None => {}
    }
    match cfg.effective_model_policy() {
        DelegationModelPolicy::Explicit(m) => m,
        DelegationModelPolicy::Inherit => parent_model.to_string(),
        DelegationModelPolicy::Cheaper => {
            cheaper_sibling(catalog, parent_model).unwrap_or_else(|| parent_model.to_string())
        }
    }
}

/// One row of `GET /delegation/models`: what `cheaper` maps `model` to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CheaperMapping {
    pub provider: String,
    pub model: String,
    /// `None` = no cheaper sibling (falls back to inherit).
    pub cheaper: Option<String>,
}

/// Mappings for a list of `provider/model` ids (deduplicated, in order).
pub fn mappings_for(catalog: &ModelCatalog, models: &[String]) -> Vec<CheaperMapping> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for model in models {
        let model = model.trim();
        if model.is_empty() || !seen.insert(model.to_ascii_lowercase()) {
            continue;
        }
        let (provider, _) = split(model);
        out.push(CheaperMapping {
            provider: provider.unwrap_or_default(),
            model: model.to_string(),
            cheaper: cheaper_sibling(catalog, model),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat() -> &'static ModelCatalog {
        ModelCatalog::global()
    }

    #[test]
    fn family_keys_strip_tiers_and_keep_versions() {
        assert_eq!(family_key("gpt-5.4-mini"), "gpt-5.4");
        assert_eq!(family_key("gpt-5.4"), "gpt-5.4");
        assert_eq!(family_key("claude-opus-4-6"), "claude-4-6");
        assert_eq!(family_key("claude-sonnet-4-6"), "claude-4-6");
        assert_eq!(family_key("gemini-3.1-pro-preview"), "gemini-3.1");
        assert_eq!(family_key("gemini-3.1-flash-lite"), "gemini-3.1");
        assert_eq!(family_key("glm-5.3-flash"), "glm-5.3");
        assert_eq!(family_key("deepseek-v4-pro"), "deepseek-v4");
        assert_eq!(
            series_and_major("claude-opus-4-7"),
            ("claude".into(), Some(4))
        );
        assert_eq!(series_and_major("gpt-5.6-luna"), ("gpt".into(), Some(5)));
        assert_eq!(series_and_major("o3-mini"), ("o".into(), Some(3)));
        assert_eq!(
            series_and_major("deepseek-v4-pro"),
            ("deepseek".into(), Some(4))
        );
        assert_eq!(series_and_major("grok-4.6"), ("grok".into(), Some(4)));
    }

    #[test]
    fn cheaper_sibling_picks_the_closest_lighter_model_of_the_same_family() {
        let c = cat();
        // opus -> sonnet (not haiku), same version.
        assert_eq!(
            cheaper_sibling(c, "anthropic/claude-opus-5").as_deref(),
            Some("anthropic/claude-sonnet-5")
        );
        assert_eq!(
            cheaper_sibling(c, "anthropic/claude-opus-4-6").as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );
        // gpt-5.4 -> mini (not nano).
        assert_eq!(
            cheaper_sibling(c, "openai/gpt-5.4").as_deref(),
            Some("openai/gpt-5.4-mini")
        );
        // gemini pro -> a flash of the same version.
        let g = cheaper_sibling(c, "google/gemini-3.1-pro-preview").unwrap();
        assert!(g.starts_with("google/gemini-3.1-flash"), "{g}");
        // zai glm-5.3 -> glm-5.3-flash.
        assert_eq!(
            cheaper_sibling(c, "zai/glm-5.3").as_deref(),
            Some("zai/glm-5.3-flash")
        );
        // Case-insensitive provider/model.
        assert_eq!(
            cheaper_sibling(c, "OpenAI/GPT-5.4").as_deref(),
            Some("openai/gpt-5.4-mini")
        );
    }

    #[test]
    fn cheaper_sibling_falls_back_to_same_major_then_to_none() {
        let c = cat();
        // No sonnet-4-7 exists: the same major version is searched.
        let m = cheaper_sibling(c, "anthropic/claude-opus-4-7").unwrap();
        assert!(m.starts_with("anthropic/claude-sonnet-4-"), "{m}");
        // Already a light tier: nothing lighter, even across versions.
        assert_eq!(cheaper_sibling(c, "openai/gpt-5.4-nano"), None);
        assert_eq!(cheaper_sibling(c, "openai/gpt-5.4-mini"), None);
        assert_eq!(cheaper_sibling(c, "openai/gpt-5.6-luna"), None);
        assert_eq!(cheaper_sibling(c, "anthropic/claude-haiku-4-5"), None);
        // A plain id without a same-family sibling does not hop versions.
        assert_eq!(cheaper_sibling(c, "xai/grok-4.6"), None);
        // ...but inside its family it does map (gpt-5.6 -> terra, o3 -> o3-mini).
        assert_eq!(
            cheaper_sibling(c, "openai/gpt-5.6").as_deref(),
            Some("openai/gpt-5.6-terra")
        );
        assert_eq!(
            cheaper_sibling(c, "openai/o3").as_deref(),
            Some("openai/o3-mini")
        );
        assert_eq!(
            cheaper_sibling(c, "deepseek/deepseek-v4-pro").as_deref(),
            Some("deepseek/deepseek-v4-flash")
        );
        // Unknown provider/model or no provider: none.
        assert_eq!(cheaper_sibling(c, "custom/unknown-model"), None);
        assert_eq!(cheaper_sibling(c, "gpt-5.4"), None);
        assert_eq!(cheaper_sibling(c, ""), None);
    }

    #[test]
    fn resolve_honours_policy_heavy_and_explicit_requests() {
        let c = cat();
        let mut cfg = DelegationConfig::default();
        assert_eq!(cfg.effective_model_policy(), DelegationModelPolicy::Cheaper);
        assert_eq!(
            resolve_subagent_model(c, &cfg, "anthropic/claude-opus-5", None),
            "anthropic/claude-sonnet-5"
        );
        // `heavy` -> the parent's model, whatever the policy.
        assert_eq!(
            resolve_subagent_model(c, &cfg, "anthropic/claude-opus-5", Some("heavy")),
            "anthropic/claude-opus-5"
        );
        // An explicit request wins.
        assert_eq!(
            resolve_subagent_model(c, &cfg, "anthropic/claude-opus-5", Some("openai/o3")),
            "openai/o3"
        );
        // No cheaper sibling -> inherit.
        assert_eq!(
            resolve_subagent_model(c, &cfg, "openai/gpt-5.4-nano", None),
            "openai/gpt-5.4-nano"
        );
        cfg.model_policy = DelegationModelPolicy::Inherit;
        assert_eq!(
            resolve_subagent_model(c, &cfg, "anthropic/claude-opus-5", None),
            "anthropic/claude-opus-5"
        );
        cfg.model_policy = DelegationModelPolicy::Explicit("zai/glm-5.3-flash".into());
        assert_eq!(
            resolve_subagent_model(c, &cfg, "anthropic/claude-opus-5", None),
            "zai/glm-5.3-flash"
        );
        // Legacy `model` alone means explicit.
        let legacy = DelegationConfig {
            model: Some("openai/gpt-5.4-mini".into()),
            ..DelegationConfig::default()
        };
        assert_eq!(
            legacy.effective_model_policy(),
            DelegationModelPolicy::Explicit("openai/gpt-5.4-mini".into())
        );
    }

    #[test]
    fn mappings_dedupe_and_report_missing_siblings() {
        let rows = mappings_for(
            cat(),
            &[
                "openai/gpt-5.4".into(),
                "openai/gpt-5.4".into(),
                "openai/gpt-5.4-nano".into(),
                "".into(),
            ],
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].provider, "openai");
        assert_eq!(rows[0].cheaper.as_deref(), Some("openai/gpt-5.4-mini"));
        assert_eq!(rows[1].cheaper, None);
    }

    #[test]
    fn policy_round_trips_through_serde() {
        for (text, policy) in [
            ("\"inherit\"", DelegationModelPolicy::Inherit),
            ("\"cheaper\"", DelegationModelPolicy::Cheaper),
            (
                "\"openai/gpt-5.4-mini\"",
                DelegationModelPolicy::Explicit("openai/gpt-5.4-mini".into()),
            ),
        ] {
            let parsed: DelegationModelPolicy = serde_json::from_str(text).unwrap();
            assert_eq!(parsed, policy);
            assert_eq!(serde_json::to_string(&parsed).unwrap(), text);
        }
        assert_eq!(
            DelegationModelPolicy::parse(" Inherit "),
            DelegationModelPolicy::Inherit
        );
        assert_eq!(
            DelegationModelPolicy::parse(""),
            DelegationModelPolicy::Cheaper
        );
    }
}
