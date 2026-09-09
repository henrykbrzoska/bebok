//! `McpTool`: wraps one MCP server tool behind the `bebok_tools::Tool` trait
//! so it joins the same registry and permission gate as built-in tools.

use std::sync::Arc;

use async_trait::async_trait;
use rmcp::model::{CallToolRequestParams, ContentBlock};
use rmcp::{Peer, RoleClient};
use serde_json::Value;

use bebok_tools::{Tool, ToolCtx, ToolOutput};

/// A tool exposed by an MCP server, named `mcp__<server>__<tool>`.
pub struct McpTool {
    name: String,
    raw_name: String,
    description: String,
    input_schema: Value,
    read_only: bool,
    peer: Peer<RoleClient>,
}

impl McpTool {
    pub fn new(
        server: &str,
        raw_name: String,
        description: String,
        input_schema: Value,
        read_only: bool,
        peer: Peer<RoleClient>,
    ) -> Arc<dyn Tool> {
        let name = format!("mcp__{server}__{raw_name}");
        Arc::new(Self {
            name,
            raw_name,
            description,
            input_schema,
            read_only,
            peer,
        })
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.input_schema.clone()
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let arguments = args.as_object().cloned().unwrap_or_default();
        let params = CallToolRequestParams::new(self.raw_name.clone()).with_arguments(arguments);

        let result = tokio::select! {
            _ = ctx.abort.cancelled() => {
                return ToolOutput::new("aborted", self.name.clone());
            }
            res = self.peer.call_tool(params) => res,
        };

        match result {
            Ok(res) => {
                let text = content_text(&res.content);
                let is_error = res.is_error == Some(true);
                if is_error {
                    ToolOutput::new(text, self.name.clone())
                } else {
                    ToolOutput {
                        text,
                        title: self.name.clone(),
                        structured: res.structured_content,
                    }
                }
            }
            Err(e) => ToolOutput::new(format!("error: {e}"), self.name.clone()),
        }
    }
}

/// Flatten an MCP `CallToolResult` content list into plain text.
fn content_text(content: &[ContentBlock]) -> String {
    let mut out = String::new();
    for block in content {
        let piece = match block {
            ContentBlock::Text(t) => t.text.clone(),
            ContentBlock::Resource(r) => r.get_text(),
            ContentBlock::ResourceLink(r) => format!("[resource: {}]", r.name),
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        if !out.is_empty() && !piece.is_empty() {
            out.push('\n');
        }
        out.push_str(&piece);
    }
    out
}
