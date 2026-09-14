//! Fleet generation: use a cheap LLM to plan a set of sub-agents for a
//! directory, choosing models from the configured provider pool.
//!
//! The public entry point is [`generate_fleet`]. All types are designed so the
//! server handler is a thin JSON-extract → call → JSON-response shim.

use std::collections::{HashMap, HashSet};

use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::config::FleetMember;
use crate::config::ResolvedConfig;
use crate::error::{CoreError, Result};

// ── Public API ──────────────────────────────────────────────────────────────

/// Options for fleet generation.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetGenOptions {
    /// Minimum members per agent type (clamped to 1..=10, default 3).
    #[serde(default = "default_min_per_type")]
    pub min_per_type: usize,
    /// Agent types to include (default: code, ask, plan, debug).
    /// Must be subsets of {code, ask, plan, debug}; "orchestrator" is rejected.
    #[serde(default = "default_types")]
    pub types: Vec<String>,
}

fn default_min_per_type() -> usize {
    3
}

fn default_types() -> Vec<String> {
    vec!["code".into(), "ask".into(), "plan".into(), "debug".into()]
}

/// Result of fleet generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetGenResult {
    pub members: Vec<FleetMember>,
    pub generation_model: String,
    pub fallback: bool,
    pub warning: Option<String>,
}

// ── Candidate pool ──────────────────────────────────────────────────────────

/// A candidate model in the pool, with pricing for sorting.
#[derive(Debug, Clone)]
struct Candidate {
    /// Full "provider/model" id (lowercase); the provider is the part before
    /// the first `/`, so a separate provider field is not needed.
    model: String,
    /// Blended price (input + output) per 1M tokens. `f64::MAX` when unknown.
    blended: f64,
}

/// All valid agent types we accept.
const VALID_TYPES: &[&str] = &["code", "ask", "plan", "debug"];

/// Agent-type descriptions for the LLM prompt.
fn agent_description(ty: &str) -> &'static str {
    match ty {
        "code" => "code implementation and editing",
        "ask" => "research and read-only analysis",
        "plan" => "design and architecture planning",
        "debug" => "diagnosis and debugging",
        _ => "general assistance",
    }
}

