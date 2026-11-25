//! Backend registry.
//!
//! Adding a CLI is: write `backends/<name>.rs` implementing
//! [`crate::backend::CliBackend`], then add one line to [`all_backends`]. If a
//! new backend needs a change anywhere outside its own file, the trait is
//! wrong — that is the acceptance test for this abstraction.

use std::sync::Arc;

use crate::backend::CliBackend;

pub mod claude;
pub mod grok;

/// Every registered backend, in a stable order (the settings card renders them
/// in this order, so keep it alphabetical by id).
pub fn all_backends() -> Vec<Arc<dyn CliBackend>> {
    vec![Arc::new(claude::ClaudeBackend), Arc::new(grok::GrokBackend)]
}

/// Look up a backend by its `extra.backend` id.
///
/// Matching is case-insensitive: the id lands in a hand-edited JSONC file and
/// `"Claude"` is not a different backend.
pub fn backend_for(id: &str) -> Option<Arc<dyn CliBackend>> {
    let wanted = id.trim();
    all_backends()
        .into_iter()
        .find(|b| b.id().eq_ignore_ascii_case(wanted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_is_registered() {
        let ids: Vec<&str> = all_backends().iter().map(|b| b.id()).collect();
        assert!(ids.contains(&"claude"), "{ids:?}");
    }

    #[test]
    fn backend_lookup_is_trimmed_and_case_insensitive() {
        assert!(backend_for("claude").is_some());
        assert!(backend_for("  Claude ").is_some());
        assert!(backend_for("nope").is_none());
        assert!(backend_for("").is_none());
    }

    #[test]
    fn backend_ids_are_unique() {
        let all = all_backends();
        let mut ids: Vec<&str> = all.iter().map(|b| b.id()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "duplicate backend id");
    }
}
