//! CORS layer (SPEC §8).
//!
//! The webview runs on `tauri://localhost` / `http://tauri.localhost` in builds
//! and on `http://localhost:4200` in browser dev. Extend via `BEBOK_CORS`
//! (comma separated); the defaults always apply.
//!
//! CORS is NOT an authorisation boundary: it only constrains browsers that
//! choose to respect it, never another local process or `curl`. The actual
//! boundary is the per-launch capability token in [`crate::auth`]; this layer
//! merely has to let the `Authorization` header through (and answer preflight
//! before the token layer sees it, hence it wraps the API router).

use axum::http::{HeaderValue, Method};
use tower_http::cors::CorsLayer;

/// CORS origins allowed to talk to the local engine (SPEC §8).
pub fn cors_layer() -> CorsLayer {
    const DEFAULTS: &[&str] = &[
        "http://localhost:4200",
        "http://127.0.0.1:4200",
        "http://localhost:8787",
        "http://tauri.localhost",
        "https://tauri.localhost",
        "tauri://localhost",
        "capacitor://localhost",
        "http://localhost",
    ];
    let mut origins: Vec<HeaderValue> = Vec::new();
    for o in DEFAULTS {
        if let Ok(v) = HeaderValue::from_str(o) {
            origins.push(v);
        }
    }
    if let Ok(extra) = std::env::var("BEBOK_CORS") {
        for o in extra.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if let Ok(v) = HeaderValue::from_str(o) {
                origins.push(v);
            }
        }
    }
    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
        ])
}

#[cfg(test)]
mod tests {
    //! Regression test for the missing-`PATCH` bug: `PATCH /projects/{id}`
    //! (WP-PROJ-GROUPS) never made it into `allow_methods`, so a browser
    //! preflight rejected the method before the request ever reached the
    //! token layer (`net::ERR_FAILED`, not a 401 — CORS runs first).
    use super::*;
    use crate::routes::build_api_router;
    use crate::state::AppState;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use std::sync::Arc;
    use tower::ServiceExt as _;

    fn test_app() -> Router {
        let dir = std::env::temp_dir().join(format!("bebok-cors-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).ok();
        let state = AppState {
            store: bebok_core::InstanceStore::with_data_dir(dir.join("data")),
            #[cfg(not(target_os = "android"))]
            ptys: Arc::new(bebok_pty::PtyManager::new()),
            debug: Arc::new(bebok_core::DebugLog::new(dir.join("debug.log"))),
            llm_trace: bebok_core::LLM_TRACE.clone(),
            remote_extensions: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        };
        build_api_router().layer(cors_layer()).with_state(state)
    }

    /// Every method a route in `build_api_router` actually uses must survive
    /// a cross-origin preflight, or the browser blocks the real request.
    #[tokio::test]
    async fn preflight_allows_every_method_the_api_uses() {
        for (path, method) in [
            ("/projects/x", "PATCH"),
            ("/projects/x", "DELETE"),
            ("/session", "POST"),
            ("/config", "PUT"),
            ("/fs/file", "PUT"),
        ] {
            let req = Request::builder()
                .method("OPTIONS")
                .uri(path)
                .header(header::ORIGIN, "http://localhost:4200")
                .header("access-control-request-method", method)
                .header(
                    "access-control-request-headers",
                    "authorization,content-type",
                )
                .body(Body::empty())
                .unwrap();
            let res = test_app().oneshot(req).await.unwrap();
            assert_eq!(
                res.status(),
                StatusCode::OK,
                "preflight for {method} {path}"
            );
            let allowed = res
                .headers()
                .get("access-control-allow-methods")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            assert!(
                allowed.contains(method),
                "{method} {path}: allow-methods {allowed:?} does not list {method}"
            );
        }
    }
}
