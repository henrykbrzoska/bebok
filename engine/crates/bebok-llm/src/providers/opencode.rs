//! OpenCode Zen gateway (`https://opencode.ai/zen/v1`).
//!
//! Zen speaks the OpenAI Chat Completions protocol, but its free tier is
//! gated on client identity: requests without first-party headers are
//! rejected with `403 FreeTierError: ... can only be used from within
//! OpenCode`. The header shape below was captured from the live official
//! CLI 1.18.34 (`OPENCODE_DEBUG_REQUEST=1 opencode run`, 200 OK on
//! `mimo-v2.6-flash-free`), so every default mirrors the real CLI byte for
//! byte and is overridable through the provider's `extra` map
//! (Settings UI):
//! - full `User-Agent` incl. ai-sdk/runtime suffix, not the bare token
//! - `x-opencode-session` + `x-opencode-session-id` carrying the SAME id,
//!   stable for the whole session (the CLI reuses one `ses_…` id)
//! - `x-opencode-request: msg_…`, unique per request
//! - `x-opencode-project: global`
//! - `stream: true` (bebok always streams)
//!
//! The CLI sends NO `x-session-affinity`, `X-Session-Id`, `HTTP-Referer`,
//! `X-Title` or `X-Source` — those are OpenRouter-isms, never send them.
//! (See `openai_body` in `openai.rs` for the `tools[]` padding half.)
//!
//! No secrets are involved — these are static, non-credential
//! identification headers.
//!
//! NOTE: impersonating another client's identity may violate the
//! gateway operator's terms of service. Use this only with your own API
//! key / workspace and accept that the operator may block it at any time.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use crate::ProviderSpec;
use crate::provider::AdapterConfig;

/// Full `User-Agent` captured from the live CLI 1.18.34 on the Chat
/// Completions endpoint. The gateway keys its first-party detection off
/// the leading token `opencode/<version>`, but the whole string is sent
/// verbatim because that is what the real client emits.
const USER_AGENT: &str = "opencode/1.18.34 ai-sdk/provider-utils/4.0.23 runtime/bun/1.4.2";

/// Default client flavour reported via `x-opencode-client` (the real CLI
/// sends its app name here: `cli` / `desktop` / `web`).
const DEFAULT_CLIENT: &str = "cli";

/// Monotonic counter disambiguating ids minted within the same tick.
static ID_SEQ: AtomicU64 = AtomicU64::new(1);

/// Process-stable session id, minted once. The real CLI keeps ONE `ses_…`
/// id for the whole session (title request and chat request carry the
/// same value); minting a fresh id per request breaks sticky routing.
static SESSION: OnceLock<String> = OnceLock::new();

pub(super) fn config(spec: &ProviderSpec, api_key: Option<String>) -> AdapterConfig {
    let mut config = super::openai_config(spec, api_key);
    // Session identity first: stable for the process, exactly like the
    // real CLI, unless the user pins one via `extra`.
    let session = session_id(spec);
    push(&mut config, "x-opencode-session", session.clone());
    push(&mut config, "x-opencode-session-id", session);
    // Request identity: fresh `msg_…` id per request, like the CLI.
    push(&mut config, "x-opencode-request", fresh_request_id());
    push_default(&mut config, spec, "user_agent", "User-Agent", USER_AGENT);
    push_default(
        &mut config,
        spec,
        "x_opencode_client",
        "x-opencode-client",
        DEFAULT_CLIENT,
    );
    push_default(
        &mut config,
        spec,
        "x_opencode_project",
        "x-opencode-project",
        "global",
    );
    config
}

fn session_id(spec: &ProviderSpec) -> String {
    match spec.extra.get("x_opencode_session_id") {
        Some(Value::String(value)) if !value.trim().is_empty() => value.clone(),
        _ => SESSION.get_or_init(fresh_session_id).clone(),
    }
}

