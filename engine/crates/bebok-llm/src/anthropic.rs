//! Anthropic-compatible request building and SSE parsing.
//!
//! Z.ai exposes an Anthropic-compatible Messages API; in M1 we use that single
//! format, but it is parsed here in isolation so that additional providers can
//! bring their own parsers later (no shared event format).

use std::collections::VecDeque;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{Stream, StreamExt, stream};
use serde_json::Value;

use crate::provider::{
    ChatMessage, ChatRequest, LlmError, Provider, StreamEvent, StreamResult, ToolCall, Usage,
    retry_after_from_headers,
};
use crate::wire::{Protocol, map_image_parts, map_tools, text_block};

/// Native Anthropic Messages endpoint.
pub const ANTHROPIC_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";

/// Anthropic provider using the Messages API.
pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: ANTHROPIC_MESSAGES_URL.to_string(),
            client: reqwest::Client::new(),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    async fn stream(
        &self,
        req: ChatRequest,
    ) -> StreamResult<BoxStream<'static, StreamResult<StreamEvent>>> {
        let model = req
            .model
            .rsplit('/')
            .next()
            .unwrap_or(&req.model)
            .to_string();
        let body = anthropic_body(&req, &model);

        let resp = self
            .client
            .post(&self.base_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let retry_after = retry_after_from_headers(resp.headers());
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Http {
                status,
                body: text,
                retry_after,
            });
        }

        Ok(Box::pin(anthropic_stream(resp)))
    }
}

/// Build the Anthropic Messages request body.
pub fn anthropic_body(req: &ChatRequest, model: &str) -> Value {
    let tools: Vec<Value> = map_tools(&req.tools, Protocol::Anthropic);

    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": req.max_tokens,
        "system": req.system,
        "messages": to_anthropic_messages(&req.messages),
        "tools": tools,
        "stream": true,
    });
    if let Some(budget) = req.thinking.anthropic_budget() {
        if let Value::Object(map) = &mut body {
            map.insert(
                "thinking".to_string(),
                serde_json::json!({ "type": "enabled", "budget_tokens": budget }),
            );
        }
    }
    body
}

/// Convert provider-neutral messages into Anthropic content blocks.
pub fn to_anthropic_messages(msgs: &[ChatMessage]) -> Vec<Value> {
    let mut out = Vec::with_capacity(msgs.len());
    for m in msgs {
        let mut blocks: Vec<Value> = Vec::new();
        // Anthropic requires tool_result blocks to come first in a user turn;
        // images and text follow, so an attachment sent together with tool
        // results is still delivered (never dropped, never misordered).
        for tr in &m.tool_results {
            blocks.push(serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tr.tool_use_id,
                "content": tr.content,
                "is_error": tr.is_error,
            }));
        }
        blocks.extend(map_image_parts(&m.content_parts, Protocol::Anthropic));
        if !m.content.is_empty() {
            blocks.push(text_block(&m.content));
        }
        for tc in &m.tool_calls {
            blocks.push(serde_json::json!({
                "type": "tool_use",
                "id": tc.id,
                "name": tc.name,
                "input": tc.input,
            }));
        }
        if blocks.is_empty() {
            blocks.push(text_block(""));
        }
        out.push(serde_json::json!({
            "role": m.role.as_str(),
            "content": blocks,
        }));
    }
    out
}

/// Convert an Anthropic SSE response into a stream of typed events.
pub fn anthropic_stream(resp: reqwest::Response) -> impl Stream<Item = StreamResult<StreamEvent>> {
    let byte_stream = resp.bytes_stream();
    let parser = AnthropicParser::new();
    let pending: VecDeque<StreamResult<StreamEvent>> = VecDeque::new();

    stream::unfold(
        (byte_stream, parser, pending),
        move |(mut byte_stream, mut parser, mut pending)| async move {
            loop {
                if let Some(item) = pending.pop_front() {
                    return Some((item, (byte_stream, parser, pending)));
                }
                match byte_stream.next().await {
                    Some(Ok(bytes)) => {
                        for ev in parser.feed(&bytes) {
                            pending.push_back(ev);
                        }
                    }
                    Some(Err(e)) => {
                        let err = Err(LlmError::Stream(e.to_string()));
                        return Some((err, (byte_stream, parser, pending)));
                    }
                    None => return None,
                }
            }
        },
    )
}

