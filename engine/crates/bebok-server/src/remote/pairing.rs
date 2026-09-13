//! Pairing state machine (WP-M1, F10-3).
//!
//! ```text
//! desktop  POST /remote/pair/start          -> { pairId, code, expiresAt, … }   (Waiting)
//! phone    POST /remote/pair { code, … }    -> long-polls …                     (Requested)
//! desktop  POST /remote/pair/confirm/{id}   -> device created, phone gets token (Consumed)
//!          POST /remote/pair/reject/{id}    -> phone gets 403
//! ```
//!
//! Codes: 8 chars from an alphabet without `0/O/1/I`, TTL 120 s, single use.
//! Wrong codes count per source IP: after 5 failures the IP is locked for
//! 10 minutes (the 6th attempt answers 429). All time arithmetic takes an
//! explicit `Instant` so the unit tests drive a mocked clock.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use subtle::ConstantTimeEq as _;
use tokio::sync::oneshot;

pub const CODE_LEN: usize = 8;
/// No `0`/`O`, `1`/`I`: the code is read off a screen or typed by hand.
pub const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
pub const CODE_TTL: Duration = Duration::from_secs(120);
pub const MAX_ATTEMPTS: u32 = 5;
pub const LOCKOUT: Duration = Duration::from_secs(600);
/// How long `POST /remote/pair` waits for the desktop's confirmation.
pub const CONFIRM_WAIT: Duration = Duration::from_secs(90);

/// What the phone sends with the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairRequest {
    pub device_name: String,
    pub model: String,
    pub platform: String,
    pub ip: String,
}

/// The desktop's decision, delivered to the waiting phone request.
#[derive(Debug)]
pub enum Outcome {
    Confirmed { device_id: String, token: String },
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairError {
    /// Source IP is locked out (429).
    Locked,
    /// Unknown code (404) — counted as an attempt.
    InvalidCode,
    /// The code's 120 s are over (410).
    Expired,
    /// A phone already claimed this code (409).
    AlreadyRequested,
    /// No such pair id (404).
    NotFound,
    /// The pair exists but no phone has sent the code yet (409).
    NotRequested,
}

impl PairError {
    pub fn status(self) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        match self {
            PairError::Locked => StatusCode::TOO_MANY_REQUESTS,
            PairError::InvalidCode | PairError::NotFound => StatusCode::NOT_FOUND,
            PairError::Expired => StatusCode::GONE,
            PairError::AlreadyRequested | PairError::NotRequested => StatusCode::CONFLICT,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            PairError::Locked => "pair_locked",
            PairError::InvalidCode => "pair_invalid_code",
            PairError::Expired => "pair_expired",
            PairError::AlreadyRequested => "pair_already_requested",
            PairError::NotFound => "pair_not_found",
            PairError::NotRequested => "pair_not_requested",
        }
    }
}

enum Stage {
    Waiting,
    Requested {
        request: PairRequest,
        reply: oneshot::Sender<Outcome>,
    },
}

struct PairSession {
    code: String,
    expires: Instant,
    expires_at_ms: u64,
    stage: Stage,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Lockout {
    failures: u32,
    until: Option<Instant>,
}

/// One `POST /remote/pair/start` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub pair_id: String,
    pub code: String,
    /// Unix milliseconds.
    pub expires_at_ms: u64,
}

#[derive(Default)]
pub struct Pairing {
    sessions: HashMap<String, PairSession>,
    lockouts: HashMap<String, Lockout>,
}

/// A fresh 8-char code from [`CODE_ALPHABET`] (CSPRNG).
pub fn generate_code() -> String {
    use rand::RngCore as _;
    let mut bytes = [0u8; CODE_LEN];
    rand::rng().fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|b| CODE_ALPHABET[(*b as usize) % CODE_ALPHABET.len()] as char)
        .collect()
}