/// A fresh real-looking session id: `ses_` + 12 lowercase hex (time-derived)
/// and 14 alphanumeric chars, matching the CLI's `ses_…` generator shape
/// (`ses_<12 hex><14 alnum>`, 30 chars total).
fn fresh_session_id() -> String {
    let seq = ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .wrapping_add(seq as u128);
    // 12 hex chars from the timestamp, exactly like the CLI's 6 time bytes.
    let hex = format!("{nanos:032x}");
    let time_part = &hex[hex.len() - 12..];
    // 14 base62 chars from a xorshifted counter (no extra deps for one header).
    let mut rng = nanos ^ 0x9e37_79b9_7f4a_7c15 ^ ((seq as u128) << 64 | 0x1234_5678);
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut rand_part = String::with_capacity(14);
    for _ in 0..14 {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rand_part.push(ALPHABET[(rng % 62) as usize] as char);
    }
    format!("ses_{time_part}{rand_part}")
}

/// A fresh real-looking request id: `msg_` + 26 base62 chars, matching the
/// CLI's per-request `x-opencode-request` shape.
fn fresh_request_id() -> String {
    let seq = ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .wrapping_add((seq as u128) << 32);
    let mut rng = nanos ^ 0x51ab_3c59_9e2f_417d;
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut rand_part = String::with_capacity(26);
    for _ in 0..26 {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rand_part.push(ALPHABET[(rng % 62) as usize] as char);
    }
    format!("msg_{rand_part}")
}

/// Push a header unconditionally (caller already resolved the value).
fn push(config: &mut AdapterConfig, header: &str, value: String) {
    config.headers.push((header.to_string(), value));
}

/// Push the user's `extra[key]` override when set, otherwise the
/// first-party default, so `headers` never carries the same header twice
/// (the request builder appends, it does not replace).
fn push_default(
    config: &mut AdapterConfig,
    spec: &ProviderSpec,
    key: &str,
    header: &str,
    default: &str,
) {
    if let Some(Value::String(value)) = spec.extra.get(key)
        && !value.trim().is_empty()
    {
        config.headers.push((header.to_string(), value.clone()));
    } else {
        config
            .headers
            .push((header.to_string(), default.to_string()));
    }
}

/// Zen models that refuse Chat Completions with `400 ModelProtocolUnsupported
/// ("Model does not support this protocol")` and only answer the OpenAI
/// *Responses* protocol (`POST {base}/responses`) — the endpoint the official
/// CLI uses for them.
///
/// Hardcoded from a live probe of the whole configured roster on
/// `POST /zen/v1/chat/completions` (one request per model with the
/// first-party identity headers): every other configured model answered
/// `200`/`4xx upstream` on Chat Completions, only these three free models
/// rejected the protocol itself. The Responses request lives in
/// [`crate::openai_responses`]; everything else keeps the default
/// Chat Completions path unchanged.
const RESPONSES_MODELS: &[&str] = &[
    "longcat-2.5-preview-free",
    "muse-spark-1.2-contributor-free",
    "muse-spark-1.3-contributor-free",
];

/// True when the request model (provider-prefixed, e.g.
/// `opencode/muse-spark-1.3-contributor-free`) must go through the Responses
/// endpoint instead of `/chat/completions`.
///
/// Gated on the `opencode/` request prefix exactly like the free-tier
/// identity handling in `openai_body`, so a same-named model on another
/// provider is never redirected.
pub(crate) fn uses_responses(req_model: &str) -> bool {
    req_model.starts_with("opencode/") && RESPONSES_MODELS.contains(&crate::spec::model_name(req_model))
}

