//! LLM provider abstraction and clients.
//!
//! [`Provider`] is the streaming interface the agent loop consumes; each
//! provider parses its own SSE format (there is no shared event format).

mod anthropic;
mod cost;
mod openai;
mod provider;
mod spec;
mod wire;
mod zai;

pub use anthropic::{ANTHROPIC_MESSAGES_URL, AnthropicProvider, to_anthropic_messages};
pub use cost::{Pricing, compute_cost, pricing_for};
pub use openai::{
    OpenAiProvider, openai_body, openai_stream, parse_openai_sse, to_openai_messages,
};
pub use provider::{
    ChatMessage, ChatRequest, ChatRole, ContentPart, LlmError, Provider, ProviderErrorKind,
    StreamEvent, StreamResult, Thinking, ToolCall, ToolDef, ToolResult, Usage, classify_http,
    retry_after_from_headers,
};
pub use spec::{
    ProviderAuth, ProviderExtraField, ProviderFieldType, ProviderKind, ProviderSpec, ProviderUiSpec,
    builtin_provider_specs, find_provider_spec, list_models, provider_catalog, provider_ui_spec,
    resolve_api_key, resolve_provider_specs,
};
pub use wire::{Protocol, map_content_part, map_image_parts, map_tool, map_tools};
pub use zai::ZaiProvider;
