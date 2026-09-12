use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// An image produced by a tool (e.g. a `browser_screenshot`), delivered to
/// the model as an image part next to the textual tool result.
///
/// Same shape as the session's `Part::Image`: a media type plus the raw
/// base64 payload (no `data:` prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolImage {
    pub media_type: String,
    pub data: String,
}

/// Text (possibly large) produced by a tool, plus an optional structured
/// payload (e.g. diffs for the UI) and an optional image the model should
/// see alongside the text (WP-BROWSER).
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub text: String,
    pub title: String,
    pub structured: Option<Value>,
    pub image: Option<ToolImage>,
}

impl ToolOutput {
    pub fn new(text: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            title: title.into(),
            structured: None,
            image: None,
        }
    }

    /// Attach a structured payload (builder style).
    pub fn with_structured(mut self, structured: Value) -> Self {
        self.structured = Some(structured);
        self
    }

    /// Attach an image the model should see (builder style).
    pub fn with_image(mut self, media_type: impl Into<String>, data: impl Into<String>) -> Self {
        self.image = Some(ToolImage {
            media_type: media_type.into(),
            data: data.into(),
        });
        self
    }
}

/// Execution context passed to every tool invocation.
///
/// Carries the instance root, the session id, and the cancellation token bound
/// to the current turn. Permission decisions are made by the agent loop's
/// permission gate *before* [`Tool::execute`] is called; tools themselves never
/// ask for permission.
#[derive(Clone)]
pub struct ToolCtx {
    /// Instance root (working directory the session is bound to).
    pub root: PathBuf,
    /// Session id the tool is running on behalf of.
    pub session_id: String,
    /// Cancellation token tied to the current turn.
    pub abort: CancellationToken,
}

/// The single extension point for all tools (built-in and MCP).
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema describing the tool's parameters.
    fn parameters_schema(&self) -> Value;
    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput;

    /// Whether the tool only reads state and never mutates it.
    ///
    /// The permission engine uses this for its default: read-only tools are
    /// `Allow`, mutating tools (and tools we cannot classify) are `Ask`.
    fn is_read_only(&self) -> bool {
        false
    }

    /// Per-call refinement of [`Tool::is_read_only`], for tools whose
    /// mutation-ness depends on their arguments (`fetch` is read-only for GET
    /// but not for POST). Defaults to the tool-wide answer, so most tools only
    /// implement [`Tool::is_read_only`].
    fn is_read_only_for(&self, _args: &Value) -> bool {
        self.is_read_only()
    }
}

/// Convenience constructor for [`ToolCtx`].
pub fn tool_ctx(root: PathBuf, session_id: String, abort: CancellationToken) -> ToolCtx {
    ToolCtx {
        root,
        session_id,
        abort,
    }
}

/// Helper used by the agent loop to convert a [`Tool`] into its LLM schema.
pub fn tool_to_schema(tool: &Arc<dyn Tool>) -> Value {
    serde_json::json!({
        "name": tool.name(),
        "description": tool.description(),
        "input_schema": tool.parameters_schema(),
    })
}
