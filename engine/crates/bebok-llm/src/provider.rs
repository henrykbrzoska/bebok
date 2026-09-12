use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type StreamResult<T> = Result<T, LlmError>;

#[derive(Debug, Clone)]
pub struct AdapterConfig {
    pub provider_name: String,
    pub env_var: String,
    pub endpoint: String,
    pub api_key: Option<String>,
    pub headers: Vec<(String, String)>,
}

pub trait ProviderAdapter: Send + Sync {
    fn type_name(&self) -> &'static str;
    fn build(&self, config: AdapterConfig) -> Result<Arc<dyn Provider>, String>;
}

static OPENAI_CHAT_ADAPTER: crate::OpenAiChatAdapter = crate::OpenAiChatAdapter;
static ANTHROPIC_MESSAGES_ADAPTER: crate::AnthropicMessagesAdapter =
    crate::AnthropicMessagesAdapter;

pub fn adapter_for(kind: crate::ProviderKind) -> &'static dyn ProviderAdapter {
    match kind {
        crate::ProviderKind::Openai => &OPENAI_CHAT_ADAPTER,
        crate::ProviderKind::Anthropic => &ANTHROPIC_MESSAGES_ADAPTER,
    }
}

pub fn build_provider(
    spec: &crate::ProviderSpec,
    fallback_api_key: Option<String>,
) -> Result<Arc<dyn Provider>, String> {
    let api_key = crate::resolve_api_key(spec).or(fallback_api_key);
    let config = crate::adapter_config(spec, api_key);
    let adapter = adapter_for(spec.kind);
    adapter.build(config)
}

/// Errors surfaced by a provider.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("http error {status}: {body}")]
    Http {
        status: u16,
        body: String,
        /// `Retry-After` hint (seconds) supplied by the provider, if any.
        retry_after: Option<u64>,
    },
    #[error("stream error: {0}")]
    Stream(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("request error: {0}")]
    Request(#[from] reqwest::Error),
}

/// Vendor-neutral classification of a provider failure.
///
/// Both protocol implementations (OpenAI Chat Completions and Anthropic
/// Messages) funnel their HTTP failures through [`classify_http`], so callers
/// such as the retry wrapper in the agent loop can reason about "is this worth
/// retrying?" without knowing which vendor produced the error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    /// 429 / explicit rate-limit envelope. Transient.
    RateLimited,
    /// 5xx (incl. Anthropic's 529 "overloaded"). Transient.
    ServerError,
    /// Missing/invalid/forbidden credentials (401, 403, auth error envelopes).
    AuthError,
    /// The requested model is unknown, unavailable or not permitted.
    ModelError,
    /// The request itself was rejected (4xx that is not auth/model/rate limit).
    BadRequest,
    /// Transport-level failure (connect/timeout/broken stream). Transient.
    Network,
    /// Anything we could not classify.
    Other,
}

impl ProviderErrorKind {
    /// Lowercase wire name (`rate_limited`, `server_error`, …).
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderErrorKind::RateLimited => "rate_limited",
            ProviderErrorKind::ServerError => "server_error",
            ProviderErrorKind::AuthError => "auth_error",
            ProviderErrorKind::ModelError => "model_error",
            ProviderErrorKind::BadRequest => "bad_request",
            ProviderErrorKind::Network => "network",
            ProviderErrorKind::Other => "other",
        }
    }

    /// True for failures that may succeed on a later attempt.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ProviderErrorKind::RateLimited
                | ProviderErrorKind::ServerError
                | ProviderErrorKind::Network
        )
    }
}

impl std::fmt::Display for ProviderErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Map an HTTP status plus the response body onto a [`ProviderErrorKind`].
///
/// The status is authoritative; the body is only consulted to tell a
/// model-related 400/404 (`model_not_found`, `not_found_error` on a model id)
/// apart from a generic bad request, and to catch providers that answer 200/400
/// with a rate-limit envelope instead of a 429.
pub fn classify_http(status: u16, body: &str) -> ProviderErrorKind {
    let hint = error_hint(body);
    match status {
        401 | 403 => ProviderErrorKind::AuthError,
        429 => ProviderErrorKind::RateLimited,
        404 => {
            if mentions_model(&hint) {
                ProviderErrorKind::ModelError
            } else {
                ProviderErrorKind::BadRequest
            }
        }
        400 | 422 => {
            if mentions_auth(&hint) {
                ProviderErrorKind::AuthError
            } else if mentions_model(&hint) {
                ProviderErrorKind::ModelError
            } else if mentions_rate_limit(&hint) {
                ProviderErrorKind::RateLimited
            } else {
                ProviderErrorKind::BadRequest
            }
        }
        s if (500..600).contains(&s) => ProviderErrorKind::ServerError,
        s if (400..500).contains(&s) => {
            if mentions_rate_limit(&hint) {
                ProviderErrorKind::RateLimited
            } else {
                ProviderErrorKind::BadRequest
            }
        }
        _ => {
            if mentions_rate_limit(&hint) {
                ProviderErrorKind::RateLimited
            } else {
                ProviderErrorKind::Other
            }
        }
    }
}

