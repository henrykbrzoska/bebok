//! Built-in tools for the Bebok engine.
//!
//! The [`Tool`] trait is the single extension point: built-in tools and (later)
//! MCP tools both implement it, and the agent loop / permission engine never
//! know the difference.

pub mod append_file;
pub mod base64;
pub mod basename;
pub mod bash;
pub mod browser;
pub mod chmod;
pub mod cp;
pub mod diff;
pub mod dirname;
pub mod docker;
pub mod du;
pub mod edit_file;
pub mod explorer;
pub mod fetch;
pub mod find;
pub mod glob_tool;
pub mod grep;
pub mod gzip;
pub mod head;
pub mod list_dir;
pub mod ln;
pub mod mkdir;
pub mod mv;
mod pathguard;
pub mod pwd;
pub mod read_file;
pub mod realpath;
pub mod registry;
pub mod rm;
pub mod runtimes;
pub mod sed;
pub mod sha256sum;
pub mod sort;
pub mod stat;
pub mod tail;
pub mod tool;
pub mod touch;
pub mod tree;
pub mod uniq;
pub mod wc;
pub mod which;
pub mod write_file;

pub use docker::{DockerStatus, check_docker};
pub use explorer::FsEntry;
pub use pathguard::resolve_in_root;
pub use registry::ToolRegistry;
pub use runtimes::Runtimes;
pub use tool::{Tool, ToolCtx, ToolImage, ToolOutput};

use std::sync::Arc;

/// All built-in tools available to the agent.
///
/// Besides the read/write/search primitives, this exposes native Rust
/// equivalents of the most important shell commands so the model can use them
/// portably instead of shelling out through `bash` (which differs between
/// `sh` and `cmd`):
///
/// * read-only — `pwd`, `head`, `tail`, `wc`, `stat`, `du`, `sort`, `uniq`,
///   `diff`, `which`, `find`, `realpath`, `basename`, `dirname`, `sha256sum`,
///   `base64` (read-only unless `out` is given);
/// * mutating — `mkdir`, `touch`, `cp`, `mv`, `rm`, `append_file`, `chmod`,
///   `ln`, `sed`, `gzip`;
/// * browser automation — the `browser_*` family (WP-BROWSER), one headless
///   Chromium page per session, every call `Ask` by default.
pub fn builtin_tools() -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        // Read / inspect.
        Arc::new(read_file::ReadFile),
        Arc::new(head::Head),
        Arc::new(tail::Tail),
        Arc::new(wc::Wc),
        Arc::new(list_dir::ListDir),
        Arc::new(tree::Tree),
        Arc::new(pwd::Pwd),
        Arc::new(stat::Stat),
        Arc::new(du::Du),
        Arc::new(glob_tool::Glob),
        Arc::new(grep::Grep),
        Arc::new(find::Find),
        Arc::new(sort::Sort),
        Arc::new(uniq::Uniq),
        Arc::new(diff::Diff),
        Arc::new(which::Which),
        Arc::new(realpath::Realpath),
        Arc::new(basename::Basename),
        Arc::new(dirname::Dirname),
        Arc::new(sha256sum::Sha256Sum),
        Arc::new(base64::Base64),
        Arc::new(fetch::Fetch),
        // Write / mutate.
        Arc::new(write_file::WriteFile),
        Arc::new(append_file::AppendFile),
        Arc::new(edit_file::EditFile),
        Arc::new(sed::Sed),
        Arc::new(mkdir::Mkdir),
        Arc::new(touch::Touch),
        Arc::new(cp::Cp),
        Arc::new(mv::Mv),
        Arc::new(rm::Rm),
        Arc::new(chmod::Chmod),
        Arc::new(ln::Ln),
        Arc::new(gzip::Gzip),
        // Escape hatch.
        Arc::new(bash::Bash),
    ];
    tools.extend(browser::tools());
    tools
}
