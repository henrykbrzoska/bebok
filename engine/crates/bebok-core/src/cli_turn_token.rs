//! Short-lived, turn-scoped tokens for CLI hook callbacks.
//!
//! A CLI provider (Claude Code) is told to run `bebok-server cli-hook <event>`
//! before/after every tool call and at the end of the turn. That child process
//! has to talk back to *this* engine process over loopback HTTP, and it must not
//! be able to do anything else: the engine's own capability token
//! (`auth::token()`) is far too broad to hand to a hook command line.
//!
//! So the CLI provider gets a separate credential per turn:
//!
//! - **issued** when the turn starts (the provider's env carries
//!   `BEBOK_HOOK_TOKEN`),
//! - **scoped** to `(session, turn)` — a token of one session is refused by
//!   another session's hook route,
//! - **revoked** when the turn ends or is cancelled,
//! - **expired** after [`TURN_TOKEN_TTL`] as a last-resort safety net (a leaked
//!   token from a crashed engine must not stay valid forever).
//!
//! Unlike the PTY connect tickets (`bebok-pty::TicketStore`, single use) a turn
//! token is *reusable*: one turn makes many tool calls, and each one is a hook.
//! The pattern is the same otherwise: an in-memory `HashMap` behind a `Mutex`,
//! opportunistic cleanup on issue, no persistence anywhere.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long a turn token stays valid before it is rejected (hard ceiling; the
/// normal path revokes it at the end of the turn).
pub const TURN_TOKEN_TTL: Duration = Duration::from_secs(12 * 60 * 60);

/// The engine's own listening URL, e.g. `http://127.0.0.1:8787`. Set once by
/// `server::serve` after binding (the desktop sidecar binds a random port).
static ENGINE_BASE_URL: OnceLock<String> = OnceLock::new();

/// Remember the bound address so CLI turns can hand their hooks a URL.
pub fn set_engine_base_url(url: impl Into<String>) {
    let _ = ENGINE_BASE_URL.set(url.into());
}

/// The engine's base URL, when it is known (not in unit tests / MCP mode).
pub fn engine_base_url() -> Option<&'static str> {
    ENGINE_BASE_URL.get().map(String::as_str)
}

/// Issue the per-turn hook environment for a CLI session.
///
/// Returns `(token, env)`: the token so the caller can revoke it when the turn
/// ends, and the environment (`BEBOK_HOOK_URL`, `BEBOK_HOOK_TOKEN`) the CLI
/// child must see. `None` when the engine does not know its own address — then
/// no hooks are wired at all, which is safer than a URL pointing nowhere.
///
/// Lives in core (not the server route) because *every* path that builds a CLI
/// provider must issue it: a delegated child (`task` / `fleet`) runs in-process
/// here, and a child whose CLI has the managed profile's hooks but no
/// `BEBOK_HOOK_TOKEN` has every tool call denied fail-closed by `cli-hook`.
/// The token is scoped to the session it is issued for, so a child gets its own.
pub fn issue_hook_env(session: &str) -> Option<(String, Vec<(String, String)>)> {
    Some(issue_hook_env_at(engine_base_url()?, session))
}

/// The pure part of [`issue_hook_env`]: build the credential for one session
/// against a known engine `base` URL. Kept separate so tests can cover both the
/// "engine knows its address" and the "no address" cases without racing on the
/// process-wide [`ENGINE_BASE_URL`].
pub fn issue_hook_env_at(base: &str, session: &str) -> (String, Vec<(String, String)>) {
    let (token, _scope) = TurnTokenStore::global().issue(session);
    let env = vec![
        (
            "BEBOK_HOOK_URL".to_string(),
            format!("{}/cli-hook/{session}", base.trim_end_matches('/')),
        ),
        ("BEBOK_HOOK_TOKEN".to_string(), token.clone()),
    ];
    (token, env)
}

/// Revoke a turn token issued by [`issue_hook_env`].
pub fn revoke_hook_token(token: &str) {
    TurnTokenStore::global().revoke(token);
}

/// What a token is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnScope {
    /// Bebok session id (the hook route's `{session}` path segment).
    pub session: String,
    /// Turn id, for diagnostics: one token per started turn.
    pub turn: String,
}

struct Token {
    scope: TurnScope,
    expires_at: Instant,
}

/// In-memory store of outstanding turn tokens.
#[derive(Default)]
pub struct TurnTokenStore {
    inner: Mutex<HashMap<String, Token>>,
}

