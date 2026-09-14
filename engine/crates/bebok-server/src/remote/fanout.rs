//! SSE fan-out filter for the `remote` scope (WP-M1, F10-4).
//!
//! A phone on a metered, sleepy link must not receive the desktop's raw
//! firehose. For a remote stream the bus is wrapped in a [`Coalescer`]:
//!
//! | kind                        | policy                                                        |
//! |-----------------------------|---------------------------------------------------------------|
//! | `session.*`, `message.updated`, `turn.*`, `permission.*`, `task.*`, `agent.*`, `fleet.*`, `browser.closed`, `remote.status`, `remote.device.changed` | pass through |
//! | `message.part.updated`      | latest snapshot per `(sessionID, messageIndex)`, ≤ 1 / 250 ms |
//! | `process.output`            | last 20 lines per process, ≤ 1 / s (off with `publish.processes=false`) |
//! | `process.exited`            | pass (flushes pending output first)                            |
//! | `browser.frame`             | only with `?interest=browser` + `publish.browser_frames`, ≤ 1 fps per session |
//! | `config.changed`            | properties stripped of `*key*` / `*token*` / `*secret*` keys  |
//! | `debug.log`, `pty.*`, `remote.pair.request`, anything else | dropped |
//!
//! The coalescer is a pure state machine driven by an explicit clock so it
//! is unit-testable; [`spawn`] runs it against a live broadcast receiver and
//! also ends the stream when the device is revoked (generation change).
//!
//! Thumbnails are **not** downscaled: the `image` crate is not a dependency
//! of the engine, so `browser.frame` is only rate-limited (see the WP-M1
//! report, open issues).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bebok_core::event::Event;
use tokio::sync::{broadcast, mpsc};

use super::{RemoteState, SseSlot};

pub const PART_INTERVAL: Duration = Duration::from_millis(250);
pub const PROCESS_INTERVAL: Duration = Duration::from_secs(1);
pub const FRAME_INTERVAL: Duration = Duration::from_secs(1);
pub const PROCESS_TAIL_LINES: usize = 20;
/// How often a live stream re-checks the device generation (revoke).
pub const REVOKE_POLL: Duration = Duration::from_secs(5);

/// Per-stream options (from the request + config).
#[derive(Debug, Clone, Default)]
pub struct FanoutOptions {
    pub interest_browser: bool,
    pub publish_browser_frames: bool,
    pub publish_processes: bool,
}

/// What the SSE writer receives.
#[derive(Debug)]
pub enum Frame {
    Event(Event),
    /// `event: resync` — the client must refresh fully.
    Resync,
    /// `event: revoked` — the device was revoked; the stream ends after it.
    Revoked,
}

#[derive(Debug)]
struct Pending<T> {
    last_emit: Option<Instant>,
    pending: Option<T>,
}

impl<T> Default for Pending<T> {
    fn default() -> Self {
        Self {
            last_emit: None,
            pending: None,
        }
    }
}

/// Accumulated `process.output` for one process.
#[derive(Debug)]
struct ProcessTail {
    template: Event,
    text: String,
}

/// The pure filter/coalescing state machine.
pub struct Coalescer {
    opts: FanoutOptions,
    parts: HashMap<(String, u64), Pending<Event>>,
    processes: HashMap<String, Pending<ProcessTail>>,
    frames: HashMap<String, Pending<Event>>,
}

impl Coalescer {
    pub fn new(opts: FanoutOptions) -> Self {
        Self {
            opts,
            parts: HashMap::new(),
            processes: HashMap::new(),
            frames: HashMap::new(),
        }
    }

    /// Kinds forwarded verbatim.
    fn passthrough(kind: &str) -> bool {
        kind.starts_with("session.")
            || kind == "message.updated"
            || kind.starts_with("turn.")
            || kind.starts_with("permission.")
            || kind.starts_with("task.")
            || kind.starts_with("agent.")
            || kind.starts_with("fleet.")
            || kind == "browser.closed"
            || kind == "remote.status"
            || kind == "remote.device.changed"
    }

