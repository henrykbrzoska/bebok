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
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
        ])
}
