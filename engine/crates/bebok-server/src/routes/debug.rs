//! Debug routes: `GET /debug/log` + `DELETE /debug/log`.
//!
//! `GET /debug/log` returns both the structured debug entries and the last 2
//! full LLM request/response payloads (`calls` array). `DELETE` clears both.

use axum::extract::State;
use axum::Json;

use crate::state::AppState;

/// `GET /debug/log` -> the debug log entries + last 2 LLM call traces.
pub async fn debug_log(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "entries": state.debug.entries(),
        "maxChars": bebok_core::debug::DEBUG_LOG_MAX_CHARS,
        "calls": state.llm_trace.list(),
    }))
}

/// `DELETE /debug/log` -> clear the debug log AND the LLM trace.
pub async fn debug_clear(State(state): State<AppState>) -> Json<serde_json::Value> {
    state.debug.clear();
    state.llm_trace.clear();
    Json(serde_json::json!({ "ok": true }))
}
