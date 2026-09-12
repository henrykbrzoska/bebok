//! OpenAI Chat Completions compatible client (SSE streaming).
//!
//! Used for OpenAI itself and every OpenAI-compatible provider (xAI, DeepSeek,
//! OpenRouter, Ollama and self-hosted OpenAI-compatible servers). Each parses
//! the standard `data: {...}` / `data: [DONE]` SSE stream; tool calls arrive as
//! deltas accumulated across chunks.

use std::collections::HashMap;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{Stream, StreamExt, stream};
use serde_json::Value;

use crate::provider::{
    ChatMessage, ChatRequest, LlmError, Provider, StreamEvent, StreamResult, ToolCall, Usage,
    retry_after_from_headers,
};
use crate::spec::model_name;
use crate::wire::{Protocol, map_image_parts, map_tools, text_block};

/// Build an OpenAI Chat Completions request body.
pub fn openai_body(req: &ChatRequest, model: &str) -> Value {
    let tools: Vec<Value> = map_tools(&req.tools, Protocol::OpenAi);

    let mut body = serde_json::json!({
        "model": model,
        "messages": to_openai_messages(&req.messages, &req.system),
        "tools": tools,
        "stream": true,
        "stream_options": { "include_usage": true },
        "max_tokens": req.max_tokens,
    });
    if let Some(effort) = req.thinking.openai_effort() {
        if let Value::Object(map) = &mut body {
            map.insert(
                "reasoning_effort".to_string(),
                Value::String(effort.to_string()),
            );
        }
    }
    body
}

/// Emit one OpenAI `content` value: a plain string for text-only content, or
/// the content-part array when images are attached. Used on every path so a
/// message that carries images *and* tool results cannot silently lose either.
fn openai_content(m: &ChatMessage) -> Value {
    use crate::provider::ContentPart;
    let has_images = m
        .content_parts
        .iter()
        .any(|p| matches!(p, ContentPart::Image { .. }));
    if !has_images {
        return Value::String(m.content.clone());
    }
    let mut parts: Vec<Value> = Vec::new();
    if !m.content.is_empty() {
        parts.push(text_block(&m.content));
    }
    parts.extend(map_image_parts(&m.content_parts, Protocol::OpenAi));
    Value::Array(parts)
}

/// Convert provider-neutral messages into OpenAI `messages` (system message
/// first, tool results as `role: tool`).
pub fn to_openai_messages(msgs: &[ChatMessage], system: &str) -> Vec<Value> {
    let mut out = Vec::with_capacity(msgs.len() + 1);
    if !system.is_empty() {
        out.push(serde_json::json!({ "role": "system", "content": system }));
    }
    for m in msgs {
        if !m.tool_results.is_empty() {
            // Each tool result becomes its own `role: tool` message.
            for tr in &m.tool_results {
                out.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tr.tool_use_id,
                    "content": tr.content,
                }));
            }
            // Never drop the accompanying user content: text and/or images are
            // re-emitted as a user message (a message can carry both tool
            // results and attachments in the same turn).
            if !m.content.is_empty() || !m.content_parts.is_empty() {
                out.push(serde_json::json!({
                    "role": m.role.as_str(),
                    "content": openai_content(m),
                }));
            }
            continue;
        }
        if m.role.as_str() == "assistant" && !m.tool_calls.is_empty() {
            let tool_calls: Vec<Value> = m
                .tool_calls
                .iter()
                .map(|tc| {
                    serde_json::json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": serde_json::to_string(&tc.input).unwrap_or_else(|_| "{}".into()),
                        }
                    })
                })
                .collect();
            out.push(serde_json::json!({
                "role": "assistant",
                "content": openai_content(m),
                "tool_calls": tool_calls,
            }));
        } else {
            out.push(serde_json::json!({
                "role": m.role.as_str(),
                "content": openai_content(m),
            }));
        }
    }
    out
}

