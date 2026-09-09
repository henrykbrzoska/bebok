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
};

/// Build an OpenAI Chat Completions request body.
pub fn openai_body(req: &ChatRequest, model: &str) -> Value {
    let tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                }
            })
        })
        .collect();

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
            map.insert("reasoning_effort".to_string(), Value::String(effort.to_string()));
        }
    }
    body
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
            // A bare user text can accompany tool results (rare; keep it).
            if !m.content.is_empty() {
                out.push(serde_json::json!({ "role": "user", "content": m.content }));
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
                "content": m.content,
                "tool_calls": tool_calls,
            }));
        } else {
            out.push(serde_json::json!({ "role": m.role.as_str(), "content": m.content }));
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

fn model_name(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
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
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Http { status, body: text });
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
                match byte_stream.next().await {
                    Some(Ok(bytes)) => parser.feed(&bytes),
                    Some(Err(e)) => {
                        let err = Err(LlmError::Stream(e.to_string()));
                        return Some((err, (byte_stream, parser)));
                    }
                    None => {
                        // Flush any pending tool calls at EOF.
                        if let Some(item) = parser.finish() {
                            return Some((item, (byte_stream, parser)));
                        }
                        return None;
                    }
                }
            }
        },
    )
}

/// Accumulated state for one in-flight tool call.
#[derive(Default)]
struct PendingTool {
    id: String,
    name: String,
    arguments: String,
}

struct OpenAiParser {
    line_buf: Vec<u8>,
    /// Buffered parsed events waiting to be yielded.
    ready: std::collections::VecDeque<StreamResult<StreamEvent>>,
    tools: HashMap<usize, PendingTool>,
    usage_in: u64,
    usage_out: u64,
}

impl OpenAiParser {
    fn new() -> Self {
        Self {
            line_buf: Vec::new(),
            ready: std::collections::VecDeque::new(),
            tools: HashMap::new(),
            usage_in: 0,
            usage_out: 0,
        }
    }

    fn next_ready(&mut self) -> Option<StreamResult<StreamEvent>> {
        self.ready.pop_front()
    }

    fn finish(&mut self) -> Option<StreamResult<StreamEvent>> {
        None
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
        if data.is_empty() || data == "[DONE]" {
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
        if let Some(usage) = v.get("usage") {
            self.usage_in = usage
                .get("prompt_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            self.usage_out = usage
                .get("completion_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
        }

        let Some(choice) = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
        else {
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
        if finish.is_some() && !self.tools.is_empty() {
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

        // Some servers send a final `[DONE]`-less completion; if finish_reason is
        // set and no tool calls, emit Done when the usage was seen.
        if finish.is_some() && self.tools.is_empty() && (self.usage_in > 0 || self.usage_out > 0) {
            self.ready.push_back(Ok(StreamEvent::Done(Usage {
                input_tokens: self.usage_in,
                output_tokens: self.usage_out,
                cost: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            })));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::provider::{ChatMessage, ChatRequest, Thinking};
    use super::openai_body;

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
