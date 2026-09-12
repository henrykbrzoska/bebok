//! Request builder (Builder): transcript -> provider `ChatRequest`.
//!
//! `RequestBuilder::build` is the single assembly point for prompt +
//! transcript + tool defs + budget pruning. Config/plugin prompt overrides
//! (editable prompts) hook in here: they transform the assembled `system`
//! text / messages before the request is returned, without touching the
//! persisted transcript.

use std::collections::HashSet;

use bebok_llm::{ChatMessage, ChatRequest, ChatRole, ContentPart, Thinking, ToolDef, ToolResult};
use bebok_tools::ToolRegistry;

use super::preset::Agent;
use crate::error::{CoreError, Result};
use crate::plugin::{Hook, PluginHost, RequestHook, RequestMessage};
use crate::session::{Role, ToolState};
use crate::store::SessionState;

/// Builder for provider requests (transcript + system prompt + tool defs).
pub struct RequestBuilder<'a> {
    pub state: &'a SessionState,
    pub agent: &'a Agent,
    pub tools: &'a ToolRegistry,
    pub model: &'a str,
    pub max_tokens: u32,
    pub thinking: Thinking,
}

impl<'a> RequestBuilder<'a> {
    pub fn new(
        state: &'a SessionState,
        agent: &'a Agent,
        tools: &'a ToolRegistry,
        model: &'a str,
        max_tokens: u32,
        thinking: Thinking,
    ) -> Self {
        Self {
            state,
            agent,
            tools,
            model,
            max_tokens,
            thinking,
        }
    }

