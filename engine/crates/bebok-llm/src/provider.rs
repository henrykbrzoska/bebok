use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type StreamResult<T> = Result<T, LlmError>;

/// Errors surfaced by a provider.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("http error {status}: {body}")]
    Http { status: u16, body: String },
    #[error("stream error: {0}")]
    Stream(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("request error: {0}")]
    Request(#[from] reqwest::Error),
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
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
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
#[derive(Debug, Clone)]
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
        assert_eq!(serde_json::to_string(&Thinking::Medium).unwrap(), "\"medium\"");
        assert_eq!(serde_json::from_str::<Thinking>("\"max\"").unwrap(), Thinking::Max);
        assert_eq!(serde_json::from_str::<Thinking>("\"high\"").unwrap(), Thinking::High);
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
}
