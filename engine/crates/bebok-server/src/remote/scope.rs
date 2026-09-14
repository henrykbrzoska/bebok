//! Route allowlist for the `remote` scope + per-device rate limiting
//! (WP-M1, F10-2).
//!
//! A device token is a *capability*, not a login: it may only reach the
//! routes a phone needs to follow and steer a session. Everything else —
//! files, config, terminals, browser actions, process kills, reverts,
//! project/MCP/tool-safety writes, diagnostics — answers
//! `403 {"error":"remote_scope"}` regardless of the handler.
//!
//! The table below is **exhaustive**: every `(method, path)` registered in
//! `routes::build_api_router` must appear with an explicit decision, and a
//! unit test parses `routes/mod.rs` to fail the build the moment a route is
//! added without one.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::http::Method;

/// Who may call a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Local scope only (the desktop shell with the launch token).
    Local,
    /// Also reachable with a `remote`-scoped device token.
    Remote,
    /// Unauthenticated by design (own auth or none) — never remote-scoped.
    Open,
}

/// The exhaustive decision table (method, route pattern, access).
pub const TABLE: &[(&str, &str, Access)] = &[
    // -- sessions -----------------------------------------------------------
    ("POST", "/session", Access::Remote),
    ("GET", "/session", Access::Remote),
    ("GET", "/session/{id}", Access::Remote),
    ("DELETE", "/session/{id}", Access::Local),
    ("GET", "/session/{id}/message", Access::Remote),
    ("GET", "/permission", Access::Remote),
    ("POST", "/session/{id}/prompt", Access::Remote),
    ("POST", "/session/{id}/abort", Access::Remote),
    ("POST", "/session/{id}/task/{taskID}/abort", Access::Remote),
    ("GET", "/session/{id}/agents", Access::Remote),
    ("GET", "/delegation/models", Access::Local),
    ("POST", "/fleet/generate", Access::Local),
    ("GET", "/session/{id}/export", Access::Local),
    ("POST", "/session/{id}/compact", Access::Local),
    ("GET", "/session/{id}/changes", Access::Remote),
    ("GET", "/session/{id}/changes/diff", Access::Remote),
    ("POST", "/session/{id}/changes/revert", Access::Local),
    ("GET", "/session/{id}/processes", Access::Remote),
    ("GET", "/processes/{id}/log", Access::Remote),
    ("POST", "/processes/{id}/kill", Access::Local),
    ("POST", "/session/{id}/truncate", Access::Local),
    (
        "POST",
        "/session/{id}/permission/{requestID}",
        Access::Remote,
    ),
    // -- browser viewer -----------------------------------------------------
    ("GET", "/session/{id}/browser", Access::Remote),
    ("GET", "/session/{id}/browser/frame", Access::Remote),
    ("POST", "/session/{id}/browser/{action}", Access::Local),
    // -- catalogues / config ------------------------------------------------
    ("GET", "/agent", Access::Remote),
    ("GET", "/mcp", Access::Local),
    ("POST", "/mcp/{name}/toggle", Access::Local),
    ("GET", "/config", Access::Local),
    ("PUT", "/config", Access::Local),
    ("GET", "/docker", Access::Local),
    ("GET", "/models", Access::Remote),
    ("GET", "/providers/catalog", Access::Local),
    // -- filesystem ---------------------------------------------------------
    ("GET", "/fs/browse", Access::Local),
    ("GET", "/fs/tree", Access::Local),
    ("GET", "/fs/file", Access::Local),
    ("PUT", "/fs/file", Access::Local),
    // -- projects -----------------------------------------------------------
    ("GET", "/projects", Access::Remote),
    ("POST", "/projects", Access::Local),
    ("PATCH", "/projects/{id}", Access::Local),
    ("DELETE", "/projects/{id}", Access::Local),
    ("POST", "/projects/{id}/open", Access::Local),
    ("GET", "/projects/{id}/git", Access::Local),
    ("POST", "/projects/{id}/git/worktree/remove", Access::Local),
    ("GET", "/version", Access::Remote),
    ("GET", "/plugins", Access::Local),
    ("GET", "/tools/safety", Access::Local),
    ("PUT", "/tools/safety", Access::Local),
    ("GET", "/stats", Access::Remote),
    ("GET", "/event", Access::Remote),
    ("GET", "/debug/log", Access::Local),
    ("DELETE", "/debug/log", Access::Local),
    // -- terminal (absent on Android) ---------------------------------------
    ("POST", "/pty", Access::Local),
    ("GET", "/pty", Access::Local),
    ("POST", "/pty/{id}/ticket", Access::Local),
    ("GET", "/pty/{id}/connect", Access::Open),
    // -- remote module (WP-M1) ----------------------------------------------
    ("POST", "/remote/enable", Access::Local),
    ("POST", "/remote/disable", Access::Local),
    ("POST", "/remote/relay", Access::Local),
    ("POST", "/remote/pair/start", Access::Local),
    ("POST", "/remote/pair", Access::Open),
    ("POST", "/remote/pair/confirm/{pairId}", Access::Local),
    ("POST", "/remote/pair/reject/{pairId}", Access::Local),
    ("GET", "/remote/devices", Access::Local),
    ("DELETE", "/remote/devices/{id}", Access::Local),
    ("GET", "/remote/status", Access::Remote),
    ("POST", "/remote/heartbeat", Access::Remote),
];

