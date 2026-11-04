//! Render a [`bebok_llm::ChatRequest`] into the single prompt string a CLI
//! takes.
//!
//! An agentic CLI is driven with one `-p <prompt>` argument and owns its own
//! conversation state, so Bebok's message list has to be flattened into plain
//! text on every turn. The format is intentionally boring — no ANSI, no JSON
//! envelope, no tool schema — because the CLI reads it as a human instruction:
//!
//! ```text
//! ## User
//!
//! what does this repo do?
//!
//! ## Assistant
//!
//! Let me look at the README.
//! Tool call: read_file({"path":"README.md"})
//! Tool result: # Bebok …
//!
//! summarize package.json
//! ```
//!
//! Prior tool calls and results are rendered as plain text. A CLI runs its own
//! tool loop, so replaying Bebok's registry calls as structured data would only
//! invite it to answer them again.
//!
//! **Images are dropped in v1.** A CLI that takes `-p` has no side channel for
//! a binary payload, so [`ContentPart::Image`] becomes a visible placeholder
//! rather than silently vanishing — the model then knows something was attached
//! and can say so.

use bebok_llm::{ChatMessage, ChatRequest, ChatRole, ContentPart};

/// Max characters of a tool-call input kept in the prompt.
const TOOL_INPUT_CAP: usize = 300;
/// Max characters of a tool result kept in the prompt.
const TOOL_RESULT_CAP: usize = 500;

/// Flatten the request's conversation into one prompt.
///
/// Every message except the last is prefixed with a `## User` / `## Assistant`
/// marker; the last message is the user's current request and is emitted last
/// with no marker at all, so a CLI reading the whole blob sees the instruction
/// at the end where it expects it.
pub fn render_prompt(req: &ChatRequest) -> String {
    let Some((last, prior)) = req.messages.split_last() else {
        // No messages: the caller passed an empty conversation. Emitting ""
        // lets the provider report a clear config error instead of sending a
        // prompt that is only scaffolding.
        return String::new();
    };

    let mut blocks: Vec<String> = Vec::new();
    for msg in prior {
        let body = render_message(msg);
        if body.trim().is_empty() {
            // An empty message carries no information; its marker would only
            // add noise a CLI might mistake for a real turn.
            continue;
        }
        blocks.push(format!("## {}\n\n{}", heading(msg.role), body));
    }

    let current = render_message(last);
    if !current.trim().is_empty() {
        blocks.push(current);
    }
    blocks.join("\n\n")
}

fn heading(role: ChatRole) -> &'static str {
    match role {
        ChatRole::User => "User",
        ChatRole::Assistant => "Assistant",
    }
}

/// One message body: text, then content parts, then tool calls, then results.
fn render_message(msg: &ChatMessage) -> String {
    let mut lines: Vec<String> = Vec::new();
    if !msg.content.trim().is_empty() {
        lines.push(msg.content.trim_end().to_string());
    }
    for part in &msg.content_parts {
        match part {
            ContentPart::Text { text } => {
                if !text.trim().is_empty() {
                    lines.push(text.trim_end().to_string());
                }
            }
            // v1 CLI providers get no images: the CLI takes a single text
            // argument, so the placeholder is the honest rendering.
            ContentPart::Image { media_type, .. } => {
                lines.push(format!("[image omitted: {media_type}]"));
            }
        }
    }
    for call in &msg.tool_calls {
        // `Value::to_string` is already compact JSON and cannot fail, which is
        // why the input is not re-serialized through a `serde_json::Error` path.
        let input = truncate(&call.input.to_string(), TOOL_INPUT_CAP);
        lines.push(format!("Tool call: {}({input})", call.name));
    }
    for result in &msg.tool_results {
        let content = truncate(&result.content, TOOL_RESULT_CAP);
        if result.is_error {
            lines.push(format!("Tool result (error): {content}"));
        } else {
            lines.push(format!("Tool result: {content}"));
        }
    }
    lines.join("\n")
}