struct AnthropicParser {
    line_buf: Vec<u8>,
    data: Vec<String>,
    block_type: Option<String>,
    tool_id: Option<String>,
    tool_name: Option<String>,
    tool_input: String,
    usage_in: u64,
    usage_out: u64,
    cache_read_in: Option<u64>,
    cache_creation_in: Option<u64>,
}

impl AnthropicParser {
    fn new() -> Self {
        Self {
            line_buf: Vec::new(),
            data: Vec::new(),
            block_type: None,
            tool_id: None,
            tool_name: None,
            tool_input: String::new(),
            usage_in: 0,
            usage_out: 0,
            cache_read_in: None,
            cache_creation_in: None,
        }
    }

    fn feed(&mut self, bytes: &[u8]) -> Vec<StreamResult<StreamEvent>> {
        let mut out = Vec::new();
        for &b in bytes {
            if b == b'\n' {
                let line = String::from_utf8_lossy(&self.line_buf).to_string();
                self.line_buf.clear();
                self.handle_line(&line, &mut out);
            } else {
                self.line_buf.push(b);
            }
        }
        out
    }

    fn handle_line(&mut self, line: &str, out: &mut Vec<StreamResult<StreamEvent>>) {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("event:") {
            // Event type is currently redundant (the JSON payload carries it).
            let _ = rest;
        } else if let Some(rest) = line.strip_prefix("data:") {
            self.data.push(rest.trim_start().to_string());
        } else if line.is_empty() {
            if let Some(ev) = self.dispatch() {
                out.push(ev);
            }
            self.data.clear();
        }
    }

