//! Debug routes: `GET /debug/log` + `DELETE /debug/log`.

use axum::extract::State;
use axum::Json;

use crate::state::AppState;

/// `GET /debug/log` -> the debug log entries (LLM + HTTP requests/responses).
pub async fn debug_log(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "entries": state.debug.entries(),
        "maxChars": bebok_core::debug::DEBUG_LOG_MAX_CHARS,
    }))
}

/// `DELETE /debug/log` -> clear the debug log.
pub async fn debug_clear(State(state): State<AppState>) -> Json<serde_json::Value> {
    state.debug.clear();
    Json(serde_json::json!({ "ok": true }))
}
