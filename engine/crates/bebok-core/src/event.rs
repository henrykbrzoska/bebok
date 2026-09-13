//! Global event bus (SPEC §3.11).
//!
//! One stream (`GET /event`), every event carries the envelope
//! `{ type, directory, sessionID, properties }` so clients can route by
//! directory. No polling anywhere.
//!
//! WP-M1 (F10-4, closes F3-17): every published event gets a monotonic
//! `seq` (the SSE `id:`) and the bus keeps a ring buffer of recent events
//! ([`RING_CAPACITY`] events or [`RING_MAX_AGE`], whichever is smaller) so a
//! client reconnecting with `Last-Event-ID` gets exactly what it missed —
//! or a `resync` hint when the gap is older than the buffer. High-volume
//! kinds ([`UNBUFFERED_KINDS`]: browser thumbnails, process output) are
//! sequenced but never buffered.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

/// Ring buffer size (events).
pub const RING_CAPACITY: usize = 2_000;
/// Ring buffer age limit.
pub const RING_MAX_AGE: Duration = Duration::from_secs(600);
/// Kinds that are sequenced but not retained for replay.
pub const UNBUFFERED_KINDS: &[&str] = &["browser.frame", "process.output"];

#[derive(Debug, Clone, serde::Serialize)]
pub struct Event {
    #[serde(rename = "type")]
    pub kind: String,
    pub directory: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub properties: serde_json::Value,
    /// Monotonic sequence number assigned by [`EventBus::publish`] (0 before
    /// publication). Serialised as `seq`; also the SSE `id:`.
    #[serde(default, skip_serializing_if = "seq_is_zero")]
    pub seq: u64,
}

fn seq_is_zero(seq: &u64) -> bool {
    *seq == 0
}

impl Event {
    pub fn new(kind: &str, directory: &str, session_id: &str) -> Self {
        Self {
            kind: kind.to_string(),
            directory: directory.to_string(),
            session_id: session_id.to_string(),
            properties: serde_json::Value::Null,
            seq: 0,
        }
    }

    pub fn with_properties(mut self, properties: serde_json::Value) -> Self {
        self.properties = properties;
        self
    }

    /// True for kinds that are never kept in the replay buffer.
    pub fn is_unbuffered(&self) -> bool {
        UNBUFFERED_KINDS.contains(&self.kind.as_str())
    }
}

/// What a resuming subscriber gets: the events it missed (in order) or a
/// `resync` flag when the gap cannot be filled from the buffer.
pub struct Replay {
    /// Buffered events with `seq > last_seen`, oldest first.
    pub events: Vec<Event>,
    /// `last_seen` is older than the buffer (or from another engine
    /// process): the client must refresh its state fully.
    pub resync: bool,
    pub rx: broadcast::Receiver<Event>,
}

struct Ring {
    next_seq: u64,
    /// `(published at, event)`; `event.seq` strictly increasing.
    buffer: VecDeque<(Instant, Event)>,
    /// Every buffered kind with `seq >= window_start` is still in `buffer`.
    window_start: u64,
}

