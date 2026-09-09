use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Text (possibly large) produced by a tool, plus an optional structured
/// payload (e.g. diffs for the UI).
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub text: String,
    pub title: String,
    pub structured: Option<Value>,
}

impl ToolOutput {
    pub fn new(text: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            title: title.into(),
            structured: None,
        }
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
