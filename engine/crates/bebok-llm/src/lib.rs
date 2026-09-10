//! LLM provider abstraction and clients.
//!
//! [`Provider`] is the streaming interface the agent loop consumes; each
//! provider parses its own SSE format (there is no shared event format).

mod anthropic;
mod cost;
mod openai;
mod provider;
mod spec;
mod zai;

pub use anthropic::{ANTHROPIC_MESSAGES_URL, AnthropicProvider, to_anthropic_messages};
pub use cost::{Pricing, compute_cost, pricing_for};
pub use openai::{OpenAiProvider, openai_body, openai_stream, to_openai_messages};
pub use provider::{
    ChatMessage, ChatRequest, ChatRole, ContentPart, LlmError, Provider, StreamEvent, StreamResult,
    Thinking, ToolCall, ToolDef, ToolResult, Usage,
};
pub use spec::{
    ProviderKind, ProviderSpec, builtin_provider_specs, find_provider_spec, list_models,
    resolve_api_key, resolve_provider_specs,
};
pub use zai::ZaiProvider;
