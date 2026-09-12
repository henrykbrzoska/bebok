//! Local capability-token auth (F0-5).
//!
//! The engine binds a loopback port, but loopback is **not** an authorisation
//! boundary: any other process on the machine (and, with a permissive CORS
//! origin, any page in the user's browser) can reach it. This module issues one
//! random capability token per engine launch and rejects every API request that
//! does not present it.
//!
//! Transport of the token, in order of precedence:
//! 1. `Authorization: Bearer <token>` — what the Angular client uses for REST
//!    **and** for the SSE stream (`GET /event` is read with `fetch`, not
//!    `EventSource`, so it can carry headers).
//! 2. `?token=<token>` query parameter — fallback for clients that cannot set
//!    headers (`curl` one-liners, a future `EventSource`/`WebSocket` consumer).
//!
//! The token itself reaches the client through the existing `BEBOK_READY`
//! handshake: the announced URL carries `?token=…`, so both the Tauri sidecar
//! reader and the Android `EngineLauncherPlugin` hand it to the webview without
//! any shell change (see `server.rs` and `client/src/core/transport.strategy.ts`).
//!
//! Exemptions:
//! - `GET /pty/{id}/connect` — a browser cannot set headers on a WebSocket
//!   upgrade; that route is already authenticated by a one-time, scope-bound
//!   PTY ticket issued by the token-protected `POST /pty/{id}/ticket`.
//! - `OPTIONS` — CORS preflight never carries credentials (it is answered by
//!   the CORS layer wrapped around this one; the check here only matters when
//!   the router is used without it, e.g. in tests).

use std::sync::LazyLock;

use axum::extract::Request;
use axum::http::{Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// The per-launch capability token: 256 bits of randomness (two v4 UUIDs),
/// kept in memory only — never written to disk, never reused across launches.
///
/// `BEBOK_TOKEN` may pin the value instead, so a launcher that prefers to hand
/// the token to the engine (rather than read it back from `BEBOK_READY`) can do
/// so. Blank values are ignored.
static TOKEN: LazyLock<String> = LazyLock::new(|| match std::env::var("BEBOK_TOKEN") {
    Ok(pinned) if !pinned.trim().is_empty() => pinned.trim().to_string(),
    _ => format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    ),
});

/// Escape hatch for browser development against a manually started engine
/// (`BEBOK_NO_AUTH=1`). Logged loudly once, because it re-opens B3.
static DISABLED: LazyLock<bool> = LazyLock::new(|| {
    let off = env_flag("BEBOK_NO_AUTH");
    if off {
        tracing::warn!(
            "BEBOK_NO_AUTH=1: the local HTTP API is UNAUTHENTICATED; any process \
             on this machine can read files, config and spawn terminals"
        );
    }
    off
});

/// The capability token for this engine process.
pub fn token() -> &'static str {
    TOKEN.as_str()
}

/// True when token checking is switched off via `BEBOK_NO_AUTH`.
pub fn disabled() -> bool {
    *DISABLED
}

/// `1` / `true` / `yes` (case-insensitive) enable an env flag.
pub fn env_flag(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes"
        }
        Err(_) => false,
    }
}

/// Length-checked, branch-free string compare (no early exit on the first
/// differing byte, so a caller cannot time-probe the token prefix).
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Routes that carry their own authentication and therefore bypass the token.
fn is_exempt(req: &Request) -> bool {
    if req.method() == Method::OPTIONS {
        return true;
    }
    // `GET /pty/{id}/connect`: WebSocket upgrade, authenticated by the one-time
    // ticket in the query string (browsers cannot set WS headers).
    let path = req.uri().path();
    path.starts_with("/pty/") && path.ends_with("/connect")
}

/// Extract the presented token from the `Authorization` header or `?token=`.
fn presented(req: &Request) -> Option<String> {
    if let Some(value) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        let value = value.trim();
        if let Some(rest) = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
        {
            return Some(rest.trim().to_string());
        }
    }
    req.uri().query().and_then(|q| {
        q.split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(k, _)| *k == "token")
            .map(|(_, v)| percent_decode(v))
    })
}

