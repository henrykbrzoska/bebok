//! Deterministic mock provider for end-to-end tests (WP-M1, F10-5).
//!
//! With `BEBOK_PROVIDER_MOCK=1` every provider the factory would build
//! resolves to [`MockProvider`] instead — no network, no API key:
//!
//! - reply: `Mock reply to: <last user text>` streamed as 4 text chunks
//!   50 ms apart, then `Done` with fixed usage (`10` in / `5` out);
//! - if the last user text contains `[write]`, the first call of that turn
//!   emits a `write_file` tool call for `mock-<n>.txt` (so a permission
//!   prompt appears); the follow-up call (after the tool result) streams the
//!   reply.
//!
//! Logged loudly once, like `BEBOK_NO_AUTH`.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt as _;
use futures::stream::BoxStream;

use crate::provider::{
    ChatRequest, ChatRole, Provider, StreamEvent, StreamResult, ToolCall, Usage,
};

/// Delay between the reply chunks.
pub const CHUNK_DELAY: Duration = Duration::from_millis(50);
pub const CHUNKS: usize = 4;
/// Marker in the user text that triggers a `write_file` tool call first.
pub const WRITE_MARKER: &str = "[write]";

static ENABLED: LazyLock<bool> = LazyLock::new(|| {
    let on = matches!(
        std::env::var("BEBOK_PROVIDER_MOCK")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    );
    if on {
        tracing::warn!(
            "BEBOK_PROVIDER_MOCK=1: every LLM provider is replaced by the deterministic \
             mock; no request reaches a real model"
        );
    }
    on
});

/// True when `BEBOK_PROVIDER_MOCK` is set (read once per process).
pub fn enabled() -> bool {
    *ENABLED
}

#[derive(Debug, Default)]
pub struct MockProvider {
    /// Counts `write_file` calls -> `mock-<n>.txt`.
    writes: AtomicUsize,
}

