//! OpenAI Responses protocol client (SSE streaming).
//!
//! OpenCode Zen answers the *Responses* protocol on `POST {base}/responses` —
//! exactly what the official CLI sends for every request (see
//! `providers::opencode::uses_responses`).
//!
//! This module builds that request body and parses its event stream into the
//! same [`StreamEvent`] vocabulary the agent loop consumes, so those models
//! behave like every other provider: `output_text` deltas become
//! [`StreamEvent::Text`], reasoning deltas become [`StreamEvent::Thinking`],
//! finished `function_call` items become [`StreamEvent::ToolCall`], and the
//! terminal `response.completed` carries the usage for a single
//! [`StreamEvent::Done`].

use std::collections::{HashMap, HashSet, VecDeque};

use futures::{Stream, StreamExt, stream};
use serde_json::Value;

use crate::provider::{
    ChatMessage, ChatRequest, LlmError, StreamEvent, StreamResult, ToolCall, Usage,
};
use crate::wire::{Protocol, map_tools};

/// Build an OpenAI Responses request body for `req`.
///
/// Shape follows the official CLI capture: `input` (system prompt as the
/// `developer` role, flat items for tool traffic), flat `tools[]`, `stream`,
/// `store: false` and `max_output_tokens`.
pub fn responses_body(req: &ChatRequest, model: &str) -> Value {
    // The Zen free-tier gate also applies to /responses: it matches literal
    // gate tool *names* (`bash` + `read`) in `tools[]`, in the Responses flat
    // shape (`tools[].name`), so tool-less requests get the same inert stub
    // padding as the Chat Completions path (`openai_body`).
    let mut tools: Vec<Value> = map_tools(&req.tools, Protocol::OpenAi)
        .into_iter()
        .map(flatten_tool)
        .collect();
    let original_tool_count = tools.len();
    let mut tool_choice_none = false;
    if req.model.starts_with("opencode/") {
        let names: HashSet<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        let mut missing: Vec<&str> = ["bash", "read"]
            .into_iter()
            .filter(|n| !names.contains(n))
            .collect();
        if !missing.is_empty() {
            missing.sort();
            for name in missing {
                tools.push(serde_json::json!({
                    "type": "function",
                    "name": name,
                    "description": "",
                    "parameters": { "type": "object" },
                    "strict": false,
                }));
            }
            // Pin only tool-less auxiliary calls, mirroring `openai_body`.
            tool_choice_none = original_tool_count == 0;
        }
    }

    let mut body = serde_json::json!({
        "model": model,
        "input": to_responses_input(&req.messages, &req.system),
        "tools": tools,
        "stream": true,
        "store": false,
        "max_output_tokens": req.max_tokens,
        // First-party fingerprint the official CLI (ai-sdk) always sends:
        // encrypted reasoning content for multi-turn reasoning continuity.
        // The gateway uses its presence as a first-party signal — requests
        // without it get `403 FreeTierError` even with correct headers.
        "include": ["reasoning.encrypted_content"],
    });
    if tool_choice_none && let Value::Object(map) = &mut body {
        map.insert("tool_choice".to_string(), Value::String("none".to_string()));
    }
    // The CLI always sends `reasoning`, even when it wants no reasoning:
    // `{"effort":"none","summary":"auto"}` (see the title request in the
    // `OPENCODE_DEBUG_REQUEST=1` capture). Omitting the block reads as a
    // non-CLI client to the gateway, so `Thinking::Off` maps to
    // `effort:none` instead of omitting the field.
    if let Value::Object(map) = &mut body {
        let effort = req.thinking.openai_effort().unwrap_or("none");
        map.insert(
            "reasoning".to_string(),
            serde_json::json!({ "effort": effort, "summary": "auto" }),
        );
    }
    // Sampling: only `temperature`/`top_p` exist in the Responses schema —
    // `frequency_penalty`/`presence_penalty`/`seed` are Chat Completions
    // parameters and would be rejected as unknown fields.
    if let Value::Object(map) = &mut body {
        let s = &req.sampling;
        if let Some(t) = s.temperature {
            map.insert(
                "temperature".to_string(),
                serde_json::json!(t.clamp(0.0, 2.0)),
            );
        }
        if let Some(p) = s.top_p {
            map.insert("top_p".to_string(), serde_json::json!(p.clamp(0.0, 1.0)));
        }
    }
    body
}