/// Build the candidate pool from the resolved config.
fn build_candidate_pool(cfg: &ResolvedConfig) -> Vec<Candidate> {
    let catalog = bebok_llm::ModelCatalog::global();
    let providers = cfg.resolved_providers();

    let mut seen: HashSet<String> = HashSet::new();
    let mut candidates: Vec<Candidate> = Vec::new();

    for spec in &providers {
        if !spec.is_configured() {
            continue;
        }
        let provider_name = spec.name.to_ascii_lowercase();

        // Union: spec.models (explicit) + catalog.provider_models(name)
        let mut model_ids: Vec<String> = Vec::new();
        for m in &spec.models {
            let full = if m.contains('/') {
                m.to_ascii_lowercase()
            } else {
                format!("{provider_name}/{}", m.to_ascii_lowercase())
            };
            if !seen.contains(&full) {
                model_ids.push(full.clone());
                seen.insert(full);
            }
        }
        for m in catalog.provider_models(&provider_name) {
            let full = format!("{provider_name}/{}", m.to_ascii_lowercase());
            if !seen.contains(&full) {
                model_ids.push(full.clone());
                seen.insert(full);
            }
        }

        for full in model_ids {
            let caps = catalog.get(&full);
            if !caps.supports_tools {
                continue;
            }
            let blended = catalog
                .pricing(&full)
                .map(|p| p.input + p.output)
                .unwrap_or(f64::MAX);
            candidates.push(Candidate {
                model: full,
                blended,
            });
        }
    }

    candidates.sort_by(|a, b| {
        a.blended
            .partial_cmp(&b.blended)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates
}

// ── Fleet generation ────────────────────────────────────────────────────────

/// Generate a fleet of sub-agents for the given config and options.
pub async fn generate_fleet(cfg: &ResolvedConfig, opts: FleetGenOptions) -> Result<FleetGenResult> {
    // Validate types.
    let mut types: Vec<String> = Vec::new();
    for t in &opts.types {
        let tl = t.to_ascii_lowercase();
        if !VALID_TYPES.contains(&tl.as_str()) {
            return Err(CoreError::BadRequest(format!(
                "invalid agent type '{t}': allowed types are code, ask, plan, debug"
            )));
        }
        if !types.contains(&tl) {
            types.push(tl);
        }
    }
    if types.is_empty() {
        types = default_types();
    }

    // Clamp min_per_type.
    let min_per_type = opts.min_per_type.clamp(1, 10);

    // Build pool.
    let candidates = build_candidate_pool(cfg);

    // Find cheapest candidate with supports_tools for generation.
    let generation_model = candidates.first().map(|c| c.model.clone()).ok_or_else(|| {
        CoreError::BadRequest("no configured providers with usable models".into())
    })?;

    // Try LLM generation.
    match try_llm_generate(cfg, &generation_model, &candidates, &types, min_per_type).await {
        Ok(result) => Ok(result),
        Err(reason) => Ok(deterministic_fallback(
            &candidates,
            &types,
            min_per_type,
            &generation_model,
            &reason.to_string(),
        )),
    }
}

// ── LLM generation attempt ──────────────────────────────────────────────────

async fn try_llm_generate(
    cfg: &ResolvedConfig,
    generation_model: &str,
    candidates: &[Candidate],
    types: &[String],
    min_per_type: usize,
) -> Result<FleetGenResult> {
    let provider = crate::provider::build_provider(cfg, generation_model)?;

    let system = "You are a fleet planner for an AI agent system. You MUST respond \
        with ONLY a JSON object — no markdown fences, no explanation, no text before \
        or after. The JSON must match exactly: \
        {\"members\": [{\"name\": \"<string>\", \"agent\": \"<string>\", \"model\": \"<string>\"}]}";

    let user = build_user_prompt(candidates, types, min_per_type);

    let request = bebok_llm::ChatRequest {
        model: generation_model.to_string(),
        system: system.to_string(),
        messages: vec![bebok_llm::ChatMessage::user(user)],
        tools: vec![],
        max_tokens: 2048,
        thinking: bebok_llm::Thinking::Off,
    };

    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        provider.stream(request),
    )
    .await
    .map_err(|_| "LLM generation timed out after 120s".to_string())
    .map_err(CoreError::BadRequest)?
    .map_err(|e| CoreError::BadRequest(format!("LLM stream error: {e}")))?;

    // Collect text from the stream.
    let mut text = String::new();
    tokio::pin!(stream);
    while let Some(ev) = stream.next().await {
        match ev {
            Ok(bebok_llm::StreamEvent::Text(t)) => text.push_str(&t),
            Ok(bebok_llm::StreamEvent::Done(_)) => break,
            Ok(_) => {} // Thinking, ToolCall — ignore
            Err(e) => return Err(CoreError::BadRequest(format!("LLM stream error: {e}"))),
        }
    }

    if text.trim().is_empty() {
        return Err("LLM returned empty response".to_string())
            .map_err(|e: String| CoreError::BadRequest(e));
    }

    // Parse the response.
    let raw = parse_llm_json(&text)?;
    let parsed: ParsedFleet = serde_json::from_str(&raw)
        .map_err(|e| CoreError::BadRequest(format!("invalid fleet JSON from LLM: {e}")))?;

    // Validate and repair.
    let (members, warning) = validate_and_repair(parsed.members, candidates, types, min_per_type);

    Ok(FleetGenResult {
        members,
        generation_model: generation_model.to_string(),
        fallback: false,
        warning,
    })
}

// ── Prompt building ─────────────────────────────────────────────────────────

fn build_user_prompt(candidates: &[Candidate], types: &[String], min_per_type: usize) -> String {
    let total_needed = types.len() * min_per_type;

    // Mark the top tercile as EXPENSIVE.
    let expensive_threshold = if candidates.is_empty() {
        0.0
    } else {
        let len = candidates.len();
        let tercyl_start = len * 2 / 3;
        candidates
            .get(tercyl_start)
            .map(|c| c.blended)
            .unwrap_or(f64::MAX)
    };

    let mut prompt = String::new();

    prompt.push_str(&format!(
        "Generate a fleet of {total_needed} sub-agents ({min_per_type} per type).\n\n"
    ));

    prompt.push_str("Agent types:\n");
    for t in types {
        prompt.push_str(&format!("- {}: {}\n", t, agent_description(t)));
    }

    prompt.push_str("\nAvailable models (sorted cheapest first):\n");
    for c in candidates {
        let tag = if c.blended >= expensive_threshold && c.blended != f64::MAX {
            " [EXPENSIVE]"
        } else if c.blended == f64::MAX {
            " [no pricing]"
        } else {
            ""
        };
        prompt.push_str(&format!("- {} (${:.2}/1M){}\n", c.model, c.blended, tag));
    }

    prompt.push_str(&format!(
        "\nRules:\n\
        - Minimum {min_per_type} members per type.\n\
        - Within each type, use DIFFERENT models.\n\
        - Prefer cheapest models. Avoid EXPENSIVE when possible.\n\
        - Names must be unique, format: \"<type>-<provider>-<shortmodel>\"\n\
        - Model must be one from the list above (full \"provider/model\" form).\n\
        - Respond with ONLY the JSON object, no other text.\n"
    ));

    prompt
}

// ── JSON parsing ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ParsedFleet {
    members: Vec<ParsedMember>,
}

