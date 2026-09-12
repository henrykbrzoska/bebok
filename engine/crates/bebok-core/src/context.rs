//! Context management (SPEC §3.9): token estimation, pruning and the
//! deterministic compaction summary. Pruning replaces old tool outputs with a
//! compact digest (non-destructive: full history stays on disk); compaction
//! produces a `[summary of messages 0..N]` marker for the internal fork.

use bebok_llm::ChatMessage;

use crate::session::{Message, Part, Role, ToolState};

/// Rough token estimate (chars / 4). Good enough for budget gating.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// Flat token estimate per attached image (all providers bill images as a
/// fixed-ish block; 1000 is a conservative middle ground).
pub const IMAGE_TOKENS_PER_IMAGE: usize = 1000;

/// Estimate the token cost of one persisted message.
pub fn estimate_message(m: &Message) -> usize {
    let mut total = estimate_tokens(&m.text_content());
    // Flat per-image estimate so context budgets account for multimodal input.
    total += m.image_parts().len() * IMAGE_TOKENS_PER_IMAGE;
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
        for p in &m.content_parts {
            match p {
                bebok_llm::ContentPart::Image { .. } => total += IMAGE_TOKENS_PER_IMAGE,
                bebok_llm::ContentPart::Text { text } => total += estimate_tokens(text),
            }
        }
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

/// Produce a compact digest of a tool output, preserving the most informative
/// parts (last lines, error messages, key data) in ~100-200 tokens. This is
/// used instead of bare `[truncated]` so the model retains semantic context
/// about what the tool returned.
pub fn summarize_tool_output(_tool_name: &str, output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let line_count = lines.len();

    // Error detection: if the output looks like an error, say so.
    let is_error = output.contains("error:")
        || output.contains("Error:")
        || output.contains("FAILED")
        || output.contains("panic:")
        || output.contains("Traceback");

    let prefix = if is_error {
        "[error output]"
    } else {
        "[summarized]"
    };

    if line_count <= 5 {
        // Short enough to include in full.
        return format!("{prefix} {output}");
    }

    // For multi-line output: keep the first 3 lines (often a header/command
    // echo) and the last 5 lines (often the actual result), with a note
    // about what was dropped.
    let head: Vec<&str> = lines[..3.min(line_count)].to_vec();
    let tail_start = line_count.saturating_sub(5);
    let tail: Vec<&str> = lines[tail_start..].to_vec();
    let dropped = line_count - head.len() - tail.len();

    let mut summary = format!("{prefix} ({line_count} lines, {dropped} dropped):\n");
    for line in &head {
        summary.push_str(line);
        summary.push('\n');
    }
    if dropped > 0 {
        summary.push_str(&format!("  ... ({dropped} lines omitted) ...\n"));
    }
    for line in &tail {
        summary.push_str(line);
        summary.push('\n');
    }
    // Remove trailing newline.
    summary.trim_end().to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_tool_output_short() {
        let out = summarize_tool_output("bash", "hello world");
        assert!(out.starts_with("[summarized]"));
        assert!(out.contains("hello world"));
    }

    #[test]
    fn summarize_tool_output_error() {
        let out = summarize_tool_output("bash", "Error: something went wrong");
        assert!(out.starts_with("[error output]"));
    }

    #[test]
    fn summarize_tool_output_long() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        let output = lines.join("\n");
        let summary = summarize_tool_output("bash", &output);
        assert!(summary.contains("100 lines"));
        assert!(summary.contains("line 0"));
        // Should contain last 5 lines.
        assert!(summary.contains("line 99"));
        assert!(summary.contains("line 95"));
    }

    #[test]
    fn estimate_chat_charges_images_but_not_dropped_markers() {
        use bebok_llm::{ChatMessage, ChatRole, ContentPart};
        let with_image = ChatMessage {
            role: ChatRole::User,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            content_parts: vec![ContentPart::Image {
                media_type: "image/png".to_string(),
                data: "aGVsbG8=".to_string(),
            }],
        };
        let with_marker = ChatMessage {
            content_parts: vec![ContentPart::Text {
                text: "[image omitted to fit the context budget: image/png]".to_string(),
            }],
            ..with_image.clone()
        };
        assert_eq!(
            estimate_chat(std::slice::from_ref(&with_image), ""),
            IMAGE_TOKENS_PER_IMAGE
        );
        assert!(estimate_chat(&[with_marker], "") < IMAGE_TOKENS_PER_IMAGE);
        let _ = ChatRole::User;
    }
}