/// Convert one nested Chat Completions tool (`{"type":"function","function":…}`)
/// into the flat Responses tool shape.
fn flatten_tool(tool: Value) -> Value {
    let f = tool.get("function").cloned().unwrap_or(Value::Null);
    let mut out = serde_json::json!({
        "type": "function",
        "name": f.get("name").cloned().unwrap_or(Value::Null),
        "description": f.get("description").cloned().unwrap_or(Value::Null),
        "parameters": f.get("parameters").cloned().unwrap_or(Value::Null),
    });
    if tool.get("strict").is_some_and(|s| *s == Value::Bool(true))
        && let Value::Object(map) = &mut out
    {
        map.insert("strict".to_string(), Value::Bool(true));
    }
    out
}

/// Convert provider-neutral messages into the Responses `input` item list.
///
/// System prompt first as a `developer` role (what the CLI sends), tool
/// results as `function_call_output` items and prior assistant tool calls as
/// `function_call` items — the protocol keeps them as siblings in `input`
/// instead of nesting them inside an assistant message.
pub fn to_responses_input(msgs: &[ChatMessage], system: &str) -> Vec<Value> {
    let mut out = Vec::with_capacity(msgs.len() + 1);
    if !system.is_empty() {
        out.push(serde_json::json!({ "role": "developer", "content": system }));
    }
    for m in msgs {
        if !m.tool_results.is_empty() {
            for tr in &m.tool_results {
                out.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": tr.tool_use_id,
                    "output": tr.content,
                }));
            }
            // Never drop the accompanying user content (text and/or images).
            if !m.content.is_empty() || !m.content_parts.is_empty() {
                out.push(serde_json::json!({
                    "role": m.role.as_str(),
                    "content": responses_content(m),
                }));
            }
            continue;
        }
        if m.role.as_str() == "assistant" && !m.tool_calls.is_empty() {
            if !m.content.is_empty() || !m.content_parts.is_empty() {
                out.push(serde_json::json!({
                    "role": "assistant",
                    "content": responses_content(m),
                }));
            }
            for tc in &m.tool_calls {
                out.push(serde_json::json!({
                    "type": "function_call",
                    "call_id": tc.id,
                    "name": tc.name,
                    "arguments": serde_json::to_string(&tc.input).unwrap_or_else(|_| "{}".into()),
                }));
            }
            continue;
        }
        out.push(serde_json::json!({
            "role": m.role.as_str(),
            "content": responses_content(m),
        }));
    }
    out
}

/// One `content` value for the Responses input: a plain string for text-only
/// messages, otherwise an array of `input_text` / `input_image` parts.
fn responses_content(m: &ChatMessage) -> Value {
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
        parts.push(serde_json::json!({ "type": "input_text", "text": m.content }));
    }
    for p in &m.content_parts {
        match p {
            ContentPart::Text { text } => {
                parts.push(serde_json::json!({ "type": "input_text", "text": text }));
            }
            ContentPart::Image { media_type, data } => {
                parts.push(serde_json::json!({
                    "type": "input_image",
                    "image_url": format!("data:{media_type};base64,{data}"),
                }));
            }
        }
    }
    Value::Array(parts)
}

/// Stream a Responses SSE response into typed events (same finalisation
/// contract as [`crate::openai_stream`]: exactly one `Done` per stream).
pub fn responses_stream(resp: reqwest::Response) -> impl Stream<Item = StreamResult<StreamEvent>> {
    let byte_stream = resp.bytes_stream();
    let parser = ResponsesParser::new();
    stream::unfold(
        (byte_stream, parser),
        move |(mut byte_stream, mut parser)| async move {
            loop {
                if let Some(item) = parser.next_ready() {
                    return Some((item, (byte_stream, parser)));
                }
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
                        parser.finish();
                    }
                }
            }
        },
    )
}