/// Truncate to at most `max` characters, appending `…` when anything was cut.
///
/// Counts characters, not bytes: slicing at a byte offset would panic inside a
/// multi-byte sequence, and a prompt full of mojibake would be worse than a
/// shorter one.
fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_string(),
        Some((cut, _)) => {
            let mut out = String::with_capacity(cut + '…'.len_utf8());
            out.push_str(&text[..cut]);
            out.push('…');
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bebok_llm::{Sampling, Thinking, ToolCall, ToolResult};
    use serde_json::json;

    fn request(messages: Vec<ChatMessage>) -> ChatRequest {
        ChatRequest {
            model: "claude".into(),
            system: String::new(),
            messages,
            tools: Vec::new(),
            max_tokens: 1024,
            thinking: Thinking::Off,
            sampling: Sampling::default(),
        }
    }

    fn assistant(content: &str) -> ChatMessage {
        ChatMessage {
            role: ChatRole::Assistant,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            content_parts: Vec::new(),
        }
    }

    fn tool_message(content: &str) -> ChatMessage {
        ChatMessage {
            role: ChatRole::User,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_results: vec![ToolResult {
                tool_use_id: "t1".into(),
                content: content.into(),
                is_error: false,
            }],
            content_parts: Vec::new(),
        }
    }

    #[test]
    fn single_user_message_is_emitted_plainly() {
        let out = render_prompt(&request(vec![ChatMessage::user("hello there")]));
        assert_eq!(out, "hello there");
    }

    #[test]
    fn empty_conversation_renders_nothing() {
        assert_eq!(render_prompt(&request(Vec::new())), "");
    }

    #[test]
    fn prior_turns_are_headed_and_the_request_comes_last() {
        let messages = vec![
            ChatMessage::user("what is this repo?"),
            assistant("Let me look at the README."),
            ChatMessage::user("now summarize package.json"),
        ];
        let out = render_prompt(&request(messages));
        let expected = "## User\n\nwhat is this repo?\n\n\
                        ## Assistant\n\nLet me look at the README.\n\n\
                        now summarize package.json";
        assert_eq!(out, expected);
        assert!(out.ends_with("now summarize package.json"), "{out}");
        assert_eq!(out.matches("now summarize package.json").count(), 1);
    }

    #[test]
    fn tool_calls_and_results_render_as_plain_lines_in_order() {
        let messages = vec![
            ChatMessage::user("read the readme"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: "Reading it.".into(),
                tool_calls: vec![ToolCall {
                    id: "call-1".into(),
                    name: "read_file".into(),
                    input: json!({ "path": "README.md" }),
                }],
                tool_results: Vec::new(),
                content_parts: Vec::new(),
            },
            tool_message("# Bebok\n\nA coding agent."),
            ChatMessage::user("thanks, now the licence"),
        ];
        let out = render_prompt(&request(messages));
        let expected = "## User\n\nread the readme\n\n\
                        ## Assistant\n\nReading it.\n\
                        Tool call: read_file({\"path\":\"README.md\"})\n\n\
                        ## User\n\nTool result: # Bebok\n\nA coding agent.\n\n\
                        thanks, now the licence";
        assert_eq!(out, expected);
    }

    #[test]
    fn error_tool_results_are_marked() {
        let messages = vec![ChatMessage {
            role: ChatRole::User,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_results: vec![ToolResult {
                tool_use_id: "t1".into(),
                content: "no such file".into(),
                is_error: true,
            }],
            content_parts: Vec::new(),
        }];
        assert_eq!(
            render_prompt(&request(messages)),
            "Tool result (error): no such file"
        );
    }

    #[test]
    fn images_become_a_placeholder_and_text_parts_are_kept() {
        let mut message = ChatMessage::user("look at this");
        message.content_parts = vec![
            ContentPart::Text {
                text: "extra context".into(),
            },
            ContentPart::Image {
                media_type: "image/png".into(),
                data: "AAAA".into(),
            },
        ];
        let out = render_prompt(&request(vec![message]));
        assert_eq!(
            out,
            "look at this\nextra context\n[image omitted: image/png]"
        );
        // The base64 payload must never reach the prompt.
        assert!(!out.contains("AAAA"));
    }

    #[test]
    fn empty_prior_messages_are_skipped() {
        let messages = vec![
            ChatMessage::user("   "),
            assistant(""),
            ChatMessage::user("only this"),
        ];
        assert_eq!(render_prompt(&request(messages)), "only this");
    }

    #[test]
    fn truncation_is_utf8_safe_and_caps_long_tool_payloads() {
        // Multi-byte characters only: a byte-offset slice at 300 would panic.
        let long_error: String = "ą".repeat(700);
        let messages = vec![ChatMessage {
            role: ChatRole::User,
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "c".into(),
                name: "bash".into(),
                input: json!({ "command": "x".repeat(400) }),
            }],
            tool_results: vec![ToolResult {
                tool_use_id: "c".into(),
                content: long_error,
                is_error: true,
            }],
            content_parts: Vec::new(),
        }];
        let out = render_prompt(&request(messages));
        let call_line = out
            .lines()
            .find(|l| l.starts_with("Tool call: bash("))
            .expect("tool call line");
        let result_line = out
            .lines()
            .find(|l| l.starts_with("Tool result (error): "))
            .expect("tool result line");

        let call_payload = call_line
            .trim_start_matches("Tool call: bash(")
            .trim_end_matches(')');
        assert!(call_payload.ends_with('…'), "{call_payload}");
        assert_eq!(call_payload.chars().count(), TOOL_INPUT_CAP + 1);

        let result_payload = result_line.trim_start_matches("Tool result (error): ");
        assert!(result_payload.ends_with('…'), "{result_payload}");
        assert_eq!(result_payload.chars().count(), TOOL_RESULT_CAP + 1);
    }

    #[test]
    fn short_payloads_are_not_truncated() {
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("", 0), "");
        assert_eq!(truncate("ąę", 5), "ąę");
    }
}
