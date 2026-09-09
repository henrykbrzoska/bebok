//! Context management (SPEC §3.9): token estimation, pruning and the
//! deterministic compaction summary. Pruning replaces old tool outputs with
//! `[truncated]` in the *request* only (full history stays on disk); compaction
//! produces a `[summary of messages 0..N]` marker for the internal fork.

use bebok_llm::ChatMessage;

use crate::session::{Message, Part, Role, ToolState};

/// Rough token estimate (chars / 4). Good enough for budget gating.
pub fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() + 3) / 4
}

/// Estimate the token cost of one persisted message.
pub fn estimate_message(m: &Message) -> usize {
    let mut total = estimate_tokens(&m.text_content());
    for p in &m.parts {
        if let Part::Tool { state, .. } = p {
            total += estimate_tokens(&serde_json::to_string(state.input()).unwrap_or_default());
            if let ToolState::Completed { output, .. } = state {
                total += estimate_tokens(output);
            }
        }
    }
    total
}

/// Estimate the token cost of a built chat request (system + messages).
pub fn estimate_chat(chat: &[ChatMessage], system: &str) -> usize {
    let mut total = estimate_tokens(system);
    for m in chat {
        total += estimate_tokens(&m.content);
        for tc in &m.tool_calls {
            total += estimate_tokens(&tc.name);
            total += estimate_tokens(&serde_json::to_string(&tc.input).unwrap_or_default());
        }
        for tr in &m.tool_results {
            total += estimate_tokens(&tr.content);
        }
    }
    total
}

/// A deterministic compaction summary for messages `0..end`: a `[summary of
/// messages 0..N]` marker followed by a digest of each message's text. (An LLM
/// summarization can replace this later without changing the fork mechanics.)
pub fn compact_summary(messages: &[Message], end: usize) -> String {
    let mut out = format!("[summary of messages 0..{}]\n", end.saturating_sub(1));
    for m in messages.iter().take(end) {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let text = m.text_content();
        let text = text.chars().take(400).collect::<String>();
        if text.is_empty() {
            out.push_str(&format!("{role}: (tool calls)\n"));
        } else {
            out.push_str(&format!("{role}: {text}\n"));
        }
    }
    out
}
