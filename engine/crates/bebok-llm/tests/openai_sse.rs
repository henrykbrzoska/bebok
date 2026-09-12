//! OpenAI SSE parser tests over recorded `data:` streams (bug B13).
//!
//! The invariant under test: **every** stream ends with exactly one
//! `StreamEvent::Done` carrying usage. The agent loop books a session's tokens
//! and cost only when it sees `Done` (`bebok-core/src/agent/turn.rs`), so a
//! stream that ends without one silently loses the accounting for that reply,
//! and a stream that emits two would double-count it.
//!
//! Fixtures in `tests/fixtures/*.sse` are the chunk sequences OpenAI-compatible
//! servers actually send, including the canonical
//! `finish_reason` -> `usage` (empty `choices`) -> `[DONE]` tail.

use bebok_llm::{StreamEvent, StreamResult, Usage, parse_openai_sse};

const TEXT_USAGE_DONE: &str = include_str!("fixtures/openai_text_usage_done.sse");
const TOOL_CALL_TRUNCATED: &str = include_str!("fixtures/openai_tool_call_truncated.sse");
const TOOL_CALL_USAGE_DONE: &str = include_str!("fixtures/openai_tool_call_usage_done.sse");
const NO_USAGE_DONE: &str = include_str!("fixtures/openai_no_usage_done.sse");
const REASONING: &str = include_str!("fixtures/openai_reasoning.sse");

/// Every chunk boundary a real network could produce, so a passing test means
/// the parser is not accidentally relying on chunk == SSE line.
const CHUNKINGS: [Option<usize>; 5] = [None, Some(1), Some(7), Some(64), Some(4096)];

fn events(body: &str, chunk: Option<usize>) -> Vec<StreamEvent> {
    parse_openai_sse(body, chunk)
        .into_iter()
        .map(|ev: StreamResult<StreamEvent>| ev.expect("no parse/stream error expected"))
        .collect()
}

fn dones(events: &[StreamEvent]) -> Vec<Usage> {
    events
        .iter()
        .filter_map(|ev| match ev {
            StreamEvent::Done(u) => Some(u.clone()),
            _ => None,
        })
        .collect()
}