    /// Build the provider request from the session transcript.
    pub async fn build(&self) -> Result<ChatRequest> {
        let messages = self.state.messages_snapshot().await;
        let mut chat: Vec<ChatMessage> = Vec::new();

        for msg in messages.iter() {
            match msg.role {
                Role::User => {
                    let text = msg.text_content();
                    let content_parts: Vec<ContentPart> = msg
                        .image_parts()
                        .into_iter()
                        .map(|(media_type, data, _name)| ContentPart::Image { media_type, data })
                        .collect();
                    // A user message may carry tool results produced right before
                    // it in the same turn; normally results are attached below.
                    chat.push(ChatMessage {
                        role: ChatRole::User,
                        content: text,
                        tool_calls: Vec::new(),
                        tool_results: Vec::new(),
                        content_parts,
                    });
                }
                Role::Assistant => {
                    let content = msg.text_content();
                    let mut tool_calls = Vec::new();
                    let mut tool_results = Vec::new();
                    for part in &msg.parts {
                        if let crate::session::Part::Tool { id, name, state } = part {
                            tool_calls.push(bebok_llm::ToolCall {
                                id: id.clone(),
                                name: name.clone(),
                                input: state.input().clone(),
                            });
                            match state {
                                ToolState::Completed { output, .. } => {
                                    tool_results.push(ToolResult {
                                        tool_use_id: id.clone(),
                                        content: output.clone(),
                                        is_error: false,
                                    });
                                }
                                ToolState::Error { error, .. } => {
                                    tool_results.push(ToolResult {
                                        tool_use_id: id.clone(),
                                        content: error.clone(),
                                        is_error: true,
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                    chat.push(ChatMessage {
                        role: ChatRole::Assistant,
                        content,
                        tool_calls,
                        tool_results: Vec::new(),
                        content_parts: Vec::new(),
                    });
                    if !tool_results.is_empty() {
                        chat.push(ChatMessage {
                            role: ChatRole::User,
                            content: String::new(),
                            tool_calls: Vec::new(),
                            tool_results,
                            content_parts: Vec::new(),
                        });
                    }
                }
            }
        }

        if chat.is_empty() {
            return Err(CoreError::Other("empty conversation".to_string()));
        }

        // --- Lazy tool definitions (token saving #7) ---
        // Count which tools the model has actually called in this session. Tools
        // that have never been called get a short stub description; tools that
        // were used get the full description. This saves ~100-300 tokens per tool
        // for unused tools (e.g. fs_tree, fs_file, debug_log, mcp list are
        // rarely needed in a simple code-editing session).
        let used_tools = collect_used_tools(&messages);
        let tool_defs: Vec<ToolDef> = self
            .tools
            .list()
            .iter()
            .filter(|t| {
                // The `fleet` fan-out tool is orchestrator-only: withheld from
                // every other agent so parallel fleets are never user-triggered
                // or spawned by a sub-agent.
                if t.name() == "fleet" && self.agent.name != "orchestrator" {
                    return false;
                }
                self.agent.tools.is_empty() || self.agent.tools.iter().any(|n| n == t.name())
            })
            .map(|t| {
                let used = used_tools.contains(t.name());
                ToolDef {
                    name: t.name().to_string(),
                    description: if used {
                        t.description().to_string()
                    } else {
                        // Short stub: just name + one-liner.
                        let desc = t.description();
                        let short: String = desc.chars().take(80).collect();
                        format!("{short} [available; call to use]")
                    },
                    input_schema: t.parameters_schema(),
                }
            })
            .collect();

        // Context management (§3.9): if the transcript exceeds the token budget,
        // prune old tool outputs down to a compact digest (non-destructive: the
        // full history stays on disk).
        let budget = self.state.config_snapshot().context_budget;
        let chat = prune_for_budget(chat, &self.agent.prompt, budget);

        Ok(ChatRequest {
            model: self.model.to_string(),
            system: self.agent.prompt.clone(),
            messages: chat,
            tools: tool_defs,
            max_tokens: self.max_tokens,
            thinking: self.thinking,
        })
    }

    /// Fire the `before.request` plugin hook and apply the (possibly mutated)
    /// system prompt back. Only the system prompt is applied back (messages
    /// stay canonical on disk).
    pub async fn apply_request_hook(&self, req: &mut ChatRequest) {
        let hooks = PluginHost::global();
        if hooks.has_plugins().await {
            let mut payload = RequestHook::new(
                self.model,
                req.system.clone(),
                req.messages.iter().map(hook_request_message).collect(),
            );
            hooks.run_hook(Hook::BEFORE_REQUEST, &mut payload).await;
            req.system = payload.system;
        }
    }
}

/// Build the provider request from the session transcript (compat shim over
/// `RequestBuilder::build`; the turn loop calls the builder directly).
pub async fn build_request(
    state: &SessionState,
    agent: &Agent,
    tools: &ToolRegistry,
    model: &str,
    max_tokens: u32,
    thinking: Thinking,
) -> Result<ChatRequest> {
    RequestBuilder::new(state, agent, tools, model, max_tokens, thinking)
        .build()
        .await
}

/// Collect the set of tool names that have been used in the session.
/// Used for lazy tool definitions: previously-used tools get full descriptions,
/// unused tools get short stubs.
fn collect_used_tools(messages: &[crate::session::Message]) -> HashSet<String> {
    let mut used = HashSet::new();
    for msg in messages {
        for part in &msg.parts {
            if let crate::session::Part::Tool { name, .. } = part {
                used.insert(name.clone());
            }
        }
    }
    used
}

/// Replace old tool results with a compact digest (oldest first) until the
/// built request fits the token budget. Never mutates the persisted transcript.
///
/// Instead of bare `[truncated]`, uses `summarize_tool_output` to preserve
/// the first/last lines and error markers so the model retains context about
/// what each tool returned.
pub fn prune_for_budget(
    mut chat: Vec<ChatMessage>,
    system: &str,
    budget: usize,
) -> Vec<ChatMessage> {
    let mut tokens = crate::context::estimate_chat(&chat, system);
    if tokens <= budget {
        return chat;
    }

    // Collect indices of tool results that can be pruned (oldest first).
    let mut candidates: Vec<(usize, usize)> = Vec::new(); // (message_idx, result_idx)
    for (i, msg) in chat.iter().enumerate() {
        for (j, tr) in msg.tool_results.iter().enumerate() {
            if tr.content != "[truncated]" {
                candidates.push((i, j));
            }
        }
    }

    for (i, j) in candidates {
        if tokens <= budget {
            break;
        }
        let old_content = &chat[i].tool_results[j].content;
        let before_tokens = crate::context::estimate_tokens(old_content);

        // Produce a compact digest that preserves semantic context.
        let tool_name = extract_tool_name(&chat, i, &chat[i].tool_results[j].tool_use_id);
        let digest = crate::context::summarize_tool_output(&tool_name, old_content);
        let after_tokens = crate::context::estimate_tokens(&digest);

        chat[i].tool_results[j].content = digest;
        tokens = tokens.saturating_sub(before_tokens) + after_tokens;
    }

    // Images are the single most expensive entries (flat IMAGE_TOKENS_PER_IMAGE
    // each) and used to be unprunable, so an image-heavy transcript could blow
    // the budget forever. Prune oldest-first, replacing each image with a text
    // marker: the model is told an image was dropped, nothing vanishes silently,
    // and the newest attachment survives whenever the budget allows.
    let image_candidates: Vec<(usize, usize)> = chat
        .iter()
        .enumerate()
        .flat_map(|(i, msg)| {
            msg.content_parts
                .iter()
                .enumerate()
                .filter(|(_, p)| matches!(p, ContentPart::Image { .. }))
                .map(move |(j, _)| (i, j))
        })
        .collect();

    for (i, j) in image_candidates {
        if tokens <= budget {
            break;
        }
        if let ContentPart::Image { media_type, .. } = &chat[i].content_parts[j] {
            let marker = format!("[image omitted to fit the context budget: {media_type}]");
            tokens = tokens.saturating_sub(crate::context::IMAGE_TOKENS_PER_IMAGE)
                + crate::context::estimate_tokens(&marker);
            chat[i].content_parts[j] = ContentPart::Text { text: marker };
        }
    }

    chat
}

/// Find the tool name for a given tool_use_id in the chat history.
fn extract_tool_name(chat: &[ChatMessage], result_msg_idx: usize, tool_use_id: &str) -> String {
    // The tool call is in the assistant message *before* the results message.
    for i in (0..=result_msg_idx).rev() {
        for tc in &chat[i].tool_calls {
            if tc.id == tool_use_id {
                return tc.name.clone();
            }
        }
    }
    "unknown".to_string()
}

/// Convert a provider message into the (lightweight) plugin request view.
pub(crate) fn hook_request_message(m: &ChatMessage) -> RequestMessage {
    RequestMessage {
        role: m.role.as_str().to_string(),
        text: m.content.clone(),
        tool_calls: m.tool_calls.len(),
        tool_results: m.tool_results.len(),
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;
    use crate::session::{Message, Part};
    use bebok_tools::ToolRegistry;

    fn test_state<'a>(
        store: &'a crate::store::InstanceStore,
        session: &'a SessionState,
    ) -> (&'a SessionState, Agent, ToolRegistry) {
        let _ = store;
        let agent = Agent::code();
        let tools = ToolRegistry::new(Vec::new());
        (session, agent, tools)
    }

    #[tokio::test]
    async fn builder_maps_user_images_to_content_parts() {
        let base = std::env::temp_dir().join(format!("bebok-img-req-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let store = crate::store::InstanceStore::with_data_dir(base.join("data"));
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session
            .append_user_message_with_images(
                "look",
                vec![Part::Image {
                    media_type: "image/png".to_string(),
                    data: "aGVsbG8=".to_string(),
                    name: Some("shot.png".to_string()),
                }],
            )
            .await
            .unwrap();

        let (_s, agent, tools) = test_state(&store, &session);
        let builder = RequestBuilder::new(&session, &agent, &tools, "m", 128, Thinking::Off);
        let req = builder.build().await.unwrap();
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].content, "look");
        assert_eq!(req.messages[0].content_parts.len(), 1);
        assert_eq!(
            req.messages[0].content_parts[0],
            ContentPart::Image {
                media_type: "image/png".to_string(),
                data: "aGVsbG8=".to_string()
            }
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn builder_text_only_has_no_content_parts() {
        let base = std::env::temp_dir().join(format!("bebok-txt-req-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let store = crate::store::InstanceStore::with_data_dir(base.join("data"));
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session.append_user_message("hi").await.unwrap();

        let (_s, agent, tools) = test_state(&store, &session);
        let builder = RequestBuilder::new(&session, &agent, &tools, "m", 128, Thinking::Off);
        let req = builder.build().await.unwrap();
        assert!(req.messages[0].content_parts.is_empty());
        let _ = Message::user("unused");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn prune_leaves_images_alone_within_budget() {
        let msgs = vec![ChatMessage {
            role: ChatRole::User,
            content: "x".to_string(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            content_parts: vec![ContentPart::Image {
                media_type: "image/png".to_string(),
                data: "aGVsbG8=".to_string(),
            }],
        }];
        let out = prune_for_budget(msgs, "", 100_000);
        assert!(matches!(out[0].content_parts[0], ContentPart::Image { .. }));
    }

    #[test]
    fn prune_replaces_oldest_images_with_marker_until_under_budget() {
        // 4 images * 1000 tokens = 4000 > 2500 budget: at least 2 are pruned.
        let msgs: Vec<ChatMessage> = (0..4)
            .map(|_| ChatMessage {
                role: ChatRole::User,
                content: "x".to_string(),
                tool_calls: Vec::new(),
                tool_results: Vec::new(),
                content_parts: vec![ContentPart::Image {
                    media_type: "image/png".to_string(),
                    data: "aGVsbG8=".to_string(),
                }],
            })
            .collect();
        let out = prune_for_budget(msgs, "", 2500);
        let images = out
            .iter()
            .flat_map(|m| m.content_parts.iter())
            .filter(|p| matches!(p, ContentPart::Image { .. }))
            .count();
        let markers = out
            .iter()
            .flat_map(|m| m.content_parts.iter())
            .filter(|p| matches!(p, ContentPart::Text { .. }))
            .count();
        assert!(images < 4, "expected pruned images, {images} left");
        assert!(markers >= 1, "dropped images must leave a marker");
        assert!(
            crate::context::estimate_chat(&out, "") <= 2500,
            "pruning must bring the request back under budget"
        );
        // Oldest first: the last message keeps its image.
        assert!(matches!(
            out.last().unwrap().content_parts[0],
            ContentPart::Image { .. }
        ));
    }

    #[test]
    fn prune_breaks_down_an_image_only_transcript() {
        // Even an oversized image-only transcript converges (no infinite loop,
        // no silent drop): every image becomes a text marker.
        let msgs: Vec<ChatMessage> = (0..10)
            .map(|_| ChatMessage {
                role: ChatRole::User,
                content: String::new(),
                tool_calls: Vec::new(),
                tool_results: Vec::new(),
                content_parts: vec![ContentPart::Image {
                    media_type: "image/jpeg".to_string(),
                    data: "aGVsbG8=".to_string(),
                }],
            })
            .collect();
        let out = prune_for_budget(msgs, "", 500);
        assert_eq!(out.len(), 10);
        assert_eq!(
            out.iter()
                .flat_map(|m| m.content_parts.iter())
                .filter(|p| matches!(p, ContentPart::Text { .. }))
                .count(),
            10
        );
    }

    /// End-to-end without a provider: a session transcript holding a PNG and a
    /// JPEG, built into a ChatRequest, must reach BOTH provider wire formats
    /// with the image payloads intact (covers repro hops 3 -> 6 together).
    #[tokio::test]
    async fn session_images_reach_both_provider_wire_formats() {
        use crate::agent::images::fixtures::{JPEG_MIN, PNG_1X1};
        let base = std::env::temp_dir().join(format!("bebok-wire-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let store = crate::store::InstanceStore::with_data_dir(base.join("data"));
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session
            .append_user_message_with_images(
                "what is in these?",
                vec![
                    Part::Image {
                        media_type: "image/png".to_string(),
                        data: PNG_1X1.to_string(),
                        name: Some("a.png".to_string()),
                    },
                    Part::Image {
                        media_type: "image/jpeg".to_string(),
                        data: JPEG_MIN.to_string(),
                        name: Some("b.jpg".to_string()),
                    },
                ],
            )
            .await
            .unwrap();

        let (_s, agent, tools) = test_state(&store, &session);
        let builder = RequestBuilder::new(&session, &agent, &tools, "m", 128, Thinking::Off);
        let req = builder.build().await.unwrap();
        assert_eq!(req.messages[0].content_parts.len(), 2);

        // OpenAI Chat Completions shape: text + two image_url data URLs.
        let openai = bebok_llm::to_openai_messages(&req.messages, &req.system);
        // The system message comes first when the agent prompt is non-empty.
        let openai_user = openai
            .iter()
            .find(|m| m["role"] == "user")
            .expect("user message present");
        let parts = openai_user["content"].as_array().unwrap();
        let urls: Vec<&str> = parts
            .iter()
            .filter_map(|p| p["image_url"]["url"].as_str())
            .collect();
        assert_eq!(urls.len(), 2, "{openai:?}");
        assert_eq!(urls[0], format!("data:image/png;base64,{PNG_1X1}"));
        assert_eq!(urls[1], format!("data:image/jpeg;base64,{JPEG_MIN}"));

        // Anthropic Messages shape: two image blocks followed by the text block.
        let anthropic = bebok_llm::to_anthropic_messages(&req.messages);
        let blocks = anthropic[0]["content"].as_array().unwrap();
        let types: Vec<&str> = blocks.iter().filter_map(|b| b["type"].as_str()).collect();
        assert_eq!(types, vec!["image", "image", "text"], "{anthropic:?}");
        assert_eq!(blocks[0]["source"]["media_type"], "image/png");
        assert_eq!(blocks[0]["source"]["data"], PNG_1X1);
        assert_eq!(blocks[1]["source"]["media_type"], "image/jpeg");
        assert_eq!(blocks[1]["source"]["data"], JPEG_MIN);

        let _ = std::fs::remove_dir_all(&base);
    }
}