/// The OpenAI-compatible streaming provider.
pub struct OpenAiProvider {
    api_key: Option<String>,
    endpoint: String,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(api_key: Option<String>, endpoint: impl Into<String>) -> Self {
        Self {
            api_key,
            endpoint: endpoint.into(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai"
    }

    async fn stream(
        &self,
        req: ChatRequest,
    ) -> StreamResult<BoxStream<'static, StreamResult<StreamEvent>>> {
        let model = model_name(&req.model).to_string();
        let body = openai_body(&req, &model);

        let mut request = self
            .client
            .post(&self.endpoint)
            .header("content-type", "application/json");
        if let Some(key) = self.api_key.as_ref().filter(|k| !k.trim().is_empty()) {
            request = request.header("authorization", format!("Bearer {key}"));
        }

        let resp = request.json(&body).send().await?;
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

        Ok(Box::pin(openai_stream(resp)))
    }
}

/// Parse an OpenAI SSE response into typed events.
pub fn openai_stream(resp: reqwest::Response) -> impl Stream<Item = StreamResult<StreamEvent>> {
    let byte_stream = resp.bytes_stream();
    let parser = OpenAiParser::new();
    stream::unfold(
        (byte_stream, parser),
        move |(mut byte_stream, mut parser)| async move {
            loop {
                if let Some(item) = parser.next_ready() {
                    return Some((item, (byte_stream, parser)));
                }
                // EOF already handled and everything drained: end the stream
                // (and never poll the byte stream past its own end).
                if parser.is_finished() {
                    return None;
                }
                match byte_stream.next().await {
                    Some(Ok(bytes)) => parser.feed(&bytes),
                    Some(Err(e)) => {
                        let err = Err(LlmError::Stream(e.to_string()));
                        return Some((err, (byte_stream, parser)));
                    }
                    None => {
                        // EOF: flush anything still pending (unterminated line,
                        // tool calls that never got a finish_reason) and make
                        // sure exactly one `Done` was emitted for this stream.
                        parser.finish();
                    }
                }
            }
        },
    )
}

/// Parse a complete (already buffered) OpenAI-compatible SSE body into typed
/// events, running the exact same state machine as [`openai_stream`] including
/// end-of-stream finalisation.
///
/// This is the transport-free entry point used by the parser tests over
/// recorded SSE fixtures; `chunk_size` splits the body into byte chunks so a
/// test can prove the parser is insensitive to where the network happened to
/// cut the stream (`None` = feed it all at once).
pub fn parse_openai_sse(body: &str, chunk_size: Option<usize>) -> Vec<StreamResult<StreamEvent>> {
    let mut parser = OpenAiParser::new();
    let bytes = body.as_bytes();
    let step = chunk_size.unwrap_or(bytes.len()).max(1);
    let mut out = Vec::new();
    for chunk in bytes.chunks(step) {
        parser.feed(chunk);
        while let Some(ev) = parser.next_ready() {
            out.push(ev);
        }
    }
    parser.finish();
    while let Some(ev) = parser.next_ready() {
        out.push(ev);
    }
    out
}

/// Accumulated state for one in-flight tool call.
#[derive(Default)]
struct PendingTool {
    id: String,
    name: String,
    arguments: String,
}

/// Incremental parser for an OpenAI-compatible SSE stream.
///
/// Finalisation contract (bug B13): **every** stream emits exactly one
/// [`StreamEvent::Done`] - never zero (the agent loop books tokens/cost only on
/// `Done`, so a missing one silently zeroes a session's usage), never two (that
/// would double-count). The `Done` carries real usage when the provider sent a
/// usage chunk, and zeroed best-effort usage when the stream ended without one
/// (e.g. cut off right after a tool call).
struct OpenAiParser {
    line_buf: Vec<u8>,
    /// Buffered parsed events waiting to be yielded.
    ready: std::collections::VecDeque<StreamResult<StreamEvent>>,
    tools: HashMap<usize, PendingTool>,
    usage_in: u64,
    usage_out: u64,
    /// A `usage` object was seen (so the counters are authoritative).
    usage_seen: bool,
    /// A `finish_reason` was seen (the completion is logically over).
    finish_seen: bool,
    /// `Done` has already been queued; never queue a second one.
    done_emitted: bool,
    /// `finish()` already ran (EOF handling is idempotent).
    finished: bool,
}

impl OpenAiParser {
    fn new() -> Self {
        Self {
            line_buf: Vec::new(),
            ready: std::collections::VecDeque::new(),
            tools: HashMap::new(),
            usage_in: 0,
            usage_out: 0,
            usage_seen: false,
            finish_seen: false,
            done_emitted: false,
            finished: false,
        }
    }

    fn next_ready(&mut self) -> Option<StreamResult<StreamEvent>> {
        self.ready.pop_front()
    }

    /// True once [`OpenAiParser::finish`] has run (EOF seen and finalised).
    fn is_finished(&self) -> bool {
        self.finished
    }