    /// Offer one bus event at `now`; returns whatever becomes emittable
    /// right away (possibly several events, e.g. a flushed process tail
    /// before its `process.exited`).
    pub fn offer(&mut self, ev: Event, now: Instant) -> Vec<Event> {
        let kind = ev.kind.as_str();
        if Self::passthrough(kind) {
            return vec![ev];
        }
        match kind {
            "message.part.updated" => {
                let key = (
                    ev.session_id.clone(),
                    ev.properties
                        .get("messageIndex")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(u64::MAX),
                );
                let slot = self.parts.entry(key).or_default();
                if due(slot.last_emit, now, PART_INTERVAL) {
                    slot.last_emit = Some(now);
                    slot.pending = None;
                    vec![ev]
                } else {
                    slot.pending = Some(ev);
                    Vec::new()
                }
            }
            "process.output" => {
                if !self.opts.publish_processes {
                    return Vec::new();
                }
                let id = ev
                    .properties
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let chunk = ev
                    .properties
                    .get("chunk")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let slot = self.processes.entry(id).or_default();
                match slot.pending.as_mut() {
                    Some(tail) => {
                        tail.text.push_str(&chunk);
                        tail.text = tail_lines(&tail.text, PROCESS_TAIL_LINES);
                        tail.template = ev;
                    }
                    None => {
                        slot.pending = Some(ProcessTail {
                            template: ev,
                            text: tail_lines(&chunk, PROCESS_TAIL_LINES),
                        });
                    }
                }
                if due(slot.last_emit, now, PROCESS_INTERVAL) {
                    slot.last_emit = Some(now);
                    slot.pending
                        .take()
                        .map(|t| vec![render_tail(t)])
                        .unwrap_or_default()
                } else {
                    Vec::new()
                }
            }
            "process.exited" => {
                if !self.opts.publish_processes {
                    return Vec::new();
                }
                let id = ev
                    .properties
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let mut out = Vec::new();
                if let Some(slot) = self.processes.remove(id)
                    && let Some(tail) = slot.pending
                {
                    out.push(render_tail(tail));
                }
                out.push(ev);
                out
            }
            "browser.frame" => {
                if !(self.opts.interest_browser && self.opts.publish_browser_frames) {
                    return Vec::new();
                }
                let slot = self.frames.entry(ev.session_id.clone()).or_default();
                if due(slot.last_emit, now, FRAME_INTERVAL) {
                    slot.last_emit = Some(now);
                    slot.pending = None;
                    vec![ev]
                } else {
                    slot.pending = Some(ev);
                    Vec::new()
                }
            }
            "config.changed" => {
                let mut ev = ev;
                strip_secrets(&mut ev.properties);
                vec![ev]
            }
            _ => {
                tracing::trace!("remote fan-out dropped {kind}");
                Vec::new()
            }
        }
    }

    /// Emit every pending item whose interval has elapsed.
    pub fn flush_due(&mut self, now: Instant) -> Vec<Event> {
        let mut out = Vec::new();
        for slot in self.parts.values_mut() {
            if slot.pending.is_some() && due(slot.last_emit, now, PART_INTERVAL) {
                slot.last_emit = Some(now);
                out.extend(slot.pending.take());
            }
        }
        for slot in self.processes.values_mut() {
            if slot.pending.is_some() && due(slot.last_emit, now, PROCESS_INTERVAL) {
                slot.last_emit = Some(now);
                out.extend(slot.pending.take().map(render_tail));
            }
        }
        for slot in self.frames.values_mut() {
            if slot.pending.is_some() && due(slot.last_emit, now, FRAME_INTERVAL) {
                slot.last_emit = Some(now);
                out.extend(slot.pending.take());
            }
        }
        out.sort_by_key(|e| e.seq);
        out
    }