#[derive(Deserialize)]
struct ParsedMember {
    name: String,
    agent: String,
    model: String,
}

/// Strip markdown fences and find the first `{` .. last `}` to extract the JSON object.
fn parse_llm_json(text: &str) -> Result<String> {
    // Strip ```json ... ``` fences.
    let stripped = strip_code_fences(text);
    let trimmed = stripped.trim();

    // Find first '{' and last '}'.
    let start = trimmed
        .find('{')
        .ok_or_else(|| CoreError::BadRequest("LLM response contains no JSON object".to_string()))?;
    let end = trimmed.rfind('}').ok_or_else(|| {
        CoreError::BadRequest("LLM response contains no closing brace".to_string())
    })?;

    Ok(trimmed[start..=end].to_string())
}

/// Extract the JSON payload from a fenced code block, if present.
/// ` ```json {...} ``` ` -> the inner `{...}`; text without fences is
/// returned unchanged (the caller then locates the first `{` .. last `}`).
fn strip_code_fences(text: &str) -> String {
    for open in ["```json", "```"] {
        if let Some(start) = text.find(open) {
            let after_open = start + open.len();
            if let Some(end) = text[after_open..].find("```") {
                return text[after_open..after_open + end].to_string();
            }
        }
    }
    text.to_string()
}

// ── Validation & repair ─────────────────────────────────────────────────────