/// Normalise what a user typed: uppercase, spaces/dashes removed.
pub fn normalize_code(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

impl Pairing {
    /// Drop expired sessions whose phone is not waiting (a waiting phone
    /// keeps its session until the desktop answers or the poll times out).
    pub fn prune(&mut self, now: Instant) {
        self.sessions
            .retain(|_, s| now < s.expires || matches!(s.stage, Stage::Requested { .. }));
        self.lockouts
            .retain(|_, l| l.until.is_some_and(|u| now < u) || l.failures > 0);
    }

    /// Desktop side: mint a code.
    pub fn start(&mut self, now: Instant, now_ms: u64) -> Started {
        self.prune(now);
        let pair_id = uuid::Uuid::new_v4().simple().to_string();
        let code = generate_code();
        self.sessions.insert(
            pair_id.clone(),
            PairSession {
                code: code.clone(),
                expires: now + CODE_TTL,
                expires_at_ms: now_ms + CODE_TTL.as_millis() as u64,
                stage: Stage::Waiting,
            },
        );
        Started {
            pair_id,
            code,
            expires_at_ms: now_ms + CODE_TTL.as_millis() as u64,
        }
    }

    fn lockout_active(&mut self, ip: &str, now: Instant) -> bool {
        let Some(l) = self.lockouts.get_mut(ip) else {
            return false;
        };
        match l.until {
            Some(until) if now < until => true,
            Some(_) => {
                // Lockout over: start counting afresh.
                *l = Lockout::default();
                false
            }
            None => false,
        }
    }

    fn record_failure(&mut self, ip: &str, now: Instant) {
        let l = self.lockouts.entry(ip.to_string()).or_default();
        l.failures += 1;
        if l.failures >= MAX_ATTEMPTS {
            l.until = Some(now + LOCKOUT);
        }
    }

    /// Phone side: claim a code. On success the returned receiver resolves
    /// when the desktop confirms/rejects.
    pub fn request(
        &mut self,
        code: &str,
        request: PairRequest,
        now: Instant,
    ) -> Result<(String, oneshot::Receiver<Outcome>), PairError> {
        let ip = request.ip.clone();
        if self.lockout_active(&ip, now) {
            return Err(PairError::Locked);
        }
        let code = normalize_code(code);
        // Constant-time scan over every session (no early exit on the first
        // matching byte); the number of live sessions is not secret.
        let mut hit: Option<String> = None;
        for (id, s) in &self.sessions {
            let same_len = s.code.len() == code.len();
            let eq: bool = same_len && bool::from(s.code.as_bytes().ct_eq(code.as_bytes()));
            if eq {
                hit = Some(id.clone());
            }
        }
        let Some(pair_id) = hit else {
            self.record_failure(&ip, now);
            return Err(PairError::InvalidCode);
        };
        let session = self.sessions.get_mut(&pair_id).expect("just found");
        if now >= session.expires {
            self.sessions.remove(&pair_id);
            return Err(PairError::Expired);
        }
        if !matches!(session.stage, Stage::Waiting) {
            return Err(PairError::AlreadyRequested);
        }
        let (tx, rx) = oneshot::channel();
        session.stage = Stage::Requested { request, reply: tx };
        self.lockouts.remove(&ip);
        Ok((pair_id, rx))
    }

    /// Desktop side (confirm/reject): take the pending request. The session
    /// is consumed — the code cannot be used again.
    pub fn take_requested(
        &mut self,
        pair_id: &str,
    ) -> Result<(PairRequest, oneshot::Sender<Outcome>), PairError> {
        let Some(session) = self.sessions.get(pair_id) else {
            return Err(PairError::NotFound);
        };
        if matches!(session.stage, Stage::Waiting) {
            return Err(PairError::NotRequested);
        }
        let session = self.sessions.remove(pair_id).expect("checked above");
        match session.stage {
            Stage::Requested { request, reply } => Ok((request, reply)),
            Stage::Waiting => Err(PairError::NotRequested),
        }
    }

    /// Phone gave up waiting (poll timeout / disconnect): forget the pair so
    /// a late desktop click answers 404 instead of minting a token nobody
    /// receives.
    pub fn abandon(&mut self, pair_id: &str) {
        self.sessions.remove(pair_id);
    }

    /// The pending phone request for `pair_id`.
    #[cfg(test)]
    pub fn pending(&self, pair_id: &str) -> Option<PairRequest> {
        match self.sessions.get(pair_id).map(|s| &s.stage) {
            Some(Stage::Requested { request, .. }) => Some(request.clone()),
            _ => None,
        }
    }

    /// Expiry (unix ms) of a pair, if it exists.
    pub fn expires_at_ms(&self, pair_id: &str) -> Option<u64> {
        self.sessions.get(pair_id).map(|s| s.expires_at_ms)
    }

    #[cfg(test)]
    pub fn live_sessions(&self) -> usize {
        self.sessions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(ip: &str) -> PairRequest {
        PairRequest {
            device_name: "Pixel".into(),
            model: "Pixel 9".into(),
            platform: "android".into(),
            ip: ip.into(),
        }
    }

    #[test]
    fn code_uses_the_unambiguous_alphabet() {
        for _ in 0..50 {
            let c = generate_code();
            assert_eq!(c.len(), CODE_LEN);
            assert!(
                c.bytes().all(|b| CODE_ALPHABET.contains(&b)),
                "{c} has a char outside the alphabet"
            );
            for bad in ['0', 'O', '1', 'I'] {
                assert!(!c.contains(bad));
            }
        }
        assert_eq!(normalize_code(" ab-cd ef "), "ABCDEF");
    }

    #[test]
    fn happy_path_confirm_and_single_use() {
        let mut p = Pairing::default();
        let t0 = Instant::now();
        let started = p.start(t0, 1_000);
        assert_eq!(started.expires_at_ms, 121_000);

        let (pair_id, rx) = p
            .request(
                &started.code.to_lowercase(),
                req("100.64.0.9"),
                t0 + Duration::from_secs(10),
            )
            .expect("valid code");
        assert_eq!(pair_id, started.pair_id);
        assert_eq!(p.pending(&pair_id).unwrap().device_name, "Pixel");

        // A second phone with the same code is refused.
        assert_eq!(
            p.request(
                &started.code,
                req("100.64.0.10"),
                t0 + Duration::from_secs(11)
            )
            .unwrap_err(),
            PairError::AlreadyRequested
        );

        let (request, reply) = p.take_requested(&pair_id).unwrap();
        assert_eq!(request.ip, "100.64.0.9");
        reply
            .send(Outcome::Confirmed {
                device_id: "d1".into(),
                token: "t".into(),
            })
            .unwrap();
        match rx.blocking_recv().unwrap() {
            Outcome::Confirmed { device_id, .. } => assert_eq!(device_id, "d1"),
            Outcome::Rejected => panic!("expected confirmation"),
        }
        // Consumed: the code cannot be reused, the pair id is gone.
        assert_eq!(
            p.request(
                &started.code,
                req("100.64.0.9"),
                t0 + Duration::from_secs(12)
            )
            .unwrap_err(),
            PairError::InvalidCode
        );
        assert_eq!(p.take_requested(&pair_id).unwrap_err(), PairError::NotFound);
        assert_eq!(p.live_sessions(), 0);
    }

    #[test]
    fn reject_and_not_requested() {
        let mut p = Pairing::default();
        let t0 = Instant::now();
        let started = p.start(t0, 0);
        assert_eq!(
            p.take_requested(&started.pair_id).unwrap_err(),
            PairError::NotRequested
        );
        let (pair_id, rx) = p.request(&started.code, req("1.2.3.4"), t0).unwrap();
        let (_, reply) = p.take_requested(&pair_id).unwrap();
        reply.send(Outcome::Rejected).unwrap();
        assert!(matches!(rx.blocking_recv().unwrap(), Outcome::Rejected));
    }

    #[test]
    fn expired_code_answers_expired_not_invalid() {
        let mut p = Pairing::default();
        let t0 = Instant::now();
        let started = p.start(t0, 0);
        let late = t0 + CODE_TTL + Duration::from_secs(1);
        assert_eq!(
            p.request(&started.code, req("1.2.3.4"), late).unwrap_err(),
            PairError::Expired
        );
        // Not counted as a failed attempt; and the session is gone.
        assert_eq!(p.lockouts.get("1.2.3.4"), None);
        assert_eq!(p.live_sessions(), 0);
        // Still valid one second before the deadline.
        let started = p.start(t0, 0);
        assert!(
            p.request(
                &started.code,
                req("1.2.3.4"),
                t0 + CODE_TTL - Duration::from_secs(1)
            )
            .is_ok()
        );
    }

    #[test]
    fn five_wrong_codes_lock_the_ip_for_ten_minutes() {
        let mut p = Pairing::default();
        let t0 = Instant::now();
        let started = p.start(t0, 0);
        for i in 0..MAX_ATTEMPTS {
            assert_eq!(
                p.request(
                    "WRONGWRO",
                    req("9.9.9.9"),
                    t0 + Duration::from_secs(i as u64)
                )
                .unwrap_err(),
                PairError::InvalidCode,
                "attempt {}",
                i + 1
            );
        }
        // 6th attempt: locked, even with the right code.
        assert_eq!(
            p.request(&started.code, req("9.9.9.9"), t0 + Duration::from_secs(6))
                .unwrap_err(),
            PairError::Locked
        );
        // Another IP is unaffected.
        assert!(
            p.request(&started.code, req("8.8.8.8"), t0 + Duration::from_secs(6))
                .is_ok()
        );
        // After the lockout the counter resets and attempts work again.
        let later = t0 + LOCKOUT + Duration::from_secs(7);
        let fresh = p.start(later, 0);
        assert_eq!(
            p.request("WRONGWRO", req("9.9.9.9"), later).unwrap_err(),
            PairError::InvalidCode
        );
        assert!(p.request(&fresh.code, req("9.9.9.9"), later).is_ok());
        // Success clears the failure count for that IP.
        assert!(!p.lockouts.contains_key("9.9.9.9"));
    }

    #[test]
    fn abandon_forgets_the_pair() {
        let mut p = Pairing::default();
        let t0 = Instant::now();
        let started = p.start(t0, 0);
        let (pair_id, _rx) = p.request(&started.code, req("1.1.1.1"), t0).unwrap();
        p.abandon(&pair_id);
        assert_eq!(p.take_requested(&pair_id).unwrap_err(), PairError::NotFound);
    }

    #[test]
    fn prune_drops_expired_waiting_sessions_only() {
        let mut p = Pairing::default();
        let t0 = Instant::now();
        let waiting = p.start(t0, 0);
        let requested = p.start(t0, 0);
        let _ = p.request(&requested.code, req("1.1.1.1"), t0).unwrap();
        p.prune(t0 + CODE_TTL + Duration::from_secs(1));
        assert!(p.expires_at_ms(&waiting.pair_id).is_none());
        assert!(p.pending(&requested.pair_id).is_some());
    }
}
