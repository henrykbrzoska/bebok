//! LLM provider abstraction and clients.
//!
//! [`Provider`] is the streaming interface the agent loop consumes; each
//! provider parses its own SSE format (there is no shared event format).

mod anthropic;
mod cache_policy;
mod cost;
mod model_catalog;
mod openai;
mod protocols;
mod provider;
mod providers;
mod spec;
mod wire;

pub use anthropic::{ANTHROPIC_MESSAGES_URL, AnthropicProvider, to_anthropic_messages};
pub use cache_policy::{CachePolicy, MIN_CACHE_PREFIX_BYTES};
pub use cost::{Pricing, compute_cost, pricing_for};
pub use model_catalog::{ModelCapabilities, ModelCatalog, ModelPricing};
pub use openai::{
    OpenAiProvider, openai_body, openai_stream, parse_openai_sse, to_openai_messages,
};
pub use protocols::{AnthropicMessagesAdapter, OpenAiChatAdapter};
pub use provider::{
    AdapterConfig, ChatMessage, ChatRequest, ChatRole, ContentPart, LlmError, Provider,
    ProviderAdapter, ProviderErrorKind, StreamEvent, StreamResult, Thinking, ToolCall, ToolDef,
    ToolResult, Usage, adapter_for, build_provider, classify_http, retry_after_from_headers,
};
pub use providers::adapter_config;
pub use spec::{
    ProviderAuth, ProviderExtraField, ProviderFieldType, ProviderKind, ProviderSpec,
    ProviderUiSpec, builtin_provider_specs, find_provider_spec, list_models, provider_catalog,
    provider_ui_spec, resolve_api_key, resolve_provider_specs,
};
pub use wire::{Protocol, map_content_part, map_image_parts, map_tool, map_tools};