/// Parse a complete (already buffered) Responses SSE body into typed events,
/// running the same state machine as [`responses_stream`]. `chunk_size` splits
/// the body into byte chunks so tests can prove chunk-boundary insensitivity.
pub fn parse_responses_sse(
    body: &str,
    chunk_size: Option<usize>,
) -> Vec<StreamResult<StreamEvent>> {
    let mut parser = ResponsesParser::new();
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

/// Accumulated state for one in-flight `function_call` item.
#[derive(Default)]
struct PendingTool {
    call_id: String,
    name: String,
    arguments: String,
}

/// Incremental parser for the Responses SSE event stream.
///
/// Same finalisation contract as the Chat Completions parser (bug B13):
/// **every** stream emits exactly one [`StreamEvent::Done`] — with real usage
/// when `response.completed` carried it, zeroed best-effort otherwise (a
/// stream cut off mid tool call still owes the call *and* a Done).
struct ResponsesParser {
    line_buf: Vec<u8>,
    ready: VecDeque<StreamResult<StreamEvent>>,
    /// In-flight `function_call` items, keyed by the Responses item id.
    pending: HashMap<String, PendingTool>,
    usage_in: u64,
    usage_out: u64,
    cache_read: Option<u64>,
    done_emitted: bool,
    finished: bool,
}

impl ResponsesParser {
    fn new() -> Self {
        Self {
            line_buf: Vec::new(),
            ready: VecDeque::new(),
            pending: HashMap::new(),
            usage_in: 0,
            usage_out: 0,
            cache_read: None,
            done_emitted: false,
            finished: false,
        }
    }

    fn next_ready(&mut self) -> Option<StreamResult<StreamEvent>> {
        self.ready.pop_front()
    }

    fn is_finished(&self) -> bool {
        self.finished
    }

    fn emit_done(&mut self) {
        if self.done_emitted {
            return;
        }
        self.done_emitted = true;
        self.ready.push_back(Ok(StreamEvent::Done(Usage {
            input_tokens: self.usage_in,
            output_tokens: self.usage_out,
            cost: None,
            cache_read_input_tokens: self.cache_read,
            cache_creation_input_tokens: None,
        })));
    }

    /// Queue every in-flight `function_call` as a ToolCall (EOF safety net:
    /// a stream cut off between `output_item.added` and `output_item.done`
    /// still delivers the call with whatever arguments arrived).
    fn flush_tools(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let mut keys: Vec<String> = self.pending.keys().cloned().collect();
        keys.sort();
        for key in keys {
            let slot = self.pending.remove(&key).unwrap_or_default();
            if slot.call_id.is_empty() && slot.name.is_empty() {
                continue;
            }
            self.ready
                .push_back(Ok(StreamEvent::ToolCall(tool_call(slot))));
        }
    }

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
            return; // `event:` lines and blanks carry no payload.
        };
        let data = data.trim_start();
        if data.is_empty() {
            return;
        }
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
        let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match kind {
            "response.output_text.delta" => {
                if let Some(delta) = v.get("delta").and_then(|d| d.as_str())
                    && !delta.is_empty()
                {
                    self.ready
                        .push_back(Ok(StreamEvent::Text(delta.to_string())));
                }
            }
            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                if let Some(delta) = v.get("delta").and_then(|d| d.as_str())
                    && !delta.is_empty()
                {
                    self.ready
                        .push_back(Ok(StreamEvent::Thinking(delta.to_string())));
                }
            }
            "response.output_item.added" => {
                let item = v.get("item");
                if item.and_then(|i| i.get("type")).and_then(|t| t.as_str())
                    == Some("function_call")
                {
                    let key = pending_key(item);
                    let slot = self.pending.entry(key).or_default();
                    fill_call(slot, item);
                }
            }
            "response.function_call_arguments.delta" => {
                let item_id = v
                    .get("item_id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(delta) = v.get("delta").and_then(|d| d.as_str()) {
                    let slot = self.pending.entry(item_id).or_default();
                    slot.arguments.push_str(delta);
                }
            }
            "response.output_item.done" => {
                let item = v.get("item");
                if item.and_then(|i| i.get("type")).and_then(|t| t.as_str())
                    == Some("function_call")
                {
                    let key = pending_key(item);
                    let slot = self.pending.entry(key.clone()).or_default();
                    fill_call(slot, item);
                    if let Some(args) = item
                        .and_then(|i| i.get("arguments"))
                        .and_then(|a| a.as_str())
                        && !args.is_empty()
                    {
                        slot.arguments = args.to_string();
                    }
                    let slot = self.pending.remove(&key).unwrap_or_default();
                    if !slot.call_id.is_empty() || !slot.name.is_empty() {
                        self.ready
                            .push_back(Ok(StreamEvent::ToolCall(tool_call(slot))));
                    }
                }
            }
            "response.completed" | "response.incomplete" => {
                if let Some(usage) = v.pointer("/response/usage") {
                    self.usage_in = usage
                        .get("input_tokens")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0);
                    self.usage_out = usage
                        .get("output_tokens")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0);
                    self.cache_read = usage
                        .pointer("/input_tokens_details/cached_tokens")
                        .and_then(|x| x.as_u64());
                }
                self.flush_tools();
                self.emit_done();
            }
            "response.failed" => {
                let message = v
                    .pointer("/response/error/message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Responses stream failed");
                self.ready
                    .push_back(Err(LlmError::Stream(message.to_string())));
                self.flush_tools();
                self.emit_done();
            }
            "error" => {
                let message = v
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Responses stream error");
                self.ready
                    .push_back(Err(LlmError::Stream(message.to_string())));
                self.flush_tools();
                self.emit_done();
            }
            _ => {}
        }
    }
}

