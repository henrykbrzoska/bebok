use bebok_llm::LlmError;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("session busy")]
    SessionBusy,
    #[error("provider error: {0}")]
    Llm(#[from] LlmError),
    #[error("tool not found: {0}")]
    ToolNotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("invalid provider configuration: {0}")]
    ProviderConfig(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