    /// The earliest instant at which [`flush_due`](Self::flush_due) would
    /// emit something (`None` = nothing pending).
    pub fn next_deadline(&self) -> Option<Instant> {
        let mut earliest: Option<Instant> = None;
        let mut consider = |last: Option<Instant>, interval: Duration| {
            let at = last.map(|l| l + interval).unwrap_or_else(Instant::now);
            earliest = Some(earliest.map_or(at, |e: Instant| e.min(at)));
        };
        for s in self.parts.values().filter(|s| s.pending.is_some()) {
            consider(s.last_emit, PART_INTERVAL);
        }
        for s in self.processes.values().filter(|s| s.pending.is_some()) {
            consider(s.last_emit, PROCESS_INTERVAL);
        }
        for s in self.frames.values().filter(|s| s.pending.is_some()) {
            consider(s.last_emit, FRAME_INTERVAL);
        }
        earliest
    }
}

fn due(last: Option<Instant>, now: Instant, interval: Duration) -> bool {
    match last {
        None => true,
        Some(l) => now.saturating_duration_since(l) >= interval,
    }
}

/// Keep the last `n` lines (a trailing newline does not count as a line).
pub fn tail_lines(text: &str, n: usize) -> String {
    let trimmed_end = text.strip_suffix('\n').unwrap_or(text);
    let lines: Vec<&str> = trimmed_end.split('\n').collect();
    if lines.len() <= n {
        return text.to_string();
    }
    let mut out = lines[lines.len() - n..].join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn render_tail(tail: ProcessTail) -> Event {
    let mut ev = tail.template;
    if let Some(obj) = ev.properties.as_object_mut() {
        obj.insert("chunk".into(), serde_json::Value::String(tail.text));
        obj.insert("coalesced".into(), serde_json::Value::Bool(true));
    }
    ev
}

/// True for keys that look like credentials (`api_key`, `token`,
/// `clientSecret`, …).
pub fn secret_key(name: &str) -> bool {
    let k = name.to_ascii_lowercase();
    k.contains("key") || k.contains("token") || k.contains("secret") || k == "password"
}

/// Recursively drop credential-looking keys from a JSON value.
pub fn strip_secrets(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.retain(|k, _| !secret_key(k));
            for v in map.values_mut() {
                strip_secrets(v);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_secrets),
        _ => {}
    }
}

/// Everything one remote stream needs besides the bus subscription.
pub struct RemoteStream {
    pub opts: FanoutOptions,
    pub state: Arc<RemoteState>,
    pub device_id: String,
    /// Registry generation at authentication time.
    pub generation: u64,
    /// 1.8 share link: only this session's events (and the session-less
    /// `remote.status` / `permission.*` frames that carry no other session).
    pub session: Option<String>,
    /// Released when the stream task ends.
    pub slot: SseSlot,
}

/// Share-scope event filter: keep events of the shared session; drop every
/// other session's. Session-less frames pass only when they are not about
/// another session (they carry an empty `session_id`).
fn share_visible(session: Option<&str>, ev: &Event) -> bool {
    match session {
        None => true,
        Some(sid) => ev.session_id == sid || ev.session_id.is_empty(),
    }
}