    /// Queue the single `Done` for this stream (no-op once emitted).
    fn emit_done(&mut self) {
        if self.done_emitted {
            return;
        }
        self.done_emitted = true;
        self.ready.push_back(Ok(StreamEvent::Done(Usage {
            input_tokens: self.usage_in,
            output_tokens: self.usage_out,
            cost: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        })));
    }

    /// Flush the accumulated tool calls (on `finish_reason` or at EOF).
    fn flush_tools(&mut self) {
        if self.tools.is_empty() {
            return;
        }
        let mut indices: Vec<usize> = self.tools.keys().copied().collect();
        indices.sort_unstable();
        for index in indices {
            let slot = self.tools.remove(&index).unwrap_or_default();
            if slot.id.is_empty() {
                continue;
            }
            let input = if slot.arguments.trim().is_empty() {
                Value::Object(serde_json::Map::new())
            } else {
                serde_json::from_str(&slot.arguments).unwrap_or(Value::String(slot.arguments))
            };
            self.ready.push_back(Ok(StreamEvent::ToolCall(ToolCall {
                id: slot.id,
                name: slot.name,
                input,
            })));
        }
    }

    /// End-of-stream handling. Flushes a trailing line without a newline, any
    /// tool calls that never saw a `finish_reason`, and guarantees the `Done`.
    /// Returns `true` when it queued something new to yield.
    fn finish(&mut self) -> bool {
        if self.finished {
            return false;
        }

        self.finished = true;
        if !self.line_buf.is_empty() {
            let line = String::from_utf8_lossy(&self.line_buf).to_string();
            self.line_buf.clear();
            self.handle_line(line.trim_end_matches('\r'));
        }
        // A stream truncated right after a tool call still owes us the call
        // and a Done - with zeroed usage if the usage chunk never arrived.
        self.flush_tools();
        self.emit_done();
        !self.ready.is_empty()
    }

    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if b == b'\n' {
                let line = String::from_utf8_lossy(&self.line_buf).to_string();
                self.line_buf.clear();
                self.handle_line(line.trim_end_matches('\r'));
            } else {
                self.line_buf.push(b);
            }
        }
    }

    fn handle_line(&mut self, line: &str) {
        let Some(data) = line.strip_prefix("data:") else {
            return;
        };
        let data = data.trim_start();
        if data.is_empty() {
            return;
        }
        // `[DONE]` terminates the stream: flush tool calls that never saw a
        // finish_reason, then emit the single Done (previously this line was
        // dropped, which is how streams ending in `usage` + `[DONE]` produced
        // no Done at all and lost their token/cost accounting).
        if data == "[DONE]" {
            self.flush_tools();
            self.emit_done();
            return;
        }
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(e) => {
                self.ready
                    .push_back(Err(LlmError::Parse(format!("bad SSE data: {e}"))));
                return;
            }
        };

        // Usage chunk (stream_options.include_usage) has empty choices + usage.
        if let Some(usage) = v.get("usage").filter(|u| !u.is_null()) {
            self.usage_in = usage
                .get("prompt_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            self.usage_out = usage
                .get("completion_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            self.usage_seen = true;
        }

        let Some(choice) = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
        else {
            // The canonical final chunk: `usage` with an empty `choices` array.
            // The completion is already finished, so this is the moment the
            // real usage becomes available - emit the Done here.
            if self.usage_seen && self.finish_seen {
                self.flush_tools();
                self.emit_done();
            }
            return;
        };
        let delta = choice.get("delta").cloned().unwrap_or(Value::Null);
        let finish = choice.get("finish_reason").and_then(|x| x.as_str());

        if let Some(content) = delta.get("content").and_then(|x| x.as_str()) {
            if !content.is_empty() {
                self.ready
                    .push_back(Ok(StreamEvent::Text(content.to_string())));
            }
        }

        // Reasoning content (DeepSeek / some OpenAI-compatible models).
        if let Some(reasoning) = delta
            .get("reasoning_content")
            .and_then(|x| x.as_str())
            .or_else(|| delta.get("reasoning").and_then(|x| x.as_str()))
        {
            if !reasoning.is_empty() {
                self.ready
                    .push_back(Ok(StreamEvent::Thinking(reasoning.to_string())));
            }
        }

        if let Some(calls) = delta.get("tool_calls").and_then(|x| x.as_array()) {
            for call in calls {
                let index = call.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                let slot = self.tools.entry(index).or_default();
                if let Some(id) = call.get("id").and_then(|x| x.as_str()) {
                    slot.id = id.to_string();
                }
                if let Some(name) = call
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|x| x.as_str())
                {
                    slot.name = name.to_string();
                }
                if let Some(args) = call
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|x| x.as_str())
                {
                    slot.arguments.push_str(args);
                }
            }
        }

        // A finish_reason closes the current tool-call accumulation.
        if finish.is_some() {
            self.finish_seen = true;
            self.flush_tools();
            // Usage already in hand (some servers put it on the finishing
            // chunk, or sent it earlier): this is the last chunk that matters.
            if self.usage_seen {
                self.emit_done();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::openai_body;
    use crate::provider::{ChatMessage, ChatRequest, Thinking};

    fn req(thinking: Thinking) -> ChatRequest {
        ChatRequest {
            model: "gpt-5".to_string(),
            system: "sys".to_string(),
            messages: vec![ChatMessage::user("hi")],
            tools: Vec::new(),
            max_tokens: 128,
            thinking,
        }
    }

    #[test]
    fn body_maps_thinking_to_reasoning_effort() {
        let off = openai_body(&req(Thinking::Off), "gpt-5");
        assert!(off.get("reasoning_effort").is_none());

        let low = openai_body(&req(Thinking::Low), "gpt-5");
        assert_eq!(low["reasoning_effort"], "low");

        let high = openai_body(&req(Thinking::High), "gpt-5");
        assert_eq!(high["reasoning_effort"], "high");

        let max = openai_body(&req(Thinking::Max), "gpt-5");
        assert_eq!(max["reasoning_effort"], "high");
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;
    use crate::provider::{ChatMessage, ChatRole, ContentPart, ToolResult};

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
    fn openai_emits_content_part_array_with_data_url() {
        let msgs = to_openai_messages(&[img_msg()], "");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        let parts = msgs[0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts[0],
            serde_json::json!({"type": "text", "text": "look"})
        );
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(
            parts[1]["image_url"]["url"],
            "data:image/png;base64,aGVsbG8="
        );
    }

    #[test]
    fn openai_text_only_path_unchanged() {
        let msgs = to_openai_messages(&[ChatMessage::user("hi")], "sys");
        assert_eq!(
            msgs[0],
            serde_json::json!({"role": "system", "content": "sys"})
        );
        assert_eq!(
            msgs[1],
            serde_json::json!({"role": "user", "content": "hi"})
        );
    }

    #[test]
    fn openai_skips_empty_text_with_images() {
        let mut m = img_msg();
        m.content.clear();
        let msgs = to_openai_messages(&[m], "");
        let parts = msgs[0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "image_url");
    }

    /// Regression for the silent-loss bug: a user message carrying an image
    /// *and* tool results must keep both.
    #[test]
    fn openai_images_survive_alongside_tool_results() {
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
        let msgs = to_openai_messages(&[m], "");
        assert_eq!(msgs[0]["role"], "tool");
        assert_eq!(msgs[0]["tool_call_id"], "call-1");
        assert_eq!(msgs[1]["role"], "user");
        let parts = msgs[1]["content"].as_array().unwrap();
        assert_eq!(
            parts[0],
            serde_json::json!({"type": "text", "text": "see this"})
        );
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(
            parts[1]["image_url"]["url"],
            "data:image/png;base64,aGVsbG8="
        );
    }

    /// User text must never vanish when tool results are present.
    #[test]
    fn openai_text_survives_alongside_tool_results() {
        let m = ChatMessage {
            role: ChatRole::User,
            content: "notes".to_string(),
            tool_calls: Vec::new(),
            tool_results: vec![ToolResult {
                tool_use_id: "call-2".to_string(),
                content: "result".to_string(),
                is_error: true,
            }],
            content_parts: Vec::new(),
        };
        let msgs = to_openai_messages(&[m], "");
        assert_eq!(
            msgs[1],
            serde_json::json!({"role": "user", "content": "notes"})
        );
    }

    #[test]
    fn openai_empty_user_with_only_tool_results_is_not_emitted() {
        let m = ChatMessage {
            role: ChatRole::User,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_results: vec![ToolResult {
                tool_use_id: "call-3".to_string(),
                content: "result".to_string(),
                is_error: false,
            }],
            content_parts: Vec::new(),
        };
        let msgs = to_openai_messages(&[m], "");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "tool");
    }
}