/// Validate parsed members and repair issues. Returns (members, optional warning).
fn validate_and_repair(
    parsed: Vec<ParsedMember>,
    candidates: &[Candidate],
    types: &[String],
    min_per_type: usize,
) -> (Vec<FleetMember>, Option<String>) {
    let valid_type_set: HashSet<&str> = types.iter().map(|s| s.as_str()).collect();
    let candidate_models: HashSet<&str> = candidates.iter().map(|c| c.model.as_str()).collect();
    let mut warnings: Vec<String> = Vec::new();
    let mut members: Vec<FleetMember> = Vec::new();
    let mut used_names: HashSet<String> = HashSet::new();
    let mut used_models_per_type: HashMap<String, HashSet<String>> = HashMap::new();

    // First pass: validate and keep good entries.
    for (idx, pm) in parsed.into_iter().enumerate() {
        // Agent type must be valid.
        let agent = pm.agent.to_ascii_lowercase();
        if !valid_type_set.contains(agent.as_str()) {
            continue; // skip invalid agent type
        }

        // Model must be in the pool.
        let model_str = match normalize_model(&pm.model, candidates) {
            Some(m) => m,
            None => continue, // skip unknown model
        };

        if !candidate_models.contains(model_str.as_str()) {
            continue;
        }

        // Name must be non-empty and unique.
        let mut name = pm.name.trim().to_string();
        if name.is_empty() {
            name = format!("{agent}-{}", used_names.len() + idx + 1);
        }
        // Deduplicate names.
        let original_name = name.clone();
        let mut suffix = 2u32;
        while used_names.contains(&name) {
            name = format!("{original_name}-{suffix}");
            suffix += 1;
        }
        used_names.insert(name.clone());

        // Within a type, prefer distinct models: if this model is already
        // used for this type, swap to the cheapest unused model from the pool.
        let models_this_type = used_models_per_type.entry(agent.clone()).or_default();
        if models_this_type.contains(&model_str) {
            if let Some(swap) = candidates
                .iter()
                .map(|c| c.model.clone())
                .find(|m| !models_this_type.contains(m.as_str()))
            {
                warnings.push(format!(
                    "model {model_str} reused in type {agent}; swapped to {swap}"
                ));
                models_this_type.insert(swap.clone());
                members.push(FleetMember {
                    name,
                    agent,
                    model: swap,
                });
                continue;
            }
            warnings.push(format!(
                "only {} distinct models available; type {agent} reuses {model_str}",
                candidates.len()
            ));
        }
        models_this_type.insert(model_str.clone());

        members.push(FleetMember {
            name,
            agent,
            model: model_str,
        });
    }

    // Second pass: fill shortfalls per type with cheapest available models.
    let all_models: Vec<&Candidate> = candidates.iter().collect(); // already sorted cheap-first.

    for ty in types {
        let current_count = members.iter().filter(|m| m.agent == *ty).count();
        if current_count >= min_per_type {
            continue;
        }

        let used_this_type = used_models_per_type.entry(ty.clone()).or_default();
        let mut round_robin_idx = 0;

        for _ in current_count..min_per_type {
            // Find next unused model (or allow reuse with warning).
            let chosen = loop {
                if round_robin_idx < all_models.len() {
                    let c = all_models[round_robin_idx];
                    round_robin_idx += 1;
                    if !used_this_type.contains(&c.model) {
                        break c.model.clone();
                    }
                } else {
                    // All models used in this type — allow reuse.
                    let idx = (round_robin_idx - all_models.len()) % all_models.len();
                    warnings.push(format!(
                        "only {} distinct cheap models available; some members share a model",
                        all_models.len()
                    ));
                    break all_models[idx].model.clone();
                }
            };

            used_this_type.insert(chosen.clone());

            // Generate unique name.
            let name = loop {
                let candidate_name = format!("{}-{}-{}", ty, "gen", members.len() + 1);
                if !used_names.contains(&candidate_name) {
                    used_names.insert(candidate_name.clone());
                    break candidate_name;
                }
            };

            members.push(FleetMember {
                name,
                agent: ty.clone(),
                model: chosen,
            });
        }
    }

    // Sort by agent type then name.
    members.sort_by(|a, b| a.agent.cmp(&b.agent).then_with(|| a.name.cmp(&b.name)));

    let warning = if warnings.is_empty() {
        None
    } else {
        Some(warnings.join("; "))
    };

    (members, warning)
}

/// Try to normalize a model string to "provider/model" form, resolving bare model IDs.
fn normalize_model(raw: &str, candidates: &[Candidate]) -> Option<String> {
    let lower = raw.to_ascii_lowercase().trim().to_string();

    // Already in provider/model form?
    if lower.contains('/') && candidates.iter().any(|c| c.model == lower) {
        return Some(lower);
    }

    // Bare model id — check if unique in the pool.
    let matches: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| {
            c.model.ends_with(&format!("/{lower}"))
                || c.model.rsplit('/').next().unwrap_or(&c.model) == lower
        })
        .collect();

    if matches.len() == 1 {
        return Some(matches[0].model.clone());
    }

    None
}

