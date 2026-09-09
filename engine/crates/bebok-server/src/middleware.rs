//! Access-log middleware (Interceptor): records every HTTP request +
//! response status in the debug log (skips long-lived streams and the debug
//! endpoints themselves).

use axum::extract::State;
use axum::middleware::Next;
use axum::response::Response;

use crate::state::AppState;

/// Access-log middleware: records every HTTP request + response status in the
/// debug log (skips long-lived streams and the debug endpoints themselves).
pub async fn log_http(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let skip = path == "/event" || path.starts_with("/debug") || path.ends_with("/connect");
    let start = std::time::Instant::now();
    let response = next.run(req).await;
    if !skip {
        let status = response.status();
        state.debug.log(
            "http",
            if status.is_success() { "response" } else { "error" },
            format!("{method} {path}{query}"),
            format!("{} ({}ms)", status.as_u16(), start.elapsed().as_millis()),
        );
    }
    response
}