/// Run the coalescer over a live receiver. Ends (dropping the sender) when
/// the client goes away, the bus closes, or the device's generation changes.
/// The replay (`Last-Event-ID`) is fed through the same filter before the
/// live events.
pub fn spawn(replay: bebok_core::event::Replay, stream: RemoteStream) -> mpsc::Receiver<Frame> {
    let RemoteStream {
        opts,
        state,
        device_id,
        generation,
        session,
        slot,
    } = stream;
    let bebok_core::event::Replay {
        events: replay,
        resync,
        mut rx,
    } = replay;
    let (tx, out) = mpsc::channel::<Frame>(256);
    tokio::spawn(async move {
        let _slot = slot;
        let mut co = Coalescer::new(opts);
        if resync && tx.send(Frame::Resync).await.is_err() {
            return;
        }
        for ev in replay {
            if !share_visible(session.as_deref(), &ev) {
                continue;
            }
            for e in co.offer(ev, Instant::now()) {
                if tx.send(Frame::Event(e)).await.is_err() {
                    return;
                }
            }
        }
        let mut revoke_tick = tokio::time::interval(REVOKE_POLL);
        revoke_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let deadline = co.next_deadline();
            let flush = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                biased;
                _ = tx.closed() => return,
                _ = revoke_tick.tick() => {
                    if state.device_generation(&device_id) != Some(generation) {
                        tracing::info!(device = %device_id, "remote stream ended: device revoked");
                        let _ = tx.send(Frame::Revoked).await;
                        return;
                    }
                }
                recv = rx.recv() => match recv {
                    Ok(ev) => {
                        if !share_visible(session.as_deref(), &ev) {
                            continue;
                        }
                        for e in co.offer(ev, Instant::now()) {
                            if tx.send(Frame::Event(e)).await.is_err() {
                                return;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(device = %device_id, "remote stream lagged, dropped {n} events");
                        if tx.send(Frame::Resync).await.is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                _ = flush => {
                    for e in co.flush_due(Instant::now()) {
                        if tx.send(Frame::Event(e)).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(session: &str, idx: u64, seq: u64) -> Event {
        let mut e = Event::new("message.part.updated", "C:/p", session)
            .with_properties(serde_json::json!({ "messageIndex": idx, "text": format!("t{seq}") }));
        e.seq = seq;
        e
    }

    fn all_opts() -> FanoutOptions {
        FanoutOptions {
            interest_browser: true,
            publish_browser_frames: true,
            publish_processes: true,
        }
    }

    /// 40 deltas in 100 ms for one part -> at most 2 emitted (the first
    /// immediately, the last on the 250 ms flush).
    #[test]
    fn burst_of_deltas_is_coalesced() {
        let mut co = Coalescer::new(all_opts());
        let t0 = Instant::now();
        let mut emitted = Vec::new();
        for i in 0..40u64 {
            let now = t0 + Duration::from_micros(i * 2_500); // 40 in 100 ms
            emitted.extend(co.offer(part("s1", 0, i + 1), now));
            emitted.extend(co.flush_due(now));
        }
        emitted.extend(co.flush_due(t0 + Duration::from_millis(300)));
        assert!(
            emitted.len() <= 2,
            "expected <= 2 coalesced events, got {}",
            emitted.len()
        );
        assert_eq!(emitted[0].seq, 1, "first delta goes out immediately");
        assert_eq!(
            emitted.last().unwrap().seq,
            40,
            "the flush carries the latest snapshot"
        );
        assert!(co.next_deadline().is_none());
        // Distinct parts are independent keys.
        let mut co = Coalescer::new(all_opts());
        let a = co.offer(part("s1", 0, 1), t0);
        let b = co.offer(part("s1", 1, 2), t0);
        let c = co.offer(part("s2", 0, 3), t0);
        assert_eq!(a.len() + b.len() + c.len(), 3);
    }

    #[test]
    fn passthrough_and_drop_lists() {
        let mut co = Coalescer::new(all_opts());
        let now = Instant::now();
        for kind in [
            "session.created",
            "message.updated",
            "turn.end",
            "permission.asked",
            "task.started",
            "agent.list.changed",
            "browser.closed",
            "remote.status",
            "remote.device.changed",
        ] {
            assert_eq!(co.offer(Event::new(kind, "", ""), now).len(), 1, "{kind}");
        }
        for kind in [
            "debug.log",
            "pty.exited",
            "remote.pair.request",
            "unknown.kind",
        ] {
            assert!(co.offer(Event::new(kind, "", ""), now).is_empty(), "{kind}");
        }
    }

    #[test]
    fn browser_frames_need_interest_and_config() {
        let now = Instant::now();
        let frame = |seq: u64| {
            let mut e = Event::new("browser.frame", "", "s1");
            e.seq = seq;
            e
        };
        let mut co = Coalescer::new(FanoutOptions {
            interest_browser: false,
            publish_browser_frames: true,
            publish_processes: true,
        });
        assert!(co.offer(frame(1), now).is_empty(), "no interest -> never");
        let mut co = Coalescer::new(FanoutOptions {
            interest_browser: true,
            publish_browser_frames: false,
            publish_processes: true,
        });
        assert!(co.offer(frame(1), now).is_empty(), "config off -> never");
        let mut co = Coalescer::new(all_opts());
        assert_eq!(co.offer(frame(1), now).len(), 1);
        assert!(
            co.offer(frame(2), now + Duration::from_millis(500))
                .is_empty()
        );
        assert!(
            co.offer(frame(3), now + Duration::from_millis(900))
                .is_empty()
        );
        let flushed = co.flush_due(now + Duration::from_millis(1_001));
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].seq, 3, "latest frame wins");
    }

    #[test]
    fn process_output_is_tailed_and_throttled() {
        let now = Instant::now();
        let out = |seq: u64, chunk: &str| {
            let mut e = Event::new("process.output", "", "s1")
                .with_properties(serde_json::json!({ "id": "p1", "chunk": chunk }));
            e.seq = seq;
            e
        };
        let mut co = Coalescer::new(all_opts());
        let first = co.offer(out(1, "l0\n"), now);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].properties["chunk"], "l0\n");
        // 30 more lines within the second: nothing until the flush, and the
        // flush carries only the last 20 lines.
        for i in 1..=30u64 {
            assert!(
                co.offer(
                    out(i + 1, &format!("l{i}\n")),
                    now + Duration::from_millis(i)
                )
                .is_empty()
            );
        }
        assert!(co.flush_due(now + Duration::from_millis(999)).is_empty());
        let flushed = co.flush_due(now + Duration::from_millis(1_000));
        assert_eq!(flushed.len(), 1);
        let chunk = flushed[0].properties["chunk"].as_str().unwrap();
        assert_eq!(chunk.lines().count(), PROCESS_TAIL_LINES);
        assert!(chunk.starts_with("l11\n"));
        assert!(chunk.ends_with("l30\n"));
        assert_eq!(flushed[0].properties["coalesced"], true);
        // `process.exited` flushes pending output first.
        assert!(
            co.offer(out(40, "bye\n"), now + Duration::from_millis(1_100))
                .is_empty()
        );
        let mut exited = Event::new("process.exited", "", "s1")
            .with_properties(serde_json::json!({ "id": "p1", "code": 0 }));
        exited.seq = 41;
        let both = co.offer(exited, now + Duration::from_millis(1_200));
        assert_eq!(both.len(), 2);
        assert_eq!(both[0].kind, "process.output");
        assert_eq!(both[0].properties["chunk"], "bye\n");
        assert_eq!(both[1].kind, "process.exited");
        // publish.processes=false drops both kinds.
        let mut co = Coalescer::new(FanoutOptions {
            publish_processes: false,
            ..all_opts()
        });
        assert!(co.offer(out(1, "x"), now).is_empty());
        assert!(
            co.offer(Event::new("process.exited", "", ""), now)
                .is_empty()
        );
    }

    #[test]
    fn config_changed_is_stripped_of_secrets() {
        let mut co = Coalescer::new(all_opts());
        let ev = Event::new("config.changed", "", "").with_properties(serde_json::json!({
            "model": "zai/x",
            "api_key": "sk-1",
            "providers": [{ "name": "openai", "apiKey": "sk-2", "accessToken": "t", "endpoint": "e" }],
            "nested": { "clientSecret": "s", "keep": 1 },
            "password": "p"
        }));
        let out = co.offer(ev, Instant::now());
        assert_eq!(out.len(), 1);
        let p = &out[0].properties;
        assert_eq!(p["model"], "zai/x");
        assert!(p.get("api_key").is_none());
        assert!(p.get("password").is_none());
        assert!(p["providers"][0].get("apiKey").is_none());
        assert!(p["providers"][0].get("accessToken").is_none());
        assert_eq!(p["providers"][0]["endpoint"], "e");
        assert!(p["nested"].get("clientSecret").is_none());
        assert_eq!(p["nested"]["keep"], 1);
    }

    #[test]
    fn tail_lines_keeps_the_last_n() {
        assert_eq!(tail_lines("a\nb\nc\n", 2), "b\nc\n");
        assert_eq!(tail_lines("a\nb\nc", 2), "b\nc");
        assert_eq!(tail_lines("a\nb\n", 5), "a\nb\n");
        assert_eq!(tail_lines("", 5), "");
    }
}
