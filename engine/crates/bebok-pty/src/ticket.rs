//! Single-use, short-lived connect tickets.
//!
//! A browser `WebSocket` cannot set custom headers, so the WS upgrade carries
//! a `?ticket=` query param instead. Tickets are bound to one pty, expire after
//! a short TTL, and are consumed atomically (first connect wins, replay fails).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a ticket stays valid before it is rejected.
pub const TICKET_TTL: Duration = Duration::from_secs(30);

struct Ticket {
    pty_id: String,
    expires_at: Instant,
}

/// In-memory store of outstanding tickets.
#[derive(Default)]
pub struct TicketStore {
    inner: Mutex<HashMap<String, Ticket>>,
}

impl TicketStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a fresh one-time ticket bound to `pty_id`. Returns the token.
    pub fn issue(&self, pty_id: &str) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string();
        let mut inner = self.inner.lock().unwrap();
        inner.insert(
            token.clone(),
            Ticket {
                pty_id: pty_id.to_string(),
                expires_at: Instant::now() + TICKET_TTL,
            },
        );
        // Opportunistic cleanup of expired tickets on issue.
        inner.retain(|_, t| t.expires_at > Instant::now());
        token
    }

    /// Atomically consume a ticket. Returns the bound pty id on success, or
    /// `None` when the token is unknown, already used, or expired.
    pub fn consume(&self, token: &str) -> Option<String> {
        let mut inner = self.inner.lock().unwrap();
        match inner.remove(token) {
            Some(t) if t.expires_at > Instant::now() => Some(t.pty_id),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_is_one_time() {
        let store = TicketStore::new();
        let token = store.issue("pty-1");
        assert_eq!(store.consume(&token).as_deref(), Some("pty-1"));
        // Second use is rejected (one-time).
        assert!(store.consume(&token).is_none());
    }

    #[test]
    fn unknown_ticket_rejected() {
        let store = TicketStore::new();
        assert!(store.consume("nope").is_none());
    }

    #[test]
    fn scope_is_bound_to_pty() {
        let store = TicketStore::new();
        let token = store.issue("pty-A");
        // The ticket resolves to the pty it was issued for, not another.
        assert_eq!(store.consume(&token).as_deref(), Some("pty-A"));
    }
}
