//! Validation for text-file attachments sent with a prompt.
//!
//! File attachments are intentionally limited to UTF-8 text. Their decoded
//! bytes travel as base64 so the same HTTP API works for text and images; the
//! model receives the decoded text inline in the user message.

use base64::Engine;
use serde::Deserialize;

use crate::error::{CoreError, Result};

/// Maximum number of non-image files attached to one prompt.
pub const MAX_FILES_PER_PROMPT: usize = 10;
/// Maximum decoded size of one attached text file (20 MiB).
pub const MAX_FILE_BYTES: usize = 20 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub struct AgentFileInput {
    pub media_type: String,
    pub data: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// Validate an attachment and turn it into a persisted file part.
pub fn validate_file(input: &AgentFileInput) -> Result<crate::session::Part> {
    if !input.media_type.starts_with("text/")
        && !matches!(
            input.media_type.as_str(),
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/x-yaml"
                | "application/yaml"
                | "application/x-sh"
        )
    {
        return Err(CoreError::Other(format!(
            "unsupported file type '{}': attach a UTF-8 text file",
            input.media_type
        )));
    }
    if input.data.len() > MAX_FILE_BYTES.saturating_mul(2) {
        return Err(CoreError::Other(
            "file attachment is too large (max 20 MB)".into(),
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&input.data)
        .map_err(|_| CoreError::Other("invalid base64 in file attachment".into()))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(CoreError::Other(
            "file attachment is too large (max 20 MB)".into(),
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| CoreError::Other("attached file is not valid UTF-8 text".into()))?;
    Ok(crate::session::Part::File {
        media_type: input.media_type.clone(),
        name: input.name.clone().unwrap_or_else(|| "attachment".into()),
        text,
    })
}

pub fn validate_files(inputs: &[AgentFileInput]) -> Result<Vec<crate::session::Part>> {
    if inputs.len() > MAX_FILES_PER_PROMPT {
        return Err(CoreError::Other(format!(
            "too many files: maximum {MAX_FILES_PER_PROMPT} per prompt"
        )));
    }
    inputs.iter().map(validate_file).collect()
}
