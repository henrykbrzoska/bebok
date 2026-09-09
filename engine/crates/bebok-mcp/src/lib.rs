//! MCP bridge crate (SPEC §3.7, milestone M4).
//!
//! The [`McpManager`] connects enabled MCP servers over stdio or streamable
//! HTTP, lists their tools, and wraps each behind the `bebok_tools::Tool`
//! trait so the rest of the system does not know MCP specifics. Toggling a
//! server off drops the connection and removes its tools.

pub mod config;
pub mod manager;
pub mod tool;

pub use config::{McpServerSpec, McpTransport};
pub use manager::{McpManager, McpStatus};
pub use tool::McpTool;
