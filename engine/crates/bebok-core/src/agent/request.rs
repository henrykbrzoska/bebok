//! Request builder (Builder): transcript -> provider `ChatRequest`.
//!
//! `RequestBuilder::build` is the single assembly point for prompt +
//! transcript + tool defs + budget pruning. Config/plugin prompt overrides
//! (editable prompts) hook in here: they transform the assembled `system`
//! text / messages before the request is returned, without touching the
//! persisted transcript.

use bebok_llm::{ChatMessage, ChatRequest, ChatRole, Thinking, ToolDef, ToolResult};
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
                    // A user message may carry tool results produced right before
                    // it in the same turn; normally results are attached below.
                    chat.push(ChatMessage {
                        role: ChatRole::User,
                        content: text,
                        tool_calls: Vec::new(),
                        tool_results: Vec::new(),
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
                    });
                    if !tool_results.is_empty() {
                        chat.push(ChatMessage {
                            role: ChatRole::User,
                            content: String::new(),
                            tool_calls: Vec::new(),
                            tool_results,
                        });
                    }
                }
            }
        }

        if chat.is_empty() {
            return Err(CoreError::Other("empty conversation".to_string()));
        }

        let tool_defs: Vec<ToolDef> = self
            .tools
            .list()
            .iter()
            .filter(|t| {
                self.agent.tools.is_empty()
                    || self.agent.tools.iter().any(|n| n == t.name())
            })
            .map(|t| ToolDef {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.parameters_schema(),
            })
            .collect();

        // Context management (§3.9): if the transcript exceeds the token budget,
        // prune old tool outputs down to a `[truncated]` marker (non-destructive:
        // the full history stays on disk).
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

/// Replace old tool results with `[truncated]` (oldest first) until the built
/// request fits the token budget. Never mutates the persisted transcript.
pub fn prune_for_budget(
    mut chat: Vec<ChatMessage>,
    system: &str,
    budget: usize,
) -> Vec<ChatMessage> {
    let truncated_tokens = crate::context::estimate_tokens("[truncated]");
    let mut tokens = crate::context::estimate_chat(&chat, system);
    if tokens <= budget {
        return chat;
    }
    'outer: for i in 0..chat.len() {
        for j in 0..chat[i].tool_results.len() {
            if chat[i].tool_results[j].content == "[truncated]" {
                continue;
            }
            let before = crate::context::estimate_tokens(&chat[i].tool_results[j].content);
            chat[i].tool_results[j].content = "[truncated]".to_string();
            tokens = tokens.saturating_sub(before) + truncated_tokens;
            if tokens <= budget {
                break 'outer;
            }
        }
    }
    chat
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