fn text(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|ev| match ev {
            StreamEvent::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

fn tool_calls(events: &[StreamEvent]) -> Vec<(String, String, serde_json::Value)> {
    events
        .iter()
        .filter_map(|ev| match ev {
            StreamEvent::ToolCall(c) => Some((c.id.clone(), c.name.clone(), c.input.clone())),
            _ => None,
        })
        .collect()
}

/// `finish_reason` -> `usage` (empty `choices`) -> `[DONE]`: exactly one Done,
/// carrying the real token counts. This is the sequence that used to emit none.
#[test]
fn finish_then_usage_then_done_emits_exactly_one_done_with_usage() {
    for chunk in CHUNKINGS {
        let evs = events(TEXT_USAGE_DONE, chunk);
        let done = dones(&evs);
        assert_eq!(
            done.len(),
            1,
            "chunking {chunk:?}: expected exactly one Done"
        );
        assert_eq!(done[0].input_tokens, 1200);
        assert_eq!(done[0].output_tokens, 37);
        assert_eq!(text(&evs), "Hello world");
        // Done is last: nothing may follow the stream's terminator.
        assert!(matches!(evs.last(), Some(StreamEvent::Done(_))));
    }
}

/// A stream cut off right after a tool call (no usage chunk, no `[DONE]`) must
/// still deliver the tool call *and* exactly one Done, with best-effort
/// (zeroed) usage rather than no accounting event at all.
#[test]
fn stream_truncated_after_tool_call_still_emits_exactly_one_done() {
    for chunk in CHUNKINGS {
        let evs = events(TOOL_CALL_TRUNCATED, chunk);
        let calls = tool_calls(&evs);
        assert_eq!(calls.len(), 1, "chunking {chunk:?}");
        assert_eq!(calls[0].0, "call_abc123");
        assert_eq!(calls[0].1, "read_file");
        assert_eq!(calls[0].2, serde_json::json!({ "path": "src/main.rs" }));

        let done = dones(&evs);
        assert_eq!(
            done.len(),
            1,
            "chunking {chunk:?}: expected exactly one Done"
        );
        assert_eq!(done[0].input_tokens, 0);
        assert_eq!(done[0].output_tokens, 0);
        assert!(matches!(evs.last(), Some(StreamEvent::Done(_))));
    }
}

/// Tool calls followed by the canonical usage + `[DONE]` tail: both calls in
/// index order, one Done with the real usage.
#[test]
fn tool_calls_with_usage_tail_emit_one_done_after_the_calls() {
    for chunk in CHUNKINGS {
        let evs = events(TOOL_CALL_USAGE_DONE, chunk);
        let calls = tool_calls(&evs);
        assert_eq!(calls.len(), 2, "chunking {chunk:?}");
        assert_eq!(calls[0].0, "call_one");
        assert_eq!(calls[1].0, "call_two");
        assert_eq!(calls[1].2, serde_json::json!({ "path": "README.md" }));

        let done = dones(&evs);
        assert_eq!(done.len(), 1, "chunking {chunk:?}");
        assert_eq!(done[0].input_tokens, 842);
        assert_eq!(done[0].output_tokens, 64);
        assert!(matches!(evs.last(), Some(StreamEvent::Done(_))));
    }
}

/// A server that never reports usage (`stream_options` unsupported) still gets
/// exactly one Done, so the turn is finalised instead of ending silently.
#[test]
fn stream_without_usage_still_emits_exactly_one_done() {
    for chunk in CHUNKINGS {
        let evs = events(NO_USAGE_DONE, chunk);
        assert_eq!(text(&evs), "hi");
        let done = dones(&evs);
        assert_eq!(done.len(), 1, "chunking {chunk:?}");
        assert_eq!(done[0].input_tokens, 0);
        assert_eq!(done[0].output_tokens, 0);
    }
}

/// Reasoning deltas (DeepSeek-style `reasoning_content`) surface as Thinking,
/// and the usage tail still yields exactly one Done.
#[test]
fn reasoning_stream_emits_thinking_and_one_done() {
    let evs = events(REASONING, None);
    let thinking: String = evs
        .iter()
        .filter_map(|ev| match ev {
            StreamEvent::Thinking(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(thinking, "let me think");
    assert_eq!(text(&evs), "42");
    assert_eq!(dones(&evs).len(), 1);
}

/// CRLF line endings (some proxies rewrite them) must parse identically.
#[test]
fn crlf_line_endings_parse_identically() {
    let crlf = REASONING.replace('\n', "\r\n");
    let lf_events = events(REASONING, None);
    let crlf_events = events(&crlf, None);
    assert_eq!(text(&crlf_events), text(&lf_events));
    assert_eq!(dones(&crlf_events).len(), 1);
    assert_eq!(dones(&crlf_events)[0].input_tokens, 10);
}

/// A stream whose final line arrives without a trailing newline is still
/// parsed (the parser flushes its line buffer at EOF) and still finalised once.
#[test]
fn unterminated_final_line_is_flushed_and_done_once() {
    let body = TEXT_USAGE_DONE.trim_end_matches(['\n', '\r']);
    let evs = events(body, None);
    assert_eq!(dones(&evs).len(), 1);
    assert_eq!(dones(&evs)[0].input_tokens, 1200);
}

/// An empty body (connection dropped before any chunk) must not hang or emit
/// zero Done events: the turn still needs its single terminator.
#[test]
fn empty_stream_emits_exactly_one_done() {
    let evs = events("", None);
    assert_eq!(evs.len(), 1);
    assert!(matches!(evs[0], StreamEvent::Done(_)));
}