impl TurnTokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The process-wide store. The hook route and the turn runner must agree on
    /// exactly one instance, so it lives here rather than in `AppState` (the CLI
    /// child cannot be handed an `Arc`).
    pub fn global() -> &'static Self {
        static GLOBAL: OnceLock<TurnTokenStore> = OnceLock::new();
        GLOBAL.get_or_init(TurnTokenStore::new)
    }

    /// Issue a fresh token bound to `session`, using the default TTL.
    pub fn issue(&self, session: &str) -> (String, TurnScope) {
        self.issue_with_ttl(session, TURN_TOKEN_TTL)
    }

    fn issue_with_ttl(&self, session: &str, ttl: Duration) -> (String, TurnScope) {
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let scope = TurnScope {
            session: session.to_string(),
            turn: uuid::Uuid::new_v4().to_string(),
        };
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        // Opportunistic cleanup of expired tokens on issue.
        inner.retain(|_, t| t.expires_at > now);
        inner.insert(
            token.clone(),
            Token {
                scope: scope.clone(),
                expires_at: now + ttl,
            },
        );
        (token, scope)
    }

    /// Resolve a token to its scope. `None` when unknown or expired.
    pub fn validate(&self, token: &str) -> Option<TurnScope> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .get(token)
            .filter(|t| t.expires_at > Instant::now())
            .map(|t| t.scope.clone())
    }

    /// Drop one token (turn finished / cancelled). Returns whether it existed.
    pub fn revoke(&self, token: &str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(token)
            .is_some()
    }

    /// Drop every token of one session (session deleted, engine-side cleanup).
    pub fn revoke_session(&self, session: &str) -> usize {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = inner.len();
        inner.retain(|_, t| t.scope.session != session);
        before - inner.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_validates_to_its_scope() {
        let store = TurnTokenStore::new();
        let (token, scope) = store.issue("sess-1");
        assert_eq!(store.validate(&token), Some(scope));
    }

    #[test]
    fn unknown_token_is_rejected() {
        let store = TurnTokenStore::new();
        assert!(store.validate("nope").is_none());
    }

    #[test]
    fn token_is_reusable_within_a_turn() {
        // Many tool calls share one turn token; a hook per call must validate.
        let store = TurnTokenStore::new();
        let (token, _) = store.issue("sess-1");
        assert!(store.validate(&token).is_some());
        assert!(store.validate(&token).is_some());
    }

    #[test]
    fn revoked_token_is_rejected() {
        let store = TurnTokenStore::new();
        let (token, _) = store.issue("sess-1");
        assert!(store.revoke(&token));
        assert!(store.validate(&token).is_none());
        assert!(!store.revoke(&token), "second revoke is a no-op");
    }

    #[test]
    fn expired_token_is_rejected() {
        let store = TurnTokenStore::new();
        let (token, _) = store.issue_with_ttl("sess-1", Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));
        assert!(store.validate(&token).is_none());
    }

    #[test]
    fn revoking_a_session_leaves_other_sessions_alone() {
        let store = TurnTokenStore::new();
        let (a, _) = store.issue("sess-a");
        let (b, _) = store.issue("sess-b");
        assert_eq!(store.revoke_session("sess-a"), 1);
        assert!(store.validate(&a).is_none());
        assert!(store.validate(&b).is_some());
    }

    #[test]
    fn each_turn_gets_a_distinct_token_and_turn_id() {
        let store = TurnTokenStore::new();
        let (t1, s1) = store.issue("sess-1");
        let (t2, s2) = store.issue("sess-1");
        assert_ne!(t1, t2);
        assert_ne!(s1.turn, s2.turn, "one token per turn");
        assert_eq!(s1.session, s2.session);
    }

    #[test]
    fn hook_env_points_at_the_session_and_carries_a_valid_token() {
        // The pair a CLI child must see, or `cli-hook` denies every tool call
        // fail-closed. A delegated child gets `child` here — never the parent.
        let (token, env) = issue_hook_env_at("http://127.0.0.1:8787", "child-1");
        let get = |key: &str| env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        assert_eq!(
            get("BEBOK_HOOK_URL"),
            Some("http://127.0.0.1:8787/cli-hook/child-1")
        );
        assert_eq!(get("BEBOK_HOOK_TOKEN"), Some(token.as_str()));
        assert_eq!(env.len(), 2);

        let scope = TurnTokenStore::global()
            .validate(&token)
            .expect("issued token validates");
        assert_eq!(scope.session, "child-1", "scoped to the child session");

        revoke_hook_token(&token);
        assert!(TurnTokenStore::global().validate(&token).is_none());
    }

    #[test]
    fn hook_env_without_an_engine_address_is_absent() {
        // The route cannot be reached without a URL; callers must not invent
        // one (the profile's hooks would then deny every call).
        if engine_base_url().is_none() {
            assert!(issue_hook_env("sess-1").is_none());
        }
    }
}
