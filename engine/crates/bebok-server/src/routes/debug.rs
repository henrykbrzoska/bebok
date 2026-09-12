//! Debug routes: `GET /debug/log` + `DELETE /debug/log`.
//!
//! Diagnostics only, and hardened accordingly (F0-6):
//! - **Off by default.** Both methods answer `404` unless `BEBOK_DIAGNOSTIC=1`
//!   is set for the engine process, so a normal install exposes nothing.
//! - **Token protected.** Like every other route, they sit behind the F0-5
//!   capability-token layer (`crate::auth`).
//! - **Redacted.** Even when enabled, the returned payload keeps only the shape
//!   of what happened — timestamps, source, kind, HTTP method/path/status and
//!   timing, message roles, part/tool kinds, usage counters — while every
//!   free-form string (system prompts, user messages, tool arguments and
//!   results) is replaced by a `<redacted: N chars>` marker.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use bebok_core::debug::DebugEntry;

use crate::state::AppState;

/// Env flag that turns the diagnostic endpoints on.
const DIAGNOSTIC_ENV: &str = "BEBOK_DIAGNOSTIC";

/// True when the operator explicitly opted into diagnostics for this launch.
fn diagnostic_enabled() -> bool {
    crate::auth::env_flag(DIAGNOSTIC_ENV)
}

/// The response for a disabled endpoint: indistinguishable from an engine
/// build without the route at all.
fn disabled() -> Response {
    (StatusCode::NOT_FOUND, "not found\n").into_response()
}

/// Redact one access-log entry.
///
/// `http` entries are already structural (`GET /session?…` + `200 (3ms)`), but
/// the query string can carry the `?token=` auth fallback and arbitrary
/// user paths, so the title is scrubbed and the detail kept. Everything else
/// (`llm` request/response bodies) is content and is replaced by a marker.
fn redact_entry(entry: DebugEntry) -> serde_json::Value {
    let http = entry.source == "http";
    serde_json::json!({
        "ts": entry.ts,
        "source": entry.source,
        "kind": entry.kind,
        "title": if http {
            crate::auth::scrub_token_query(&entry.title)
        } else {
            entry.title
        },
        "detail": if http {
            entry.detail
        } else {
            format!("<redacted: {} chars>", entry.detail.chars().count())
        },
    })
}

/// `GET /debug/log` -> redacted debug entries + redacted LLM call traces.
/// `404` unless `BEBOK_DIAGNOSTIC=1`.
pub async fn debug_log(State(state): State<AppState>) -> Response {
    if !diagnostic_enabled() {
        return disabled();
    }
    let entries: Vec<serde_json::Value> = state
        .debug
        .entries()
        .into_iter()
        .map(redact_entry)
        .collect();
    Json(serde_json::json!({
        "entries": entries,
        "maxChars": bebok_core::debug::DEBUG_LOG_MAX_CHARS,
        "calls": state.llm_trace.list_redacted(),
    }))
    .into_response()
}

/// `DELETE /debug/log` -> clear the debug log AND the LLM trace.
/// `404` unless `BEBOK_DIAGNOSTIC=1` (same surface as the GET).
pub async fn debug_clear(State(state): State<AppState>) -> Response {
    if !diagnostic_enabled() {
        return disabled();
    }
    state.debug.clear();
    state.llm_trace.clear();
    Json(serde_json::json!({ "ok": true })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_entry_title_is_scrubbed_of_the_token() {
        let entry = DebugEntry {
            ts: 3,
            source: "http".to_string(),
            kind: "response".to_string(),
            title: "GET /fs/tree?directory=C:/p&token=deadbeef".to_string(),
            detail: "200 (1ms)".to_string(),
        };
        let red = redact_entry(entry);
        assert_eq!(red["title"], "GET /fs/tree?directory=C:/p&token=<redacted>");
    }

    #[test]
    fn llm_entry_detail_is_replaced() {
        let entry = DebugEntry {
            ts: 1,
            source: "llm".to_string(),
            kind: "request".to_string(),
            title: "chat".to_string(),
            detail: "my password is hunter2".to_string(),
        };
        let red = redact_entry(entry);
        assert_eq!(red["source"], "llm");
        assert_eq!(red["kind"], "request");
        assert_eq!(red["ts"], 1);
        assert_eq!(red["detail"], "<redacted: 22 chars>");
    }

    #[test]
    fn http_entry_keeps_status_and_timing() {
        let entry = DebugEntry {
            ts: 2,
            source: "http".to_string(),
            kind: "response".to_string(),
            title: "GET /session?directory=C:/p".to_string(),
            detail: "200 (3ms)".to_string(),
        };
        let red = redact_entry(entry);
        assert_eq!(red["title"], "GET /session?directory=C:/p");
        assert_eq!(red["detail"], "200 (3ms)");
    }
}
