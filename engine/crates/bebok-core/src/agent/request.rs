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
}