/// Minimal percent-decoding for the `?token=` fallback (the token alphabet is
/// hex, but a pinned `BEBOK_TOKEN` may contain escaped characters).
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(hi), Some(lo)) => {
                        out.push((hi * 16 + lo) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// True when the request may proceed.
pub fn is_authorized(req: &Request) -> bool {
    if disabled() || is_exempt(req) {
        return true;
    }
    presented(req).is_some_and(|t| ct_eq(&t, token()))
}

/// 401 for a request without a usable token. The body is deliberately terse —
/// it must not hint at the expected value.
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer realm=\"bebok\"")],
        "missing or invalid engine token\n",
    )
        .into_response()
}

/// Axum middleware: the single place the capability token is enforced.
/// Applied once around the whole API router (`routes::build_api_router`).
pub async fn require_token(req: Request, next: Next) -> Response {
    if is_authorized(&req) {
        next.run(req).await
    } else {
        unauthorized()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::build_api_router;
    use crate::state::AppState;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use std::sync::Arc;
    use tower::ServiceExt as _;

    /// A router identical to the served one (auth layer included), over a
    /// throwaway state: no `build_app()` side effects (no global debug.log
    /// truncation, no event-forwarder task).
    pub(crate) fn test_app() -> Router {
        let dir = std::env::temp_dir().join(format!("bebok-auth-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).ok();
        let state = AppState {
            store: bebok_core::InstanceStore::new(),
            #[cfg(not(target_os = "android"))]
            ptys: Arc::new(bebok_pty::PtyManager::new()),
            debug: Arc::new(bebok_core::DebugLog::new(dir.join("debug.log"))),
            llm_trace: bebok_core::LLM_TRACE.clone(),
        };
        build_api_router().with_state(state)
    }

    pub(crate) fn temp_project() -> String {
        let dir = std::env::temp_dir().join(format!("bebok-auth-proj-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).ok();
        dir.to_string_lossy().into_owned()
    }

    async fn status(req: HttpRequest<Body>) -> StatusCode {
        test_app().oneshot(req).await.unwrap().status()
    }

    fn get(uri: &str) -> HttpRequest<Body> {
        HttpRequest::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    fn get_auth(uri: &str) -> HttpRequest<Body> {
        HttpRequest::builder()
            .method(Method::GET)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {}", token()))
            .body(Body::empty())
            .unwrap()
    }

    #[test]
    fn token_is_random_and_long() {
        // 2 x v4 UUID in simple form.
        assert_eq!(token().len(), 64);
        assert!(token().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn ct_eq_matches_eq() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "ab"));
        assert!(!ct_eq("", "a"));
        assert!(ct_eq("", ""));
    }

    #[tokio::test]
    async fn session_requires_token() {
        let dir = temp_project();
        assert_eq!(
            status(get("/session")).await,
            StatusCode::UNAUTHORIZED,
            "GET /session without a token"
        );
        assert_eq!(
            status(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/session")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        r#"{{"directory":"{}"}}"#,
                        dir.escape_debug()
                    )))
                    .unwrap()
            )
            .await,
            StatusCode::UNAUTHORIZED,
            "POST /session without a token"
        );
        assert_eq!(
            status(get("/session/0000/message")).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn session_ok_with_token() {
        let dir = temp_project();
        let uri = format!("/session?directory={}", urlencode(&dir));
        assert_eq!(status(get_auth(&uri)).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn fs_requires_token() {
        let dir = temp_project();
        let uri = format!("/fs/tree?directory={}", urlencode(&dir));
        assert_eq!(status(get(&uri)).await, StatusCode::UNAUTHORIZED);
        assert_eq!(status(get_auth(&uri)).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn config_requires_token() {
        assert_eq!(status(get("/config")).await, StatusCode::UNAUTHORIZED);
    }

    #[cfg(not(target_os = "android"))]
    #[tokio::test]
    async fn pty_requires_token() {
        // Listing terminals: 401 without, 200 with.
        assert_eq!(status(get("/pty")).await, StatusCode::UNAUTHORIZED);
        assert_eq!(status(get_auth("/pty")).await, StatusCode::OK);
        // Spawning a terminal (full local code execution) must be rejected.
        assert_eq!(
            status(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/pty")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap()
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        // Ticket issuing is token-protected too.
        assert_eq!(
            status(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/pty/abc/ticket")
                    .body(Body::empty())
                    .unwrap()
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
    }

    /// The WebSocket upgrade cannot carry an `Authorization` header, so it is
    /// exempt from the token layer and authenticated by the one-time PTY ticket
    /// instead. The exemption is path+suffix bound: nothing else under `/pty`
    /// gets it.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn pty_connect_is_the_only_exempt_route() {
        let connect = HttpRequest::builder()
            .uri("/pty/abc/connect?ticket=nope")
            .body(Body::empty())
            .unwrap();
        assert!(is_authorized(&connect), "WS upgrade must skip the token");

        for uri in [
            "/pty",
            "/pty/abc/ticket",
            "/pty/abc",
            "/connect",
            "/fs/file",
        ] {
            let req = HttpRequest::builder().uri(uri).body(Body::empty()).unwrap();
            assert!(!is_authorized(&req), "{uri} must still require the token");
        }
    }

    /// End to end over the router: the exempt WebSocket route is *not*
    /// answered with 401 — it reaches the handler/extractor, which rejects an
    /// unusable upgrade (426) or an unknown ticket (403) on its own.
    #[cfg(not(target_os = "android"))]
    #[tokio::test]
    async fn pty_connect_is_not_blocked_by_the_token_layer() {
        let req = HttpRequest::builder()
            .method(Method::GET)
            .uri("/pty/abc/connect?ticket=nope")
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .header(header::SEC_WEBSOCKET_VERSION, "13")
            .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
            .body(Body::empty())
            .unwrap();
        let got = status(req).await;
        assert_ne!(got, StatusCode::UNAUTHORIZED);
        assert!(
            got == StatusCode::FORBIDDEN || got == StatusCode::UPGRADE_REQUIRED,
            "unexpected status for an unticketed WS upgrade: {got}"
        );
    }

    /// SSE is consumed with `fetch`, so it carries the header like any REST
    /// call: 401 without, 200 (stream open) with.
    #[tokio::test]
    async fn sse_event_stream_requires_token() {
        assert_eq!(status(get("/event")).await, StatusCode::UNAUTHORIZED);
        let res = test_app().oneshot(get_auth("/event")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
    }

    /// Header-less clients may pass `?token=`; a wrong value still fails.
    #[tokio::test]
    async fn query_token_is_accepted_and_verified() {
        assert_eq!(
            status(get(&format!("/pty?token={}", token()))).await,
            StatusCode::OK
        );
        assert_eq!(
            status(get("/pty?token=deadbeef")).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn wrong_scheme_or_value_is_rejected() {
        let req = HttpRequest::builder()
            .uri("/pty")
            .header(header::AUTHORIZATION, format!("Basic {}", token()))
            .body(Body::empty())
            .unwrap();
        assert_eq!(status(req).await, StatusCode::UNAUTHORIZED);

        let req = HttpRequest::builder()
            .uri("/pty")
            .header(header::AUTHORIZATION, "Bearer 00000000")
            .body(Body::empty())
            .unwrap();
        assert_eq!(status(req).await, StatusCode::UNAUTHORIZED);
    }

    /// Percent-encoded `?token=` values decode before comparison.
    #[test]
    fn percent_decode_basics() {
        assert_eq!(percent_decode("abc"), "abc");
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("a%zz"), "a%zz");
    }

    /// Minimal query escaping for test URIs (paths contain `\` and `:`).
    pub(crate) fn urlencode(raw: &str) -> String {
        raw.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                other => format!("%{other:02X}"),
            })
            .collect()
    }
}