/// Collapse the interesting strings of an error envelope into one lowercase
/// haystack. Handles both shapes seen in the wild:
/// - OpenAI: `{"error":{"message":..,"type":..,"code":..}}`
/// - Anthropic: `{"type":"error","error":{"type":..,"message":..}}`
///   plus the plain `{"msg":..}`/`{"message":..}` envelopes of Z.ai & friends.
fn error_hint(body: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return body.to_ascii_lowercase();
    };
    let mut parts: Vec<String> = Vec::new();
    let mut push = |value: Option<&Value>| {
        if let Some(s) = value.and_then(|x| x.as_str()) {
            parts.push(s.to_ascii_lowercase());
        }
    };
    push(v.pointer("/error/message"));
    push(v.pointer("/error/type"));
    push(v.pointer("/error/code"));
    push(v.get("message"));
    push(v.get("msg"));
    push(v.get("code"));
    if parts.is_empty() {
        return body.to_ascii_lowercase();
    }
    parts.join(" ")
}

fn mentions_model(hint: &str) -> bool {
    hint.contains("model")
}

fn mentions_auth(hint: &str) -> bool {
    hint.contains("api key")
        || hint.contains("api_key")
        || hint.contains("authentication")
        || hint.contains("unauthorized")
        || hint.contains("invalid_api")
}

fn mentions_rate_limit(hint: &str) -> bool {
    hint.contains("rate limit") || hint.contains("rate_limit") || hint.contains("too many requests")
}

/// Read the provider's retry hint from response headers (`retry-after` in
/// seconds, or `retry-after-ms`/`x-ratelimit-reset-after` in milliseconds).
pub fn retry_after_from_headers(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    if let Some(secs) = headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<f64>().ok())
    {
        return Some(secs.max(0.0).ceil() as u64);
    }
    for name in ["retry-after-ms", "x-ratelimit-reset-after"] {
        if let Some(ms) = headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<f64>().ok())
        {
            return Some((ms / 1000.0).max(0.0).ceil() as u64);
        }
    }
    None
}

impl LlmError {
    /// Vendor-neutral classification of this error.
    pub fn kind(&self) -> ProviderErrorKind {
        match self {
            LlmError::Http { status, body, .. } => classify_http(*status, body),
            LlmError::Stream(_) => ProviderErrorKind::Network,
            LlmError::Parse(_) => ProviderErrorKind::Other,
            LlmError::Provider(msg) => {
                let hint = msg.to_ascii_lowercase();
                if mentions_rate_limit(&hint) {
                    ProviderErrorKind::RateLimited
                } else if mentions_auth(&hint) {
                    ProviderErrorKind::AuthError
                } else {
                    ProviderErrorKind::Other
                }
            }
            LlmError::Request(e) => {
                if e.is_timeout() || e.is_connect() || e.is_request() {
                    ProviderErrorKind::Network
                } else {
                    ProviderErrorKind::Other
                }
            }
        }
    }

    /// The provider's own retry hint, when it supplied one.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            LlmError::Http { retry_after, .. } => retry_after.map(Duration::from_secs),
            _ => None,
        }
    }

    /// True when retrying the same request may succeed.
    pub fn is_transient(&self) -> bool {
        self.kind().is_transient()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    User,
    Assistant,
}

impl ChatRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
        }
    }
}

/// A tool call requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// A tool result returned to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

/// A provider-neutral message in the request conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default)]
    pub tool_results: Vec<ToolResult>,
    /// Multimodal parts (e.g. images); empty for text-only messages.
    #[serde(default)]
    pub content_parts: Vec<ContentPart>,
}

/// One multimodal content part attached to a [`ChatMessage`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    Image { media_type: String, data: String },
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            content_parts: Vec::new(),
        }
    }
}

/// Schema of a tool offered to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Provider-neutral reasoning/thinking effort for a request. `Off` (the
/// default) leaves the provider's default behaviour; higher levels request
/// deeper reasoning. Serialized lowercase: `off`/`low`/`medium`/`high`/`max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Thinking {
    #[default]
    Off,
    Low,
    Medium,
    High,
    Max,
}

