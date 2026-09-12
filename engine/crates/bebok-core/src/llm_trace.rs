//! Last-N LLM call trace (memory-only ring, never flushed to disk).
//!
//! Stores the last 2 complete request/response JSON payloads for inspection
//! in the Debug tab. The ring is a global static so `turn.rs` (bebok-core)
//! can push without threading `AppState` through the call stack.
//!
//! The stored payloads are the raw wire bodies — full system prompts, user
//! messages, tool arguments and tool output. They stay in this process:
//! `GET /debug/log` serves [`LlmTrace::list_redacted`], which keeps the shape
//! (message count, roles, part/tool kinds, usage counters, timings) and
//! replaces every free-form string with a `<redacted: N chars>` marker (F0-6).
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
    ///   usage: { input_tokens, output_tokens, … } }`,
    /// `{ model, error: "…" }` on failure, or `{ pending: true }` while the
    /// call is still streaming.
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

    /// Update the response of an in-flight call (no-op when already evicted).
    pub fn complete(&self, id: u64, response: serde_json::Value) {
        let mut q = self.inner.write().unwrap();
        if let Some(call) = q.iter_mut().find(|c| c.id == id) {
            call.response = response;
        }
    }

    /// Snapshot of all stored calls (newest last).
    pub fn list(&self) -> Vec<LlmCall> {
        self.inner.read().unwrap().iter().cloned().collect()
    }

    /// Snapshot with every free-form payload replaced by a length marker
    /// (F0-6). This is what `GET /debug/log` serves: the trace keeps full
    /// system prompts, user messages and tool output in memory, which must
    /// never leave the process verbatim.
    pub fn list_redacted(&self) -> Vec<LlmCall> {
        self.list()
            .into_iter()
            .map(|call| LlmCall {
                request: redact(&call.request),
                response: redact(&call.response),
                ..call
            })
            .collect()
    }

    /// Clear the ring (called from DELETE /debug/log).
    pub fn clear(&self) {
        self.inner.write().unwrap().clear();
    }
}

/// String fields that describe the SHAPE of a call rather than its content,
/// and are therefore safe to keep verbatim in a redacted trace: model ids,
/// message roles, part/tool kinds, tool + call ids, stop reasons. Everything
/// else that is a string is content (prompts, messages, tool arguments and
/// results) and is replaced by `<redacted: N chars>`.
const STRUCTURAL_KEYS: &[&str] = &[
    "model",
    "role",
    "type",
    "kind",
    "name",
    "id",
    "tool_call_id",
    "toolCallId",
    "tool_use_id",
    "callID",
    "finish_reason",
    "finishReason",
    "stop_reason",
    "stopReason",
    "status",
    "state",
    "provider",
    "providerID",
    "index",
];

/// Free-form error text is useful for diagnostics but may quote the payload,
/// so it is kept only up to this many characters.
const ERROR_MAX_CHARS: usize = 200;

/// Character-safe truncation with an explicit marker.
pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars).collect();
    format!("{head}… <truncated, {} chars total>", s.chars().count())
}

/// Recursively redact a traced payload: numbers, booleans, nulls, object keys
/// and structural strings survive; every other string becomes a length marker.
/// Structure (message count, part kinds, tool names, usage counters) is what
/// makes the trace useful, and none of it is user content.
pub fn redact(value: &serde_json::Value) -> serde_json::Value {
    redact_at(value, None)
}

fn redact_at(value: &serde_json::Value, key: Option<&str>) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(s) => match key {
            Some(k) if STRUCTURAL_KEYS.contains(&k) => Value::String(s.clone()),
            Some("error") => Value::String(truncate(s, ERROR_MAX_CHARS)),
            _ => Value::String(format!("<redacted: {} chars>", s.chars().count())),
        },
        Value::Array(items) => Value::Array(items.iter().map(|v| redact_at(v, key)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), redact_at(v, Some(k.as_str()))))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Convenience: push to the global trace. Call from the agent loop.
pub fn push_llm_call(call: LlmCall) {
    LLM_TRACE.push(call);
}