// ── Deterministic fallback ──────────────────────────────────────────────────

/// Build a deterministic fleet when LLM generation fails.
fn deterministic_fallback(
    candidates: &[Candidate],
    types: &[String],
    min_per_type: usize,
    generation_model: &str,
    reason: &str,
) -> FleetGenResult {
    let mut members: Vec<FleetMember> = Vec::new();
    let mut used_names: HashSet<String> = HashSet::new();

    for ty in types {
        // Round-robin over the pool STARTING AT this type's offset, so each
        // type begins on a different cheap model (cheapest for code, second
        // cheapest for ask, ...). Cycles when min_per_type exceeds the pool.
        let type_idx = types.iter().position(|t| t == ty).unwrap_or(0);
        let mut round_robin_idx = type_idx % candidates.len().max(1);
        let mut type_count = 0;

        while type_count < min_per_type {
            let model = candidates
                .get(round_robin_idx % candidates.len().max(1))
                .map(|c| c.model.clone())
                .unwrap_or_else(|| "unknown/unknown".to_string());
            round_robin_idx += 1;

            let name = loop {
                let candidate_name = format!("{}-{}", ty, type_count + 1);
                if !used_names.contains(&candidate_name) {
                    used_names.insert(candidate_name.clone());
                    break candidate_name;
                }
            };

            members.push(FleetMember {
                name,
                agent: ty.clone(),
                model,
            });
            type_count += 1;
        }
    }

    FleetGenResult {
        members,
        generation_model: generation_model.to_string(),
        fallback: true,
        warning: Some(format!(
            "LLM generation failed ({reason}); using deterministic fallback"
        )),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a fake ResolvedConfig with given provider specs.
    fn fake_config(providers: Vec<bebok_llm::ProviderSpec>) -> ResolvedConfig {
        ResolvedConfig {
            providers,
            ..Default::default()
        }
    }

    /// Helper: a ProviderSpec with known models and a fake key.
    fn fake_spec(
        name: &str,
        kind: bebok_llm::ProviderKind,
        models: Vec<&str>,
    ) -> bebok_llm::ProviderSpec {
        bebok_llm::ProviderSpec {
            name: name.to_string(),
            kind,
            endpoint: Some(format!("https://{name}.example.com/v1")),
            api_key: Some("test-key".into()),
            models: models.into_iter().map(String::from).collect(),
            extra: Default::default(),
        }
    }

    // ── Pool building tests ──────────────────────────────────────────────

    #[test]
    fn pool_includes_configured_providers_only() {
        let cfg = fake_config(vec![
            fake_spec("openai", bebok_llm::ProviderKind::Openai, vec!["gpt-4.1"]),
            // No key for anthropic → not configured.
            bebok_llm::ProviderSpec {
                name: "anthropic".into(),
                kind: bebok_llm::ProviderKind::Anthropic,
                endpoint: None,
                api_key: None, // no env var set either
                models: vec!["claude-sonnet-4-5".into()],
                extra: Default::default(),
            },
        ]);
        let pool = build_candidate_pool(&cfg);
        assert!(
            pool.iter().any(|c| c.model.starts_with("openai/")),
            "openai should be in pool"
        );
    }

    #[test]
    fn pool_sorts_cheap_first() {
        let catalog = bebok_llm::ModelCatalog::global();
        // Find a cheap and an expensive model from the catalog.
        let mut cheap = None;
        let mut expensive = None;
        for provider in &["deepseek", "openai"] {
            for m in catalog.provider_models(provider) {
                let full = format!("{provider}/{m}");
                if let Some(p) = catalog.pricing(&full) {
                    let blended = p.input + p.output;
                    if cheap.is_none() && blended < 5.0 && catalog.get(&full).supports_tools {
                        cheap = Some((full, blended));
                    } else if blended > 20.0 && catalog.get(&full).supports_tools {
                        expensive = Some((full, blended));
                    }
                }
            }
        }
        if let (Some((cheap_model, cheap_price)), Some((exp_model, exp_price))) = (cheap, expensive)
        {
            let cfg = fake_config(vec![
                fake_spec(
                    cheap_model.split('/').next().unwrap(),
                    bebok_llm::ProviderKind::Openai,
                    vec![cheap_model.split('/').nth(1).unwrap()],
                ),
                fake_spec(
                    exp_model.split('/').next().unwrap(),
                    bebok_llm::ProviderKind::Openai,
                    vec![exp_model.split('/').nth(1).unwrap()],
                ),
            ]);
            let pool = build_candidate_pool(&cfg);
            if pool.len() >= 2 {
                let cheap_idx = pool.iter().position(|c| c.model == cheap_model);
                let exp_idx = pool.iter().position(|c| c.model == exp_model);
                if let (Some(ci), Some(ei)) = (cheap_idx, exp_idx) {
                    assert!(
                        ci < ei,
                        "cheap ({cheap_model} ${cheap_price}) should be before expensive ({exp_model} ${exp_price})"
                    );
                }
            }
        }
    }

    // ── JSON parsing tests ───────────────────────────────────────────────

    #[test]
    fn parse_plain_json() {
        let text = r#"{"members":[{"name":"code-a","agent":"code","model":"openai/gpt-4.1"}]}"#;
        let result = parse_llm_json(text).unwrap();
        assert!(result.starts_with('{'));
        assert!(result.ends_with('}'));
    }

    #[test]
    fn parse_json_with_fences() {
        let text = r#"```json
{"members":[{"name":"ask-b","agent":"ask","model":"deepseek/deepseek-chat"}]}
```"#;
        let result = parse_llm_json(text).unwrap();
        let parsed: ParsedFleet = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed.members.len(), 1);
        assert_eq!(parsed.members[0].agent, "ask");
    }

    #[test]
    fn parse_json_with_surrounding_text() {
        let text = r#"Here is the fleet plan:
```json
{"members":[{"name":"plan-1","agent":"plan","model":"openai/gpt-4.1"}]}
```
Hope this helps!"#;
        let result = parse_llm_json(text).unwrap();
        let parsed: ParsedFleet = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed.members[0].agent, "plan");
    }

    #[test]
    fn parse_json_with_extra_text_before_after() {
        let text = "Sure! {\"members\":[{\"name\":\"x\",\"agent\":\"code\",\"model\":\"openai/gpt-4.1\"}]} done";
        let result = parse_llm_json(text).unwrap();
        let parsed: ParsedFleet = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed.members.len(), 1);
    }

    #[test]
    fn parse_no_json_returns_error() {
        let result = parse_llm_json("I don't have a JSON for you");
        assert!(result.is_err());
    }

    // ── Validation & repair tests ────────────────────────────────────────

    fn make_candidates() -> Vec<Candidate> {
        vec![
            Candidate {
                model: "deepseek/deepseek-chat".into(),
                blended: 0.5,
            },
            Candidate {
                model: "deepseek/deepseek-reasoner".into(),
                blended: 1.0,
            },
            Candidate {
                model: "openai/gpt-4.1".into(),
                blended: 10.0,
            },
            Candidate {
                model: "openai/gpt-4.1-mini".into(),
                blended: 2.0,
            },
        ]
    }

    #[test]
    fn validation_rejects_invalid_agent_type() {
        let parsed = vec![ParsedMember {
            name: "orch-1".into(),
            agent: "orchestrator".into(),
            model: "openai/gpt-4.1".into(),
        }];
        let types = vec!["code".into(), "ask".into(), "plan".into(), "debug".into()];
        let (members, _) = validate_and_repair(parsed, &make_candidates(), &types, 3);
        assert!(
            members.iter().all(|m| m.agent != "orchestrator"),
            "orchestrator should be rejected"
        );
    }

    #[test]
    fn validation_rejects_unknown_model() {
        let parsed = vec![ParsedMember {
            name: "code-1".into(),
            agent: "code".into(),
            model: "nonexistent/fake-model".into(),
        }];
        // Single type in scope: unknown model rejected, backfill gives 3 code.
        let types = vec!["code".into()];
        let (members, _) = validate_and_repair(parsed, &make_candidates(), &types, 3);
        assert_eq!(members.len(), 3, "backfill should produce 3 code members");
        assert!(
            members.iter().all(|m| m.model != "nonexistent/fake-model"),
            "unknown model must not survive"
        );
    }

    #[test]
    fn validation_deduplicates_names() {
        let parsed = vec![
            ParsedMember {
                name: "code-x".into(),
                agent: "code".into(),
                model: "deepseek/deepseek-chat".into(),
            },
            ParsedMember {
                name: "code-x".into(),
                agent: "code".into(),
                model: "openai/gpt-4.1".into(),
            },
            ParsedMember {
                name: "code-x".into(),
                agent: "code".into(),
                model: "openai/gpt-4.1-mini".into(),
            },
            ParsedMember {
                name: "code-x".into(),
                agent: "code".into(),
                model: "deepseek/deepseek-reasoner".into(),
            },
        ];
        let types = vec!["code".into()];
        let (members, _) = validate_and_repair(parsed, &make_candidates(), &types, 3);
        let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
        let unique: HashSet<&str> = names.iter().copied().collect();
        assert_eq!(
            names.len(),
            unique.len(),
            "all names must be unique: {names:?}"
        );
    }

    #[test]
    fn validation_deduplicates_models_per_type() {
        let parsed = vec![
            ParsedMember {
                name: "code-1".into(),
                agent: "code".into(),
                model: "deepseek/deepseek-chat".into(),
            },
            ParsedMember {
                name: "code-2".into(),
                agent: "code".into(),
                model: "deepseek/deepseek-chat".into(),
            },
            ParsedMember {
                name: "code-3".into(),
                agent: "code".into(),
                model: "deepseek/deepseek-chat".into(),
            },
        ];
        let types = vec!["code".into()];
        let (members, _) = validate_and_repair(parsed, &make_candidates(), &types, 3);
        let models: Vec<&str> = members.iter().map(|m| m.model.as_str()).collect();
        // At least one should be swapped to a different model.
        let unique_models: HashSet<&str> = models.iter().copied().collect();
        assert!(
            unique_models.len() > 1,
            "duplicate models per type should be swapped: {models:?}"
        );
    }

    #[test]
    fn validation_fills_shortfall() {
        let parsed = vec![ParsedMember {
            name: "code-1".into(),
            agent: "code".into(),
            model: "deepseek/deepseek-chat".into(),
        }];
        let types = vec!["code".into()];
        let (members, _) = validate_and_repair(parsed, &make_candidates(), &types, 3);
        assert_eq!(members.len(), 3, "should fill to minPerType");
        assert!(members.iter().all(|m| m.agent == "code"));
    }

    #[test]
    fn validation_fills_all_types() {
        let parsed = vec![];
        let types = vec!["code".into(), "ask".into(), "plan".into(), "debug".into()];
        let (members, _) = validate_and_repair(parsed, &make_candidates(), &types, 3);
        assert_eq!(members.len(), 12, "4 types * 3 each = 12");
        for ty in &types {
            let count = members.iter().filter(|m| m.agent == *ty).count();
            assert!(count >= 3, "type {ty} should have at least 3, got {count}");
        }
    }

    // ── Deterministic fallback tests ─────────────────────────────────────

    #[test]
    fn fallback_meets_min_per_type() {
        let candidates = make_candidates();
        let types = vec!["code".into(), "ask".into(), "plan".into(), "debug".into()];
        let result = deterministic_fallback(&candidates, &types, 3, "openai/gpt-4.1", "test error");
        assert!(result.fallback);
        assert!(result.warning.is_some());
        assert_eq!(result.members.len(), 12, "4 types * 3 = 12");
        for ty in &types {
            let count = result.members.iter().filter(|m| m.agent == *ty).count();
            assert!(count >= 3, "type {ty}: expected >= 3, got {count}");
        }
    }

    #[test]
    fn fallback_uses_cheapest_models() {
        let candidates = make_candidates();
        let types = vec!["code".into()];
        let result = deterministic_fallback(&candidates, &types, 3, "test-model", "timeout");
        // Round-robin from the cheapest (candidates are pre-sorted cheap-first).
        assert_eq!(result.members[0].model, "deepseek/deepseek-chat");
        for m in &result.members {
            assert!(
                candidates.iter().any(|c| c.model == m.model),
                "fallback model must come from the pool"
            );
        }
    }

    #[test]
    fn fallback_names_are_unique() {
        let candidates = make_candidates();
        let types = vec!["code".into(), "ask".into()];
        let result = deterministic_fallback(&candidates, &types, 3, "m", "reason");
        let names: HashSet<&str> = result.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names.len(), result.members.len(), "names must be unique");
    }

    // ── FleetGenOptions defaults ─────────────────────────────────────────

    #[test]
    fn options_default_values() {
        let opts: FleetGenOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(opts.min_per_type, 3);
        assert_eq!(opts.types, vec!["code", "ask", "plan", "debug"]);
    }

    #[test]
    fn options_custom_values() {
        let opts: FleetGenOptions =
            serde_json::from_str(r#"{"minPerType":5,"types":["code","debug"]}"#).unwrap();
        assert_eq!(opts.min_per_type, 5);
        assert_eq!(opts.types, vec!["code", "debug"]);
    }

    #[test]
    fn orchestrator_type_rejected() {
        let cfg = fake_config(vec![fake_spec(
            "openai",
            bebok_llm::ProviderKind::Openai,
            vec!["gpt-4.1"],
        )]);
        let opts = FleetGenOptions {
            min_per_type: 2,
            types: vec!["code".into(), "orchestrator".into()],
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(generate_fleet(&cfg, opts));
        assert!(result.is_err(), "orchestrator type should be rejected");
    }

    // ── normalize_model ──────────────────────────────────────────────────

    #[test]
    fn normalize_bare_model_unique() {
        let candidates = vec![
            Candidate {
                model: "openai/gpt-4.1".into(),
                blended: 1.0,
            },
            Candidate {
                model: "deepseek/deepseek-chat".into(),
                blended: 0.5,
            },
        ];
        assert_eq!(
            normalize_model("gpt-4.1", &candidates),
            Some("openai/gpt-4.1".into())
        );
    }

    #[test]
    fn normalize_full_model() {
        let candidates = vec![Candidate {
            model: "openai/gpt-4.1".into(),
            blended: 1.0,
        }];
        assert_eq!(
            normalize_model("openai/gpt-4.1", &candidates),
            Some("openai/gpt-4.1".into())
        );
    }

    #[test]
    fn normalize_ambiguous_bare_model() {
        let candidates = vec![
            Candidate {
                model: "openai/chat".into(),
                blended: 1.0,
            },
            Candidate {
                model: "deepseek/chat".into(),
                blended: 0.5,
            },
        ];
        assert_eq!(normalize_model("chat", &candidates), None);
    }

    // ── strip_code_fences ────────────────────────────────────────────────

    #[test]
    fn fences_stripped_correctly() {
        let input = "```json\n{\"a\":1}\n```";
        let result = strip_code_fences(input);
        assert!(!result.contains("```"));
        assert!(result.contains("{\"a\":1}"));
    }

    #[test]
    fn no_fences_unchanged() {
        let input = "{\"a\":1}";
        assert_eq!(strip_code_fences(input), input);
    }
}
