//! Built-in tools for the Bebok engine.
//!
//! The [`Tool`] trait is the single extension point: built-in tools and (later)
//! MCP tools both implement it, and the agent loop / permission engine never
//! know the difference.

pub mod bash;
pub mod docker;
pub mod explorer;
pub mod glob_tool;
pub mod grep;
pub mod list_dir;
pub mod read_file;
pub mod registry;
pub mod runtimes;
pub mod tool;
pub mod tree;
pub mod write_file;

pub use docker::{DockerStatus, check_docker};
pub use explorer::FsEntry;
pub use registry::ToolRegistry;
pub use runtimes::Runtimes;
pub use tool::{Tool, ToolCtx, ToolOutput};

use std::sync::Arc;

/// All built-in tools available in Milestone 1.
pub fn builtin_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(read_file::ReadFile),
        Arc::new(write_file::WriteFile),
        Arc::new(bash::Bash),
        Arc::new(glob_tool::Glob),
        Arc::new(grep::Grep),
        Arc::new(list_dir::ListDir),
        Arc::new(tree::Tree),
    ]
}
