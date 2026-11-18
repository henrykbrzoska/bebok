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
//! **Images travel as blocks, when the CLI accepts them on stdin.**
//! [`render_blocks`] keeps each [`ContentPart::Image`] as a
//! [`PromptBlock::Image`] (the base64 payload, which the stdin stream-json
//! encoding carries as an `image` content block). [`render_prompt`] — used when
//! the prompt has to go on the command line — keeps a visible placeholder
//! instead, so the model still knows something was attached.

use bebok_llm::{ChatMessage, ChatRequest, ChatRole, ContentPart};

use crate::backend::PromptBlock;

/// Max characters of a tool-call input kept in the prompt.
const TOOL_INPUT_CAP: usize = 300;
/// Max characters of a tool result kept in the prompt.
const TOOL_RESULT_CAP: usize = 500;

/// Flatten the request's conversation into one prompt string, with each image
/// replaced by a placeholder. This is the form a CLI takes on its command line.
///
/// Every message except the last is prefixed with a `## User` / `## Assistant`
/// marker; the last message is the user's current request and is emitted last
/// with no marker at all, so a CLI reading the whole blob sees the instruction
/// at the end where it expects it.
pub fn render_prompt(req: &ChatRequest) -> String {
    render(req, false)
        .into_iter()
        .filter_map(|block| match block {
            PromptBlock::Text(text) => Some(text),
            PromptBlock::Image { .. } => None,
        })
        .collect()
}

/// The same conversation as structured blocks: text runs interleaved with
/// real image payloads. This is the form a CLI takes on stdin
/// (`--input-format stream-json`).
pub fn render_blocks(req: &ChatRequest) -> Vec<PromptBlock> {
    render(req, true)
}

/// Shared layout for both forms. `keep_images` decides whether an image
/// becomes a [`PromptBlock::Image`] or a visible placeholder line.
fn render(req: &ChatRequest, keep_images: bool) -> Vec<PromptBlock> {
    let Some((last, prior)) = req.messages.split_last() else {
        // No messages: the caller passed an empty conversation. Emitting
        // nothing lets the provider report a clear config error instead of
        // sending a prompt that is only scaffolding.
        return Vec::new();
    };

    let mut out: Vec<PromptBlock> = Vec::new();
    for msg in prior {
        let pieces = message_pieces(msg, keep_images);
        if pieces_are_blank(&pieces) {
            // An empty message carries no information; its marker would only
            // add noise a CLI might mistake for a real turn.
            continue;
        }
        separate(&mut out);
        append_text(&mut out, &format!("## {}\n\n", heading(msg.role)));
        push_pieces(&mut out, pieces);
    }

    let pieces = message_pieces(last, keep_images);
    if !pieces_are_blank(&pieces) {
        separate(&mut out);
        push_pieces(&mut out, pieces);
    }
    out
}

/// One piece of a message body before it is laid out into blocks.
enum Piece {
    Text(String),
    Image { media_type: String, data: String },
}

fn heading(role: ChatRole) -> &'static str {
    match role {
        ChatRole::User => "User",
        ChatRole::Assistant => "Assistant",
    }
}

/// One message body: text, then content parts, then tool calls, then results.
fn message_pieces(msg: &ChatMessage, keep_images: bool) -> Vec<Piece> {
    let mut pieces: Vec<Piece> = Vec::new();
    if !msg.content.trim().is_empty() {
        pieces.push(Piece::Text(msg.content.trim_end().to_string()));
    }
    for part in &msg.content_parts {
        match part {
            ContentPart::Text { text } => {
                if !text.trim().is_empty() {
                    pieces.push(Piece::Text(text.trim_end().to_string()));
                }
            }
            ContentPart::Image { media_type, data } => {
                if keep_images {
                    pieces.push(Piece::Image {
                        media_type: media_type.clone(),
                        data: data.clone(),
                    });
                } else {
                    pieces.push(Piece::Text(format!("[image omitted: {media_type}]")));
                }
            }
        }
    }
    for call in &msg.tool_calls {
        // `Value::to_string` is already compact JSON and cannot fail, which is
        // why the input is not re-serialized through a `serde_json::Error` path.
        let input = truncate(&call.input.to_string(), TOOL_INPUT_CAP);
        pieces.push(Piece::Text(format!("Tool call: {}({input})", call.name)));
    }
    for result in &msg.tool_results {
        let content = truncate(&result.content, TOOL_RESULT_CAP);
        if result.is_error {
            pieces.push(Piece::Text(format!("Tool result (error): {content}")));
        } else {
            pieces.push(Piece::Text(format!("Tool result: {content}")));
        }
    }
    pieces
}

fn pieces_are_blank(pieces: &[Piece]) -> bool {
    pieces.iter().all(|piece| match piece {
        Piece::Text(text) => text.trim().is_empty(),
        Piece::Image { .. } => false,
    })
}

/// Lay pieces out into blocks. Every piece after the first is preceded by a
/// newline, so concatenating the text blocks (with each image as its
/// placeholder) reproduces the placeholder form byte for byte.
fn push_pieces(out: &mut Vec<PromptBlock>, pieces: Vec<Piece>) {
    for (index, piece) in pieces.into_iter().enumerate() {
        if index > 0 {
            append_text(out, "\n");
        }
        match piece {
            Piece::Text(text) => append_text(out, &text),
            Piece::Image { media_type, data } => {
                out.push(PromptBlock::Image { media_type, data });
            }
        }
    }
}

/// Blank line between two messages (no-op before the first one).
fn separate(out: &mut Vec<PromptBlock>) {
    if !out.is_empty() {
        append_text(out, "\n\n");
    }
}

fn append_text(out: &mut Vec<PromptBlock>, text: &str) {
    match out.last_mut() {
        Some(PromptBlock::Text(existing)) => existing.push_str(text),
        _ => out.push(PromptBlock::Text(text.to_string())),
    }
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

    #[test]
    fn blocks_keep_the_image_payload_between_text_runs() {
        let mut message = ChatMessage::user("look at this");
        message.content_parts = vec![ContentPart::Image {
            media_type: "image/png".into(),
            data: "AAAA".into(),
        }];
        let last = ChatMessage::user("what is in it?");
        let blocks = render_blocks(&request(vec![message, last]));
        assert_eq!(
            blocks,
            vec![
                PromptBlock::Text("## User\n\nlook at this\n".into()),
                PromptBlock::Image {
                    media_type: "image/png".into(),
                    data: "AAAA".into(),
                },
                PromptBlock::Text("\n\nwhat is in it?".into()),
            ]
        );
    }

    #[test]
    fn blocks_and_prompt_agree_once_images_are_replaced() {
        let mut message = ChatMessage::user("look");
        message.content_parts = vec![ContentPart::Image {
            media_type: "image/jpeg".into(),
            data: "BBBB".into(),
        }];
        let req = request(vec![message, assistant("ok"), ChatMessage::user("done")]);

        let text_of_blocks: String = render_blocks(&req)
            .into_iter()
            .map(|block| match block {
                PromptBlock::Text(text) => text,
                PromptBlock::Image { media_type, .. } => {
                    format!("[image omitted: {media_type}]")
                }
            })
            .collect();
        assert_eq!(text_of_blocks, render_prompt(&req));
        assert!(!render_prompt(&req).contains("BBBB"));
    }
}
