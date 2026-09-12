//! Wire-format mapping shared by the protocol implementations.
//!
//! `openai.rs` and `anthropic.rs` used to each carry their own near-identical
//! copy of "turn a [`ToolDef`] into the provider's tool schema" and "turn a
//! [`ContentPart`] into the provider's content block". Both operations differ
//! only in the JSON envelope, so they live here once, parameterised by
//! [`Protocol`], and each implementation calls in instead of re-deriving the
//! shape. One place to fix when a provider tightens its schema.

use serde_json::Value;

use crate::provider::{ContentPart, ToolDef};

/// The wire protocol a request is being built for (not the vendor: xAI,
/// DeepSeek, Groq, Ollama & co. all speak [`Protocol::OpenAi`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// OpenAI Chat Completions.
    OpenAi,
    /// Anthropic Messages.
    Anthropic,
}

/// Map one tool definition onto the protocol's tool schema.
///
/// - OpenAI: `{"type":"function","function":{name,description,parameters}}`
/// - Anthropic: `{name,description,input_schema}`
pub fn map_tool(tool: &ToolDef, protocol: Protocol) -> Value {
    match protocol {
        Protocol::OpenAi => serde_json::json!({
            "type": "function",
            "function": {
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.input_schema,
            }
        }),
        Protocol::Anthropic => serde_json::json!({
            "name": tool.name,
            "description": tool.description,
            "input_schema": tool.input_schema,
        }),
    }
}

/// Map a whole tool list onto the protocol's tool array.
pub fn map_tools(tools: &[ToolDef], protocol: Protocol) -> Vec<Value> {
    tools.iter().map(|t| map_tool(t, protocol)).collect()
}

/// Map one content part onto the protocol's content block.
///
/// - OpenAI text: `{"type":"text","text":…}`; image: `{"type":"image_url",
///   "image_url":{"url":"data:<media-type>;base64,<data>"}}`
/// - Anthropic text: `{"type":"text","text":…}`; image: `{"type":"image",
///   "source":{"type":"base64","media_type":…,"data":…}}`
pub fn map_content_part(part: &ContentPart, protocol: Protocol) -> Value {
    match (part, protocol) {
        (ContentPart::Text { text }, _) => serde_json::json!({ "type": "text", "text": text }),
        (ContentPart::Image { media_type, data }, Protocol::OpenAi) => serde_json::json!({
            "type": "image_url",
            "image_url": { "url": format!("data:{media_type};base64,{data}") },
        }),
        (ContentPart::Image { media_type, data }, Protocol::Anthropic) => serde_json::json!({
            "type": "image",
            "source": { "type": "base64", "media_type": media_type, "data": data },
        }),
    }
}

/// Map the image parts of a message (text is carried separately by both
/// implementations, which have different rules about where it goes).
pub fn map_image_parts(parts: &[ContentPart], protocol: Protocol) -> Vec<Value> {
    parts
        .iter()
        .filter(|p| matches!(p, ContentPart::Image { .. }))
        .map(|p| map_content_part(p, protocol))
        .collect()
}

/// Build a text content block for the given protocol (identical shape today,
/// routed through here so a future protocol can diverge in one place).
pub fn text_block(text: &str) -> Value {
    serde_json::json!({ "type": "text", "text": text })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> ToolDef {
        ToolDef {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
            }),
        }
    }

    fn image() -> ContentPart {
        ContentPart::Image {
            media_type: "image/png".to_string(),
            data: "aGVsbG8=".to_string(),
        }
    }

    #[test]
    fn same_tool_def_maps_to_both_protocol_shapes() {
        let t = tool();

        let openai = map_tool(&t, Protocol::OpenAi);
        assert_eq!(openai["type"], "function");
        assert_eq!(openai["function"]["name"], "read_file");
        assert_eq!(openai["function"]["description"], "Read a file");
        assert_eq!(openai["function"]["parameters"], t.input_schema);
        assert!(openai.get("input_schema").is_none());

        let anthropic = map_tool(&t, Protocol::Anthropic);
        assert_eq!(anthropic["name"], "read_file");
        assert_eq!(anthropic["description"], "Read a file");
        assert_eq!(anthropic["input_schema"], t.input_schema);
        assert!(anthropic.get("function").is_none());
    }

    #[test]
    fn map_tools_preserves_order_for_both_protocols() {
        let tools = vec![
            tool(),
            ToolDef {
                name: "bash".to_string(),
                description: "Run a command".to_string(),
                input_schema: serde_json::json!({ "type": "object" }),
            },
        ];
        let openai = map_tools(&tools, Protocol::OpenAi);
        let anthropic = map_tools(&tools, Protocol::Anthropic);
        assert_eq!(openai.len(), 2);
        assert_eq!(anthropic.len(), 2);
        assert_eq!(openai[1]["function"]["name"], "bash");
        assert_eq!(anthropic[1]["name"], "bash");
    }

    #[test]
    fn same_image_part_maps_to_both_protocol_shapes() {
        let p = image();

        let openai = map_content_part(&p, Protocol::OpenAi);
        assert_eq!(openai["type"], "image_url");
        assert_eq!(openai["image_url"]["url"], "data:image/png;base64,aGVsbG8=");

        let anthropic = map_content_part(&p, Protocol::Anthropic);
        assert_eq!(anthropic["type"], "image");
        assert_eq!(anthropic["source"]["type"], "base64");
        assert_eq!(anthropic["source"]["media_type"], "image/png");
        assert_eq!(anthropic["source"]["data"], "aGVsbG8=");
    }

    #[test]
    fn text_parts_map_identically_for_both_protocols() {
        let p = ContentPart::Text {
            text: "hello".to_string(),
        };
        let expected = serde_json::json!({ "type": "text", "text": "hello" });
        assert_eq!(map_content_part(&p, Protocol::OpenAi), expected);
        assert_eq!(map_content_part(&p, Protocol::Anthropic), expected);
        assert_eq!(text_block("hello"), expected);
    }

    #[test]
    fn map_image_parts_skips_text_parts() {
        let parts = vec![
            ContentPart::Text {
                text: "look".to_string(),
            },
            image(),
        ];
        let openai = map_image_parts(&parts, Protocol::OpenAi);
        assert_eq!(openai.len(), 1);
        assert_eq!(openai[0]["type"], "image_url");
        let anthropic = map_image_parts(&parts, Protocol::Anthropic);
        assert_eq!(anthropic.len(), 1);
        assert_eq!(anthropic[0]["type"], "image");
    }
}