/// Item-id key for a pending `function_call` (falls back to the call id, then
/// the shared empty bucket).
fn pending_key(item: Option<&Value>) -> String {
    item.and_then(|i| i.get("id"))
        .and_then(|i| i.as_str())
        .or_else(|| item.and_then(|i| i.get("call_id")).and_then(|c| c.as_str()))
        .unwrap_or("")
        .to_string()
}

/// Copy identifying fields of a `function_call` item into its pending slot.
fn fill_call(slot: &mut PendingTool, item: Option<&Value>) {
    if let Some(name) = item.and_then(|i| i.get("name")).and_then(|n| n.as_str())
        && !name.is_empty()
    {
        slot.name = name.to_string();
    }
    if let Some(call_id) = item
        .and_then(|i| i.get("call_id"))
        .and_then(|c| c.as_str())
        .or_else(|| item.and_then(|i| i.get("id")).and_then(|i| i.as_str()))
        && !call_id.is_empty()
    {
        slot.call_id = call_id.to_string();
    }
}

fn tool_call(slot: PendingTool) -> ToolCall {
    let input = if slot.arguments.trim().is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(&slot.arguments).unwrap_or(Value::String(slot.arguments))
    };
    ToolCall {
        id: slot.call_id,
        name: slot.name,
        input,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatRole, Sampling, Thinking, ToolDef, ToolResult};

    fn req(model: &str) -> ChatRequest {
        ChatRequest {
            model: model.to_string(),
            system: "sys".to_string(),
            messages: vec![ChatMessage::user("hi")],
            tools: Vec::new(),
            max_tokens: 128,
            thinking: Thinking::Off,
            sampling: Sampling::default(),
        }
    }

    fn tool_request(model: &str) -> ChatRequest {
        let mut request = req(model);
        request.tools = vec![ToolDef {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
        }];
        request
    }

    #[test]
    fn body_follows_the_cli_shape() {
        let body = responses_body(&req("opencode/muse-spark-1.3-contributor-free"), "muse");
        assert_eq!(body["model"], "muse");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["max_output_tokens"], 128);
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["input"][0]["role"], "developer");
        assert_eq!(body["input"][0]["content"], "sys");
        assert_eq!(body["input"][1]["role"], "user");
        assert_eq!(body["input"][1]["content"], "hi");
        // First-party fingerprint the CLI always sends (title request in the
        // `OPENCODE_DEBUG_REQUEST=1` capture): encrypted reasoning content
        // plus an explicit `reasoning` block even when no reasoning is
        // wanted (`effort:none`).
        assert_eq!(
            body["include"],
            serde_json::json!(["reasoning.encrypted_content"])
        );
        assert_eq!(body["reasoning"]["effort"], "none");
        assert_eq!(body["reasoning"]["summary"], "auto");
    }

    #[test]
    fn body_pads_tool_less_requests_with_inert_gate_stubs() {
        let body = responses_body(&req("opencode/muse-spark-1.2-contributor-free"), "muse");
        let names: Vec<&str> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(names, vec!["bash", "read"]);
        // Flat Responses shape, no nested `function` object.
        assert!(body["tools"][0].get("function").is_none());
        // Tool-less auxiliary call: stubs pinned inert.
        assert_eq!(body["tool_choice"], "none");
    }

    #[test]
    fn body_keeps_real_tools_unpinned() {
        let body = responses_body(
            &tool_request("opencode/longcat-2.5-preview-free"),
            "longcat",
        );
        let names: Vec<&str> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        // Real tool kept, gate stubs appended, real tool unpinned.
        assert_eq!(names, vec!["read_file", "bash", "read"]);
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn body_maps_thinking_and_sampling_responses_style() {
        let mut request = req("opencode/muse-spark-1.3-contributor-free");
        request.thinking = Thinking::High;
        request.sampling = Sampling {
            temperature: Some(0.7),
            top_p: Some(0.9),
            frequency_penalty: Some(0.5),
            presence_penalty: Some(-0.5),
            seed: Some(42),
            top_k: Some(40),
        };
        let body = responses_body(&request, "muse");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert!((body["temperature"].as_f64().unwrap() - 0.7).abs() < 1e-6);
        assert!((body["top_p"].as_f64().unwrap() - 0.9).abs() < 1e-6);
        // Chat-only params must not leak into the Responses body.
        for key in [
            "frequency_penalty",
            "presence_penalty",
            "seed",
            "top_k",
            "reasoning_effort",
        ] {
            assert!(body.get(key).is_none(), "{key} must be omitted");
        }
    }

    #[test]
    fn input_maps_tool_traffic_to_flat_items() {
        let mut request = req("opencode/muse-spark-1.3-contributor-free");
        request.messages = vec![
            ChatMessage {
                role: ChatRole::Assistant,
                content: "calling".to_string(),
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"cmd": "ls"}),
                }],
                tool_results: vec![],
                content_parts: vec![],
            },
            ChatMessage {
                role: ChatRole::User,
                content: String::new(),
                tool_calls: vec![],
                tool_results: vec![ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "files".to_string(),
                    is_error: false,
                }],
                content_parts: vec![],
            },
        ];
        let input = to_responses_input(&request.messages, &request.system);
        assert_eq!(input.len(), 4);
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[1]["content"], "calling");
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call_1");
        assert_eq!(input[2]["name"], "bash");
        assert_eq!(input[2]["arguments"], r#"{"cmd":"ls"}"#);
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["call_id"], "call_1");
        assert_eq!(input[3]["output"], "files");
    }

    const HAPPY: &str = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\"}\n\n",
        "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"bash\",\"arguments\":\"\"}}\n\n",
        "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}\n\n",
        "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"bash\",\"arguments\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hel\"}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"lo\"}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"input_tokens_details\":{\"cached_tokens\":3}}}}\n\n",
    );

    fn kinds(events: &[StreamResult<StreamEvent>]) -> Vec<&'static str> {
        events
            .iter()
            .map(|e| match e.as_ref().expect("event") {
                StreamEvent::Text(_) => "text",
                StreamEvent::Thinking(_) => "thinking",
                StreamEvent::ToolCall(_) => "tool",
                StreamEvent::Done(_) => "done",
            })
            .collect()
    }

    #[test]
    fn sse_parses_text_tool_call_and_single_done() {
        for chunk in [None, Some(7)] {
            let events = parse_responses_sse(HAPPY, chunk);
            assert_eq!(
                kinds(&events),
                vec!["tool", "text", "text", "done"],
                "chunked parse must match (chunk={chunk:?})"
            );
            let StreamEvent::ToolCall(call) = events[0].as_ref().unwrap() else {
                panic!("first event must be the tool call");
            };
            assert_eq!(call.id, "call_1");
            assert_eq!(call.name, "bash");
            assert_eq!(call.input, serde_json::json!({"cmd": "ls"}));
            let StreamEvent::Done(usage) = events[3].as_ref().unwrap() else {
                panic!("last event must be Done");
            };
            assert_eq!(usage.input_tokens, 10);
            assert_eq!(usage.output_tokens, 5);
            assert_eq!(usage.cache_read_input_tokens, Some(3));
        }
    }

    #[test]
    fn truncated_stream_still_delivers_tool_call_and_done() {
        // Cut right after `output_item.added` + a partial argument delta: the
        // finalisation contract still owes a ToolCall and exactly one Done.
        let body = concat!(
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_9\",\"call_id\":\"call_9\",\"name\":\"bash\",\"arguments\":\"\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_9\",\"delta\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}\n\n",
        );
        let events = parse_responses_sse(body, None);
        assert_eq!(kinds(&events), vec!["tool", "done"]);
        let StreamEvent::ToolCall(call) = events[0].as_ref().unwrap() else {
            panic!("first event must be the tool call");
        };
        assert_eq!(call.id, "call_9");
        assert_eq!(call.name, "bash");
        assert_eq!(call.input, serde_json::json!({"cmd": "ls"}));
        let StreamEvent::Done(usage) = events[1].as_ref().unwrap() else {
            panic!("last event must be Done");
        };
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
    }

    #[test]
    fn failed_stream_queues_error_then_single_done() {
        let body = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"upstream died\"}}}\n\n";
        let events = parse_responses_sse(body, None);
        assert_eq!(events.len(), 2);
        let Err(LlmError::Stream(message)) = &events[0] else {
            panic!("first event must be the stream error");
        };
        assert_eq!(message, "upstream died");
        assert!(matches!(events[1], Ok(StreamEvent::Done(_))));
    }
}