/// Begin a call: visible in `GET /debug/log` immediately with
/// `response = { pending: true }`, so the in-flight (last) request is never
/// missing while streaming. Returns the allocated call id — pass it to
/// [`complete_llm_call`] when the stream finishes or fails.
pub fn begin_llm_call(model: String, request: serde_json::Value) -> u64 {
    let id = LLM_TRACE.next_id();
    LLM_TRACE.push(LlmCall {
        id,
        ts: crate::util::now_ms(),
        model,
        request,
        response: serde_json::json!({ "pending": true }),
    });
    id
}

/// Finish a call started with [`begin_llm_call`].
pub fn complete_llm_call(id: u64, response: serde_json::Value) {
    LLM_TRACE.complete(id, response);
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
    fn complete_updates_pending_in_place() {
        let trace = LlmTrace::new(2);
        trace.push(LlmCall {
            id: 7,
            ts: 1000,
            model: "m".to_string(),
            request: serde_json::json!({}),
            response: serde_json::json!({ "pending": true }),
        });
        trace.complete(7, serde_json::json!({ "ok": true }));
        let calls = trace.list();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].response, serde_json::json!({ "ok": true }));
    }

    #[test]
    fn redact_keeps_shape_and_hides_content() {
        let request = serde_json::json!({
            "model": "claude-x",
            "temperature": 0.2,
            "stream": true,
            "system": "You are Bebok. SECRET-SYSTEM-PROMPT",
            "messages": [
                { "role": "user", "content": "my password is hunter2" },
                {
                    "role": "assistant",
                    "content": [
                        { "type": "text", "text": "here you go" },
                        {
                            "type": "tool_use",
                            "name": "read_file",
                            "id": "call_1",
                            "input": { "path": "C:/secrets/id_rsa" }
                        }
                    ]
                }
            ]
        });
        let red = redact(&request);

        // Structure survives.
        assert_eq!(red["model"], "claude-x");
        assert_eq!(red["temperature"], 0.2);
        assert_eq!(red["stream"], true);
        assert_eq!(red["messages"][0]["role"], "user");
        assert_eq!(red["messages"][1]["content"][1]["type"], "tool_use");
        assert_eq!(red["messages"][1]["content"][1]["name"], "read_file");
        assert_eq!(red["messages"][1]["content"][1]["id"], "call_1");

        // Content does not.
        let flat = red.to_string();
        for leak in ["SECRET-SYSTEM-PROMPT", "hunter2", "here you go", "id_rsa"] {
            assert!(
                !flat.contains(leak),
                "{leak} leaked through redaction: {flat}"
            );
        }
        assert_eq!(red["messages"][0]["content"], "<redacted: 22 chars>");
    }

    #[test]
    fn redact_truncates_errors_and_keeps_usage() {
        let response = serde_json::json!({
            "model": "m",
            "usage": { "input_tokens": 1200, "output_tokens": 34 },
            "error": "short boom",
        });
        let red = redact(&response);
        assert_eq!(red["usage"]["input_tokens"], 1200);
        assert_eq!(red["error"], "short boom");

        let long = "x".repeat(500);
        let red = redact(&serde_json::json!({ "error": long }));
        let text = red["error"].as_str().unwrap();
        assert!(text.contains("truncated, 500 chars total"));
        assert!(text.chars().count() < 260);
    }

    #[test]
    fn list_redacted_leaves_metadata_intact() {
        let trace = LlmTrace::new(2);
        trace.push(LlmCall {
            id: 5,
            ts: 4242,
            model: "m".to_string(),
            request: serde_json::json!({ "messages": [{ "role": "user", "content": "hi" }] }),
            response: serde_json::json!({ "pending": true }),
        });
        let calls = trace.list_redacted();
        assert_eq!(calls[0].id, 5);
        assert_eq!(calls[0].ts, 4242);
        assert_eq!(calls[0].model, "m");
        assert_eq!(calls[0].response, serde_json::json!({ "pending": true }));
        assert_eq!(
            calls[0].request["messages"][0]["content"],
            "<redacted: 2 chars>"
        );
        // The ring itself still holds the unredacted payload.
        assert_eq!(trace.list()[0].request["messages"][0]["content"], "hi");
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