    fn dispatch(&mut self) -> Option<StreamResult<StreamEvent>> {
        let data = self.data.join("\n");
        if data.is_empty() {
            return None;
        }
        let v: Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(e) => return Some(Err(LlmError::Parse(format!("bad SSE data: {e}")))),
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match ty {
            "message_start" => {
                self.usage_in = v
                    .pointer("/message/usage/input_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
                self.cache_read_in = v
                    .pointer("/message/usage/cache_read_input_tokens")
                    .and_then(|x| x.as_u64());
                self.cache_creation_in = v
                    .pointer("/message/usage/cache_creation_input_tokens")
                    .and_then(|x| x.as_u64());
                None
            }
            "content_block_start" => {
                let cb = v.get("content_block").cloned().unwrap_or(Value::Null);
                let bty = cb
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                self.block_type = Some(bty.clone());
                if bty == "tool_use" {
                    self.tool_id = cb.get("id").and_then(|x| x.as_str()).map(str::to_string);
                    self.tool_name = cb.get("name").and_then(|x| x.as_str()).map(str::to_string);
                    self.tool_input = String::new();
                }
                None
            }
            "content_block_delta" => {
                let delta = v.get("delta").cloned().unwrap_or(Value::Null);
                match delta.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "text_delta" => delta
                        .get("text")
                        .and_then(|x| x.as_str())
                        .map(|t| Ok(StreamEvent::Text(t.to_string()))),
                    "thinking_delta" => delta
                        .get("thinking")
                        .and_then(|x| x.as_str())
                        .map(|t| Ok(StreamEvent::Thinking(t.to_string()))),
                    "input_json_delta" => {
                        if let Some(pj) = delta.get("partial_json").and_then(|x| x.as_str()) {
                            self.tool_input.push_str(pj);
                        }
                        None
                    }
                    _ => None,
                }
            }
            "content_block_stop" => {
                if self.block_type.as_deref() == Some("tool_use") {
                    let input = if self.tool_input.trim().is_empty() {
                        Value::Object(serde_json::Map::new())
                    } else {
                        serde_json::from_str(&self.tool_input).unwrap_or(Value::Null)
                    };
                    let call = ToolCall {
                        id: self.tool_id.clone().unwrap_or_default(),
                        name: self.tool_name.clone().unwrap_or_default(),
                        input,
                    };
                    self.block_type = None;
                    Some(Ok(StreamEvent::ToolCall(call)))
                } else {
                    self.block_type = None;
                    None
                }
            }
            "message_delta" => {
                self.usage_out = v
                    .pointer("/usage/output_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
                // Z.ai reports input/cache usage in message_delta (message_start is 0).
                if let Some(n) = v.pointer("/usage/input_tokens").and_then(|x| x.as_u64()) {
                    self.usage_in = n;
                }
                if let Some(n) = v
                    .pointer("/usage/cache_read_input_tokens")
                    .and_then(|x| x.as_u64())
                {
                    self.cache_read_in = Some(n);
                }
                if let Some(n) = v
                    .pointer("/usage/cache_creation_input_tokens")
                    .and_then(|x| x.as_u64())
                {
                    self.cache_creation_in = Some(n);
                }
                None
            }
            "message_stop" => Some(Ok(StreamEvent::Done(Usage {
                input_tokens: self.usage_in,
                output_tokens: self.usage_out,
                cost: None,
                cache_read_input_tokens: self.cache_read_in,
                cache_creation_input_tokens: self.cache_creation_in,
            }))),
            "error" => {
                let msg = v
                    .pointer("/error/message")
                    .and_then(|x| x.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                Some(Err(LlmError::Provider(msg)))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::anthropic_body;
    use crate::provider::{ChatMessage, ChatRequest, Thinking};

    fn req(thinking: Thinking) -> ChatRequest {
        ChatRequest {
            model: "claude-sonnet".to_string(),
            system: "sys".to_string(),
            messages: vec![ChatMessage::user("hi")],
            tools: Vec::new(),
            max_tokens: 128,
            thinking,
        }
    }

    #[test]
    fn body_maps_thinking_to_budget() {
        let off = anthropic_body(&req(Thinking::Off), "claude-sonnet");
        assert!(off.get("thinking").is_none());

        let low = anthropic_body(&req(Thinking::Low), "claude-sonnet");
        assert_eq!(low["thinking"]["type"], "enabled");
        assert_eq!(low["thinking"]["budget_tokens"], 1024);

        let max = anthropic_body(&req(Thinking::Max), "claude-sonnet");
        assert_eq!(max["thinking"]["budget_tokens"], 8192);
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;
    use crate::provider::{ChatMessage, ChatRole, ContentPart};

    fn img_msg() -> ChatMessage {
        ChatMessage {
            role: ChatRole::User,
            content: "look".to_string(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            content_parts: vec![ContentPart::Image {
                media_type: "image/png".to_string(),
                data: "aGVsbG8=".to_string(),
            }],
        }
    }

    #[test]
    fn anthropic_emits_image_blocks_before_text() {
        let msgs = to_anthropic_messages(&[img_msg()]);
        assert_eq!(msgs.len(), 1);
        let blocks = msgs[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "image");
        assert_eq!(blocks[0]["source"]["type"], "base64");
        assert_eq!(blocks[0]["source"]["media_type"], "image/png");
        assert_eq!(blocks[0]["source"]["data"], "aGVsbG8=");
        assert_eq!(blocks[1]["type"], "text");
        assert_eq!(blocks[1]["text"], "look");
    }

    #[test]
    fn anthropic_text_only_path_unchanged() {
        let msgs = to_anthropic_messages(&[ChatMessage::user("hi")]);
        let blocks = msgs[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0], serde_json::json!({"type": "text", "text": "hi"}));
    }

    /// Regression: images attached in the same turn as tool results keep both,
    /// with tool_result blocks first (Anthropic API ordering requirement).
    #[test]
    fn anthropic_tool_results_precede_images() {
        use crate::provider::ToolResult;
        let m = ChatMessage {
            role: ChatRole::User,
            content: "see this".to_string(),
            tool_calls: Vec::new(),
            tool_results: vec![ToolResult {
                tool_use_id: "call-1".to_string(),
                content: "ok".to_string(),
                is_error: false,
            }],
            content_parts: vec![ContentPart::Image {
                media_type: "image/png".to_string(),
                data: "aGVsbG8=".to_string(),
            }],
        };
        let msgs = to_anthropic_messages(&[m]);
        let blocks = msgs[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "call-1");
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["data"], "aGVsbG8=");
        assert_eq!(blocks[2]["type"], "text");
        assert_eq!(blocks[2]["text"], "see this");
    }
}