impl Thinking {
    /// Lowercase wire name: `off`/`low`/`medium`/`high`/`max`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Thinking::Off => "off",
            Thinking::Low => "low",
            Thinking::Medium => "medium",
            Thinking::High => "high",
            Thinking::Max => "max",
        }
    }

    /// OpenAI `reasoning_effort` value (`None` = omit, keep provider default).
    pub fn openai_effort(&self) -> Option<&'static str> {
        match self {
            Thinking::Off => None,
            Thinking::Low => Some("low"),
            Thinking::Medium => Some("medium"),
            Thinking::High | Thinking::Max => Some("high"),
        }
    }

    /// Anthropic `thinking.budget_tokens` value (`None` = omit thinking block).
    pub fn anthropic_budget(&self) -> Option<u32> {
        match self {
            Thinking::Off => None,
            Thinking::Low => Some(1024),
            Thinking::Medium => Some(2048),
            Thinking::High => Some(4096),
            Thinking::Max => Some(8192),
        }
    }
}

/// The request the agent loop builds for a turn.
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDef>,
    pub max_tokens: u32,
    /// Requested reasoning/thinking effort (provider-mapped; `Off` = default).
    pub thinking: Thinking,
}

/// Token/cost usage reported by a provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// Prompt tokens served from the provider's prompt cache (cache hit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    /// Prompt tokens written to the provider's prompt cache (cache miss/write).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
}

/// A typed event emitted by a provider stream.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Text(String),
    Thinking(String),
    ToolCall(ToolCall),
    Done(Usage),
}