/// Swap the provider's chat URL (`…/chat/completions`, what
/// [`ProviderSpec::chat_url`] yields) for the Responses endpoint.
pub(crate) fn responses_url(endpoint: &str) -> String {
    match endpoint.strip_suffix("/chat/completions") {
        Some(base) => format!("{base}/responses"),
        None => endpoint.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin_provider_specs;

    fn opencode_spec() -> ProviderSpec {
        builtin_provider_specs()
            .into_iter()
            .find(|spec| spec.name == "opencode")
            .unwrap()
    }

    #[test]
    fn opencode_sends_first_party_defaults() {
        let config = super::config(&opencode_spec(), Some("key".into()));
        for (name, value) in [
            (
                "User-Agent",
                "opencode/1.18.34 ai-sdk/provider-utils/4.0.23 runtime/bun/1.4.2",
            ),
            ("x-opencode-client", "cli"),
            ("x-opencode-project", "global"),
        ] {
            assert!(
                config
                    .headers
                    .contains(&(name.to_string(), value.to_string())),
                "missing default header {name}"
            );
        }
        // The session id goes out under two names, both equal, `ses_`-shaped.
        let ids: Vec<&str> = ["x-opencode-session", "x-opencode-session-id"]
            .iter()
            .map(|name| {
                let matches: Vec<_> = config.headers.iter().filter(|(n, _)| n == name).collect();
                assert_eq!(matches.len(), 1, "{name} must appear exactly once");
                matches[0].1.as_str()
            })
            .collect();
        assert!(
            ids.iter().all(|id| *id == ids[0]),
            "session headers must agree: {ids:?}"
        );
        assert!(
            ids[0].starts_with("ses_"),
            "session id must look real: {}",
            ids[0]
        );
        assert_eq!(ids[0].len(), 30, "ses_ + 26 chars: {}", ids[0]);
        // One fresh `msg_…` request id per build.
        let req_ids: Vec<_> = config
            .headers
            .iter()
            .filter(|(n, _)| n == "x-opencode-request")
            .collect();
        assert_eq!(
            req_ids.len(),
            1,
            "x-opencode-request must appear exactly once"
        );
        assert!(
            req_ids[0].1.starts_with("msg_"),
            "request id must look real: {}",
            req_ids[0].1
        );
        assert_eq!(req_ids[0].1.len(), 30, "msg_ + 26 chars: {}", req_ids[0].1);
        // None of the invented / OpenRouter-ism headers may be sent.
        for banned in [
            "x-session-affinity",
            "X-Session-Id",
            "HTTP-Referer",
            "X-Title",
            "X-Source",
        ] {
            assert!(
                config.headers.iter().all(|(n, _)| n != banned),
                "{banned} must be gone"
            );
        }
        // Session is stable across builds, request ids are unique.
        let second = super::config(&opencode_spec(), Some("key".into()));
        let second_session = second
            .headers
            .iter()
            .find(|(n, _)| n == "x-opencode-session")
            .unwrap()
            .1
            .clone();
        assert_eq!(ids[0], second_session, "session id must be stable");
        let second_req = second
            .headers
            .iter()
            .find(|(n, _)| n == "x-opencode-request")
            .unwrap()
            .1
            .clone();
        assert_ne!(
            &req_ids[0].1, &second_req,
            "request ids must be unique per build"
        );
    }

    #[test]
    fn opencode_extra_overrides_the_default_once() {
        let mut spec = opencode_spec();
        spec.extra
            .insert("x_opencode_client".into(), Value::String("desktop".into()));
        let config = super::config(&spec, Some("key".into()));
        let matches: Vec<_> = config
            .headers
            .iter()
            .filter(|(name, _)| name == "x-opencode-client")
            .collect();
        assert_eq!(
            matches,
            [("x-opencode-client".to_string(), "desktop".to_string())]
                .iter()
                .collect::<Vec<_>>(),
            "override must replace the default, not duplicate it"
        );
    }

    #[test]
    fn responses_models_are_hardcoded_to_the_responses_endpoint() {
        for model in [
            "muse-spark-1.2-contributor-free",
            "muse-spark-1.3-contributor-free",
            "longcat-2.5-preview-free",
        ] {
            assert!(
                super::uses_responses(&format!("opencode/{model}")),
                "{model} must use /responses"
            );
            assert!(
                !super::uses_responses(model),
                "bare (non-opencode/) model must not be redirected"
            );
        }
        // Everything else keeps the Chat Completions path — including models
        // that merely share the free-tier suffix or are paid variants.
        for model in [
            "opencode/ling-3.1-flash-free",
            "opencode/mimo-v2.6-flash-free",
            "opencode/big-pickle",
            "opencode/jev-1.13",
            "opencode/gpt-5.5",
        ] {
            assert!(!super::uses_responses(model), "{model} must stay on chat");
        }
        // Same model id on a different provider: never redirected.
        assert!(!super::uses_responses("openrouter/muse-spark-1.3-contributor-free"));
    }

    #[test]
    fn responses_url_swaps_the_chat_suffix() {
        assert_eq!(
            super::responses_url("https://opencode.ai/zen/v1/chat/completions"),
            "https://opencode.ai/zen/v1/responses"
        );
        // Unknown shapes pass through unchanged instead of corrupting the URL.
        assert_eq!(
            super::responses_url("https://example.test/v1/messages"),
            "https://example.test/v1/messages"
        );
    }
}