impl Ring {
    fn evict(&mut self, now: Instant) {
        while let Some((at, ev)) = self.buffer.front() {
            if self.buffer.len() > RING_CAPACITY
                || now.saturating_duration_since(*at) > RING_MAX_AGE
            {
                self.window_start = ev.seq + 1;
                self.buffer.pop_front();
            } else {
                break;
            }
        }
    }
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
    ring: Arc<Mutex<Ring>>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            tx,
            ring: Arc::new(Mutex::new(Ring {
                next_seq: 1,
                buffer: VecDeque::with_capacity(RING_CAPACITY + 1),
                window_start: 1,
            })),
        }
    }

    /// Publish an event; a missing subscriber is not an error. Assigns the
    /// sequence number and buffers the event (unless unbuffered) under one
    /// lock, so broadcast order and `seq` order always agree.
    pub fn publish(&self, mut event: Event) {
        let kind = event.kind.clone();
        let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        event.seq = ring.next_seq;
        ring.next_seq += 1;
        if !event.is_unbuffered() {
            ring.buffer.push_back((Instant::now(), event.clone()));
            ring.evict(Instant::now());
        }
        if self.tx.send(event).is_err() {
            tracing::debug!("event {kind} dropped: no active subscribers");
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    /// The last sequence number handed out (0 = nothing published yet).
    pub fn last_seq(&self) -> u64 {
        self.ring.lock().unwrap_or_else(|e| e.into_inner()).next_seq - 1
    }

    /// Subscribe and, when `last_seen` is given, collect what the client
    /// missed. The snapshot and the subscription happen under the ring lock,
    /// so no event can fall between the replay and the live stream.
    pub fn subscribe_from(&self, last_seen: Option<u64>) -> Replay {
        let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        ring.evict(Instant::now());
        let rx = self.tx.subscribe();
        let last_seq = ring.next_seq - 1;
        let Some(n) = last_seen else {
            return Replay {
                events: Vec::new(),
                resync: false,
                rx,
            };
        };
        if n == last_seq {
            return Replay {
                events: Vec::new(),
                resync: false,
                rx,
            };
        }
        // Ids from the future belong to a previous engine process.
        if n > last_seq || n + 1 < ring.window_start {
            return Replay {
                events: Vec::new(),
                resync: true,
                rx,
            };
        }
        let events = ring
            .buffer
            .iter()
            .filter(|(_, ev)| ev.seq > n)
            .map(|(_, ev)| ev.clone())
            .collect();
        Replay {
            events,
            resync: false,
            rx,
        }
    }

    /// Number of events currently retained (tests / diagnostics).
    pub fn buffered_len(&self) -> usize {
        self.ring
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .buffer
            .len()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: &str) -> Event {
        Event::new(kind, "C:/p", "s1")
    }

    #[test]
    fn publish_assigns_monotonic_seq_and_serialises_it() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        bus.publish(ev("session.updated"));
        bus.publish(ev("browser.frame"));
        bus.publish(ev("turn.end"));
        let a = rx.try_recv().unwrap();
        let b = rx.try_recv().unwrap();
        let c = rx.try_recv().unwrap();
        assert_eq!((a.seq, b.seq, c.seq), (1, 2, 3));
        assert_eq!(bus.last_seq(), 3);
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["seq"], 3);
        assert_eq!(json["type"], "turn.end");
        // An unpublished event has no `seq` key.
        let json = serde_json::to_value(ev("x")).unwrap();
        assert!(json.get("seq").is_none());
        // Unbuffered kinds are sequenced but not retained.
        assert_eq!(bus.buffered_len(), 2);
    }

    #[test]
    fn replay_returns_exactly_the_missed_events() {
        let bus = EventBus::new(16);
        for i in 0..10 {
            bus.publish(ev(&format!("k{i}")));
        }
        // Missed the last 5 (seq 6..=10).
        let replay = bus.subscribe_from(Some(5));
        assert!(!replay.resync);
        let seqs: Vec<u64> = replay.events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![6, 7, 8, 9, 10]);
        assert_eq!(replay.events[0].kind, "k5");
        // Up to date: nothing.
        let replay = bus.subscribe_from(Some(10));
        assert!(replay.events.is_empty() && !replay.resync);
        // No header: nothing, no resync.
        let replay = bus.subscribe_from(None);
        assert!(replay.events.is_empty() && !replay.resync);
        // From the future (previous engine process): resync.
        let replay = bus.subscribe_from(Some(11));
        assert!(replay.resync && replay.events.is_empty());
        // Live events after the snapshot still arrive on the receiver.
        let mut replay = bus.subscribe_from(Some(9));
        bus.publish(ev("live"));
        assert_eq!(replay.events.len(), 1);
        assert_eq!(replay.rx.try_recv().unwrap().kind, "live");
    }

    #[test]
    fn replay_skips_unbuffered_kinds_without_a_gap() {
        let bus = EventBus::new(16);
        bus.publish(ev("a")); // 1
        bus.publish(ev("browser.frame")); // 2 (not retained)
        bus.publish(ev("process.output")); // 3 (not retained)
        bus.publish(ev("b")); // 4
        let replay = bus.subscribe_from(Some(1));
        assert!(!replay.resync);
        let seqs: Vec<u64> = replay.events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![4]);
        // Resuming from an unbuffered id is fine too.
        let replay = bus.subscribe_from(Some(2));
        assert_eq!(replay.events.len(), 1);
        assert!(!replay.resync);
    }

    #[test]
    fn ring_evicts_beyond_capacity_and_signals_resync() {
        let bus = EventBus::new(16);
        // 3 000 events: the first 1 000 fall out of the 2 000 window.
        for i in 0..3_000 {
            bus.publish(ev(&format!("k{i}")));
        }
        assert_eq!(bus.buffered_len(), RING_CAPACITY);
        assert_eq!(bus.last_seq(), 3_000);
        // Window is seq 1001..=3000.
        let replay = bus.subscribe_from(Some(1_000));
        assert!(!replay.resync, "exactly at the window start is replayable");
        assert_eq!(replay.events.len(), 2_000);
        assert_eq!(replay.events[0].seq, 1_001);
        let replay = bus.subscribe_from(Some(999));
        assert!(replay.resync, "one before the window: resync");
        assert!(replay.events.is_empty());
        let replay = bus.subscribe_from(Some(0));
        assert!(replay.resync, "3 000 missed events: resync");
        let replay = bus.subscribe_from(Some(2_995));
        assert_eq!(replay.events.len(), 5);
    }

    #[test]
    fn ring_evicts_by_age() {
        let bus = EventBus::new(16);
        bus.publish(ev("old"));
        // Backdate the entry beyond RING_MAX_AGE.
        {
            let mut ring = bus.ring.lock().unwrap();
            let stale = Instant::now() - (RING_MAX_AGE + Duration::from_secs(1));
            ring.buffer.front_mut().unwrap().0 = stale;
        }
        bus.publish(ev("new"));
        assert_eq!(bus.buffered_len(), 1);
        let replay = bus.subscribe_from(Some(0));
        assert!(replay.resync, "the old event is gone: resync");
        let replay = bus.subscribe_from(Some(1));
        assert!(!replay.resync);
        assert_eq!(replay.events.len(), 1);
        assert_eq!(replay.events[0].kind, "new");
    }
}
