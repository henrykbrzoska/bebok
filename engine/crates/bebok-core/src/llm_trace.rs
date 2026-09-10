//! Last-N LLM call trace (memory-only ring, never flushed to disk).
//!
/// Stores the last 2 complete request/response JSON payloads for inspection
/// in the Debug tab. The ring is a global static so `turn.rs` (bebok-core)
/// can push without threading `AppState` through the call stack.
use std::collections::VecDeque;
use std::sync::{Arc, LazyLock, RwLock};

use serde::Serialize;

/// Global trace singleton — push from the agent loop, read from the debug
/// route. `Arc` inside so every clone refers to the same ring.
pub static LLM_TRACE: LazyLock<Arc<LlmTrace>> = LazyLock::new(|| Arc::new(LlmTrace::new(2)));

/// One captured LLM call: request body + response summary (or error).
#[derive(Debug, Clone, Serialize)]
pub struct LlmCall {
    /// Monotonically increasing call id (1, 2, 3, …).
    pub id: u64,
    /// Unix ms timestamp.
    pub ts: i64,
    /// Model string sent to the provider.
    pub model: String,
    /// Full wire request body (ChatRequest serialized as JSON).
    pub request: serde_json::Value,
    /// Response summary: `{ model, message: <assembled Message JSON>,
    ///   usage: { input_tokens, output_tokens, … } }` or
    /// `{ model, error: "…" }` on failure.
    pub response: serde_json::Value,
}

/// Thread-safe ring buffer holding the last `capacity` LLM calls.
#[derive(Debug)]
pub struct LlmTrace {
    inner: RwLock<VecDeque<LlmCall>>,
    capacity: usize,
    counter: std::sync::atomic::AtomicU64,
}

impl LlmTrace {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: RwLock::new(VecDeque::with_capacity(capacity)),
            capacity,
            counter: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Atomically allocate the next monotonically increasing call id.
    pub fn next_id(&self) -> u64 {
        self.counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Append a call, evicting the oldest when at capacity.
    pub fn push(&self, call: LlmCall) {
        let mut q = self.inner.write().unwrap();
        if q.len() >= self.capacity {
            q.pop_front();
        }
        q.push_back(call);
    }

    /// Snapshot of all stored calls (newest last).
    pub fn list(&self) -> Vec<LlmCall> {
        self.inner.read().unwrap().iter().cloned().collect()
    }

    /// Clear the ring (called from DELETE /debug/log).
    pub fn clear(&self) {
        self.inner.write().unwrap().clear();
    }
}

/// Convenience: push to the global trace. Call from the agent loop.
pub fn push_llm_call(call: LlmCall) {
    LLM_TRACE.push(call);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_call(id: u64) -> LlmCall {
        LlmCall {
            id,
            ts: 1000 + id as i64,
            model: format!("model-{id}"),
            request: serde_json::json!({"seq": id}),
            response: serde_json::json!({"ok": true}),
        }
    }

    #[test]
    fn ring_cap_2() {
        let trace = LlmTrace::new(2);
        trace.push(dummy_call(1));
        trace.push(dummy_call(2));
        trace.push(dummy_call(3));

        let calls = trace.list();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, 2);
        assert_eq!(calls[1].id, 3);
    }

    #[test]
    fn clear_empties_ring() {
        let trace = LlmTrace::new(2);
        trace.push(dummy_call(1));
        trace.clear();
        assert!(trace.list().is_empty());
    }

    #[test]
    fn next_id_is_sequential() {
        let trace = LlmTrace::new(2);
        let a = trace.next_id();
        let b = trace.next_id();
        let c = trace.next_id();
        assert_eq!(a, 1);
        assert_eq!(b, 2);
        assert_eq!(c, 3);
    }

    #[test]
    fn global_trace_works() {
        LLM_TRACE.clear();
        push_llm_call(dummy_call(99));
        let calls = LLM_TRACE.list();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, 99);
        LLM_TRACE.clear();
    }
}