/// The streaming interface the agent loop consumes.
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;

    async fn stream(
        &self,
        req: ChatRequest,
    ) -> StreamResult<BoxStream<'static, StreamResult<StreamEvent>>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_serde_is_lowercase() {
        assert_eq!(serde_json::to_string(&Thinking::Off).unwrap(), "\"off\"");
        assert_eq!(
            serde_json::to_string(&Thinking::Medium).unwrap(),
            "\"medium\""
        );
        assert_eq!(
            serde_json::from_str::<Thinking>("\"max\"").unwrap(),
            Thinking::Max
        );
        assert_eq!(
            serde_json::from_str::<Thinking>("\"high\"").unwrap(),
            Thinking::High
        );
    }

    #[test]
    fn thinking_provider_mappings() {
        assert_eq!(Thinking::Off.openai_effort(), None);
        assert_eq!(Thinking::Low.openai_effort(), Some("low"));
        assert_eq!(Thinking::Medium.openai_effort(), Some("medium"));
        assert_eq!(Thinking::High.openai_effort(), Some("high"));
        assert_eq!(Thinking::Max.openai_effort(), Some("high"));

        assert_eq!(Thinking::Off.anthropic_budget(), None);
        assert_eq!(Thinking::Low.anthropic_budget(), Some(1024));
        assert_eq!(Thinking::Medium.anthropic_budget(), Some(2048));
        assert_eq!(Thinking::High.anthropic_budget(), Some(4096));
        assert_eq!(Thinking::Max.anthropic_budget(), Some(8192));
    }

    /// OpenAI-style error envelope: `{"error":{"message","type","code"}}`.
    fn openai_err(kind: &str, code: &str, msg: &str) -> String {
        serde_json::json!({
            "error": { "message": msg, "type": kind, "code": code, "param": null }
        })
        .to_string()
    }

    /// Anthropic-style error envelope: `{"type":"error","error":{"type","message"}}`.
    fn anthropic_err(kind: &str, msg: &str) -> String {
        serde_json::json!({
            "type": "error",
            "error": { "type": kind, "message": msg }
        })
        .to_string()
    }

    #[test]
    fn classifies_openai_style_errors() {
        assert_eq!(
            classify_http(
                429,
                &openai_err(
                    "rate_limit_error",
                    "rate_limit_exceeded",
                    "Rate limit reached for gpt-4o"
                )
            ),
            ProviderErrorKind::RateLimited
        );
        assert_eq!(
            classify_http(
                401,
                &openai_err(
                    "invalid_request_error",
                    "invalid_api_key",
                    "Incorrect API key provided"
                )
            ),
            ProviderErrorKind::AuthError
        );
        assert_eq!(
            classify_http(
                404,
                &openai_err(
                    "invalid_request_error",
                    "model_not_found",
                    "The model `gpt-9` does not exist"
                )
            ),
            ProviderErrorKind::ModelError
        );
        assert_eq!(
            classify_http(
                400,
                &openai_err("invalid_request_error", "model_not_found", "Unknown model")
            ),
            ProviderErrorKind::ModelError
        );
        assert_eq!(
            classify_http(
                400,
                &openai_err(
                    "invalid_request_error",
                    "context_length_exceeded",
                    "too many tokens"
                )
            ),
            ProviderErrorKind::BadRequest
        );
        assert_eq!(
            classify_http(
                500,
                &openai_err("server_error", "", "The server had an error")
            ),
            ProviderErrorKind::ServerError
        );
        assert_eq!(
            classify_http(503, "upstream connect error"),
            ProviderErrorKind::ServerError
        );
    }

    #[test]
    fn classifies_anthropic_style_errors() {
        assert_eq!(
            classify_http(
                429,
                &anthropic_err(
                    "rate_limit_error",
                    "Number of requests has exceeded your limit"
                )
            ),
            ProviderErrorKind::RateLimited
        );
        assert_eq!(
            classify_http(
                401,
                &anthropic_err("authentication_error", "invalid x-api-key")
            ),
            ProviderErrorKind::AuthError
        );
        assert_eq!(
            classify_http(403, &anthropic_err("permission_error", "not allowed")),
            ProviderErrorKind::AuthError
        );
        assert_eq!(
            classify_http(404, &anthropic_err("not_found_error", "model: claude-nope")),
            ProviderErrorKind::ModelError
        );
        assert_eq!(
            classify_http(
                400,
                &anthropic_err("invalid_request_error", "max_tokens must be positive")
            ),
            ProviderErrorKind::BadRequest
        );
        // Anthropic's "overloaded" is a 529: still a server-side transient.
        assert_eq!(
            classify_http(529, &anthropic_err("overloaded_error", "Overloaded")),
            ProviderErrorKind::ServerError
        );
    }

    #[test]
    fn classifies_zai_style_envelope_without_status() {
        // Z.ai answers 200/400 with `{"code":..,"msg":..,"success":false}`.
        let body =
            serde_json::json!({ "code": 1302, "msg": "rate limit reached", "success": false })
                .to_string();
        assert_eq!(classify_http(400, &body), ProviderErrorKind::RateLimited);
    }

    #[test]
    fn transient_kinds_are_the_retryable_ones() {
        assert!(ProviderErrorKind::RateLimited.is_transient());
        assert!(ProviderErrorKind::ServerError.is_transient());
        assert!(ProviderErrorKind::Network.is_transient());
        assert!(!ProviderErrorKind::AuthError.is_transient());
        assert!(!ProviderErrorKind::ModelError.is_transient());
        assert!(!ProviderErrorKind::BadRequest.is_transient());
        assert!(!ProviderErrorKind::Other.is_transient());
    }

    #[test]
    fn llm_error_exposes_kind_and_retry_after() {
        let err = LlmError::Http {
            status: 429,
            body: anthropic_err("rate_limit_error", "slow down"),
            retry_after: Some(7),
        };
        assert_eq!(err.kind(), ProviderErrorKind::RateLimited);
        assert!(err.is_transient());
        assert_eq!(err.retry_after(), Some(Duration::from_secs(7)));

        let err = LlmError::Http {
            status: 401,
            body: openai_err("invalid_request_error", "invalid_api_key", "bad key"),
            retry_after: None,
        };
        assert_eq!(err.kind(), ProviderErrorKind::AuthError);
        assert!(!err.is_transient());
        assert_eq!(err.retry_after(), None);

        assert_eq!(
            LlmError::Stream("connection reset".into()).kind(),
            ProviderErrorKind::Network
        );
        assert_eq!(
            LlmError::Parse("bad SSE data".into()).kind(),
            ProviderErrorKind::Other
        );
    }

    #[test]
    fn retry_after_header_forms() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut h = HeaderMap::new();
        h.insert("retry-after", HeaderValue::from_static("30"));
        assert_eq!(retry_after_from_headers(&h), Some(30));

        let mut h = HeaderMap::new();
        h.insert("retry-after-ms", HeaderValue::from_static("1500"));
        assert_eq!(retry_after_from_headers(&h), Some(2));

        let mut h = HeaderMap::new();
        h.insert("x-ratelimit-reset-after", HeaderValue::from_static("4000"));
        assert_eq!(retry_after_from_headers(&h), Some(4));

        // HTTP-date form is not parsed (no chrono dependency) -> no hint.
        let mut h = HeaderMap::new();
        h.insert(
            "retry-after",
            HeaderValue::from_static("Wed, 21 Oct 2026 07:28:00 GMT"),
        );
        assert_eq!(retry_after_from_headers(&h), None);

        assert_eq!(retry_after_from_headers(&HeaderMap::new()), None);
    }
}