/// Segment-wise match: `{x}` matches exactly one non-empty segment.
pub fn pattern_matches(pattern: &str, path: &str) -> bool {
    let mut p = pattern.trim_end_matches('/').split('/');
    let mut s = path.trim_end_matches('/').split('/');
    loop {
        match (p.next(), s.next()) {
            (None, None) => return true,
            (Some(pp), Some(ss)) => {
                let wildcard = pp.starts_with('{') && pp.ends_with('}');
                if wildcard {
                    if ss.is_empty() {
                        return false;
                    }
                } else if pp != ss {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// Look up the decision for a concrete request. `None` = unknown route
/// (falls through to the router's own 404/405, but is *denied* for the
/// remote scope by [`remote_allowed`]).
pub fn access_for(method: &Method, path: &str) -> Option<Access> {
    let m = method.as_str();
    TABLE
        .iter()
        .find(|(tm, pattern, _)| *tm == m && pattern_matches(pattern, path))
        .map(|(_, _, access)| *access)
}

/// True when a `remote`-scoped device may call `method path`. Unknown
/// routes are denied (deny by default).
pub fn remote_allowed(method: &Method, path: &str) -> bool {
    matches!(access_for(method, path), Some(Access::Remote))
}

// -- rate limiting ---------------------------------------------------------

/// Sustained requests per second per device.
pub const RATE_PER_SEC: f64 = 10.0;
/// Burst allowance: an app start fires ~10 catalogue/list calls at once,
/// so the bucket holds two seconds' worth before the sustained rate bites.
pub const RATE_BURST: f64 = 20.0;
/// Concurrent `GET /event` streams per device.
pub const MAX_SSE_PER_DEVICE: usize = 2;

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

/// Token bucket per device id: `RATE_BURST` tokens, refilled at
/// `RATE_PER_SEC`. Idle buckets are pruned when the map grows.
#[derive(Debug, Default)]
pub struct RateLimiter {
    buckets: HashMap<String, Bucket>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume one token for `id` at `now`; `false` = over the limit.
    pub fn check_at(&mut self, id: &str, now: Instant) -> bool {
        if self.buckets.len() > 64 {
            let stale = Duration::from_secs(60);
            self.buckets
                .retain(|_, b| now.saturating_duration_since(b.last) < stale);
        }
        let bucket = self.buckets.entry(id.to_string()).or_insert(Bucket {
            tokens: RATE_BURST,
            last: now,
        });
        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * RATE_PER_SEC).min(RATE_BURST);
        bucket.last = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    pub fn check(&mut self, id: &str) -> bool {
        self.check_at(id, Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_matching_is_segment_wise() {
        assert!(pattern_matches("/session", "/session"));
        assert!(pattern_matches("/session", "/session/"));
        assert!(pattern_matches("/session/{id}", "/session/abc"));
        assert!(!pattern_matches("/session/{id}", "/session"));
        assert!(!pattern_matches("/session/{id}", "/session/abc/message"));
        assert!(pattern_matches(
            "/session/{id}/permission/{requestID}",
            "/session/s1/permission/r1"
        ));
        assert!(!pattern_matches("/session/{id}", "/session//"));
        assert!(!pattern_matches("/fs/file", "/fs/files"));
    }

    #[test]
    fn remote_allowlist_decisions() {
        assert!(remote_allowed(&Method::POST, "/session/abc/prompt"));
        assert!(remote_allowed(&Method::GET, "/event"));
        assert!(remote_allowed(&Method::GET, "/session"));
        assert!(remote_allowed(&Method::POST, "/remote/heartbeat"));
        assert!(remote_allowed(&Method::POST, "/session/abc/permission/r-1"));
        assert!(!remote_allowed(&Method::PUT, "/fs/file"));
        assert!(!remote_allowed(&Method::GET, "/fs/file"));
        assert!(!remote_allowed(&Method::GET, "/config"));
        assert!(!remote_allowed(&Method::PUT, "/config"));
        assert!(!remote_allowed(&Method::POST, "/pty"));
        assert!(!remote_allowed(&Method::GET, "/pty/x/connect"));
        assert!(!remote_allowed(&Method::POST, "/session/abc/browser/click"));
        assert!(!remote_allowed(&Method::POST, "/processes/p/kill"));
        assert!(!remote_allowed(&Method::DELETE, "/session/abc"));
        assert!(!remote_allowed(&Method::POST, "/remote/pair/start"));
        assert!(!remote_allowed(&Method::GET, "/remote/devices"));
        assert!(!remote_allowed(&Method::POST, "/remote/pair"));
        // Unknown routes: deny by default.
        assert!(!remote_allowed(&Method::GET, "/nope"));
        assert!(!remote_allowed(&Method::GET, "/debug/log"));
    }

    /// Extract every `(METHOD, path)` registered through `.route("…", …)` in
    /// a router source file. Handles multi-line chains like
    /// `post(a).get(b)` and ignores comments.
    fn routes_in_source(src: &str) -> Vec<(String, String)> {
        let src: String = src
            .lines()
            .map(|l| match l.find("//") {
                Some(i) if !l[..i].contains('"') => &l[..i],
                _ => l,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut out = Vec::new();
        let mut rest = src.as_str();
        while let Some(start) = rest.find(".route(") {
            let body = &rest[start + ".route(".len()..];
            let mut depth = 1usize;
            let mut end = 0usize;
            for (i, ch) in body.char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let call = &body[..end];
            let q1 = call.find('"').expect("route path literal");
            let q2 = call[q1 + 1..].find('"').expect("route path literal end") + q1 + 1;
            let path = call[q1 + 1..q2].to_string();
            let chain = &call[q2 + 1..];
            for method in ["get", "post", "put", "patch", "delete"] {
                let needle = format!("{method}(");
                let mut from = 0usize;
                while let Some(i) = chain[from..].find(&needle) {
                    let at = from + i;
                    let boundary = at == 0
                        || !chain[..at]
                            .chars()
                            .next_back()
                            .is_some_and(|c| c.is_alphanumeric() || c == '_');
                    if boundary {
                        out.push((method.to_ascii_uppercase(), path.clone()));
                    }
                    from = at + needle.len();
                }
            }
            rest = &body[end..];
        }
        out
    }

    /// Every route registered in `routes/mod.rs` has an explicit decision,
    /// and every decision points at a real route. Adding a route without
    /// classifying it — or leaving a stale entry — fails here.
    #[test]
    fn every_api_route_has_an_explicit_scope_decision() {
        let registered = routes_in_source(include_str!("../routes/mod.rs"));
        assert!(
            registered.len() > 40,
            "route parser broke: only {} routes found",
            registered.len()
        );
        let mut missing = Vec::new();
        for (method, path) in &registered {
            let listed = TABLE.iter().any(|(m, p, _)| m == method && p == path);
            if !listed {
                missing.push(format!("{method} {path}"));
            }
        }
        assert!(
            missing.is_empty(),
            "routes without a remote-scope decision in remote/scope.rs TABLE: {missing:?}"
        );
        let mut stale = Vec::new();
        for (method, path, _) in TABLE {
            if !registered.iter().any(|(m, p)| m == method && p == path) {
                stale.push(format!("{method} {path}"));
            }
        }
        assert!(
            stale.is_empty(),
            "scope TABLE entries without a registered route: {stale:?}"
        );
        // No duplicate decisions.
        let mut seen = std::collections::HashSet::new();
        for (m, p, _) in TABLE {
            assert!(seen.insert((m, p)), "duplicate TABLE entry {m} {p}");
        }
    }

    #[test]
    fn rate_limiter_allows_burst_then_refills() {
        let mut rl = RateLimiter::new();
        let t0 = Instant::now();
        for _ in 0..20 {
            assert!(rl.check_at("d1", t0), "burst of 20 must pass");
        }
        assert!(
            !rl.check_at("d1", t0),
            "21st in the same instant is refused"
        );
        // Another device has its own bucket.
        assert!(rl.check_at("d2", t0));
        // 100 ms later one token has been refilled.
        assert!(rl.check_at("d1", t0 + Duration::from_millis(100)));
        assert!(!rl.check_at("d1", t0 + Duration::from_millis(100)));
        // Sustained: 1 s later exactly 10 more, not 11.
        let t1 = t0 + Duration::from_millis(1_100);
        for _ in 0..10 {
            assert!(rl.check_at("d1", t1));
        }
        assert!(!rl.check_at("d1", t1));
        // After a long pause: the full burst again (capped at RATE_BURST).
        let t2 = t1 + Duration::from_secs(60);
        for _ in 0..20 {
            assert!(rl.check_at("d1", t2));
        }
        assert!(!rl.check_at("d1", t2));
    }
}