impl MockProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// The last user message with text (tool-result-only turns are skipped).
    pub fn last_user_text(req: &ChatRequest) -> String {
        req.messages
            .iter()
            .rev()
            .find(|m| m.role == ChatRole::User && !m.content.trim().is_empty())
            .map(|m| m.content.trim().to_string())
            .unwrap_or_default()
    }

    /// True when the request ends with tool results (we already issued the
    /// tool call for this turn).
    fn after_tool_result(req: &ChatRequest) -> bool {
        req.messages
            .last()
            .is_some_and(|m| m.role == ChatRole::User && !m.tool_results.is_empty())
    }

    /// Split `text` into `CHUNKS` pieces on char boundaries (the last chunk
    /// takes the remainder; empty chunks are emitted for very short text so
    /// the shape is always 4 + Done).
    pub fn chunks(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        let per = chars.len().div_ceil(CHUNKS).max(1);
        let mut out: Vec<String> = chars.chunks(per).map(|c| c.iter().collect()).collect();
        while out.len() < CHUNKS {
            out.push(String::new());
        }
        out.truncate(CHUNKS);
        out
    }

    fn usage() -> Usage {
        Usage {
            input_tokens: 10,
            output_tokens: 5,
            cost: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        }
    }

    /// The events for one `stream` call (pure; the delays are added when
    /// the stream is polled).
    pub fn script(&self, req: &ChatRequest) -> Vec<StreamEvent> {
        let last = Self::last_user_text(req);
        if last.contains(WRITE_MARKER) && !Self::after_tool_result(req) {
            let n = self.writes.fetch_add(1, Ordering::Relaxed) + 1;
            return vec![
                StreamEvent::ToolCall(ToolCall {
                    id: format!("mock-toolu-{n}"),
                    name: "write_file".to_string(),
                    input: serde_json::json!({
                        "path": format!("mock-{n}.txt"),
                        "content": format!("mock write #{n}\n"),
                    }),
                }),
                StreamEvent::Done(Self::usage()),
            ];
        }
        let mut events: Vec<StreamEvent> = Self::chunks(&format!("Mock reply to: {last}"))
            .into_iter()
            .map(StreamEvent::Text)
            .collect();
        events.push(StreamEvent::Done(Self::usage()));
        events
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn stream(
        &self,
        req: ChatRequest,
    ) -> StreamResult<BoxStream<'static, StreamResult<StreamEvent>>> {
        let events = self.script(&req);
        let stream =
            futures::stream::iter(events.into_iter().enumerate()).then(|(i, ev)| async move {
                // 50 ms between chunks (not before the first one, not before Done).
                if i > 0 && matches!(ev, StreamEvent::Text(_)) {
                    tokio::time::sleep(CHUNK_DELAY).await;
                }
                Ok(ev)
            });
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ChatMessage;

    fn req(messages: Vec<ChatMessage>) -> ChatRequest {
        ChatRequest {
            model: "mock/x".into(),
            system: String::new(),
            messages,
            tools: Vec::new(),
            max_tokens: 100,
            thinking: crate::Thinking::Off,
            session_id: None,
            directory: None,
        }
    }

    #[test]
    fn chunks_are_always_four() {
        assert_eq!(
            MockProvider::chunks("abcdefgh"),
            vec!["ab", "cd", "ef", "gh"]
        );
        assert_eq!(
            MockProvider::chunks("abcdefghi"),
            vec!["abc", "def", "ghi", ""]
        );
        assert_eq!(MockProvider::chunks("a"), vec!["a", "", "", ""]);
        assert_eq!(MockProvider::chunks("").len(), CHUNKS);
        assert_eq!(MockProvider::chunks("zażółć").concat(), "zażółć");
    }

    #[tokio::test]
    async fn plain_prompt_streams_four_chunks_then_done() {
        let p = MockProvider::new();
        let started = std::time::Instant::now();
        let mut stream = p
            .stream(req(vec![ChatMessage::user("hello there")]))
            .await
            .unwrap();
        let mut text = String::new();
        let mut n = 0;
        let mut done = None;
        while let Some(ev) = stream.next().await {
            match ev.unwrap() {
                StreamEvent::Text(t) => {
                    n += 1;
                    text.push_str(&t);
                }
                StreamEvent::Done(u) => done = Some(u),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(n, CHUNKS);
        assert_eq!(text, "Mock reply to: hello there");
        let usage = done.expect("Done");
        assert_eq!((usage.input_tokens, usage.output_tokens), (10, 5));
        assert!(
            started.elapsed() >= CHUNK_DELAY * 3,
            "chunks are spaced {CHUNK_DELAY:?} apart"
        );
        assert_eq!(p.name(), "mock");
    }

    #[test]
    fn write_marker_emits_a_tool_call_then_replies_after_the_result() {
        let p = MockProvider::new();
        let first = p.script(&req(vec![ChatMessage::user("please [write] a file")]));
        assert_eq!(first.len(), 2);
        match &first[0] {
            StreamEvent::ToolCall(call) => {
                assert_eq!(call.name, "write_file");
                assert_eq!(call.input["path"], "mock-1.txt");
                assert_eq!(call.id, "mock-toolu-1");
            }
            other => panic!("expected tool call, got {other:?}"),
        }
        assert!(matches!(first[1], StreamEvent::Done(_)));

        // The follow-up request carries the tool result: text reply now.
        let mut after_tool = ChatMessage::user("");
        after_tool.tool_results.push(crate::provider::ToolResult {
            tool_use_id: "mock-toolu-1".into(),
            content: "ok".into(),
            is_error: false,
        });
        let second = p.script(&req(vec![
            ChatMessage::user("please [write] a file"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: String::new(),
                tool_calls: vec![],
                tool_results: vec![],
                content_parts: vec![],
            },
            after_tool,
        ]));
        assert_eq!(second.len(), CHUNKS + 1);
        let text: String = second
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Mock reply to: please [write] a file");

        // A second `[write]` turn numbers its file mock-2.txt.
        let third = p.script(&req(vec![ChatMessage::user("[write] again")]));
        match &third[0] {
            StreamEvent::ToolCall(call) => assert_eq!(call.input["path"], "mock-2.txt"),
            other => panic!("expected tool call, got {other:?}"),
        }
    }

    #[test]
    fn last_user_text_skips_tool_result_only_messages() {
        let mut tr = ChatMessage::user("");
        tr.tool_results.push(crate::provider::ToolResult {
            tool_use_id: "x".into(),
            content: "y".into(),
            is_error: false,
        });
        let r = req(vec![ChatMessage::user("first"), tr]);
        assert_eq!(MockProvider::last_user_text(&r), "first");
        assert_eq!(MockProvider::last_user_text(&req(vec![])), "");
    }
}
