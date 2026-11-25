//! `?directory=` query shared by directory-keyed endpoints.
//!
//! `PUT /config` additionally accepts `?scope=global|project` (which layer to
//! write) and `?replace=true` (replace the whole file instead of merging).

use serde::Deserialize;

/// `?directory=` query for endpoints that require it (agents/mcp/config).
/// `PUT /config` additionally accepts `?scope=global|project` (which layer to
/// write) and `?replace=true` (replace the whole file instead of merging).
#[derive(Deserialize)]
pub struct DirectoryQuery {
    pub directory: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub replace: Option<bool>,
}

/// Remove a test temp dir, retrying briefly on Windows: a just-aborted server
/// task may still hold file handles for a moment (OS error 32, "being used by
/// another process"), which makes a single `remove_dir_all(...).unwrap()`
/// flaky on loaded CI runners. If the dir is still locked after the retries
/// (e.g. the agent hot-reload `notify` watcher holds the project dir for the
/// rest of the test process), give up with a warning instead of failing the
/// test — the OS reclaims temp dirs, and cleanup must never fail a test.
#[cfg(test)]
pub fn remove_test_dir(base: &std::path::Path) {
    for _ in 0..50 {
        if std::fs::remove_dir_all(base).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // Best effort: never fail a test on leftover temp files.
    if base.exists() {
        eprintln!("remove_test_dir({base:?}): still locked, leaving for the OS to reclaim");
        let _ = std::fs::remove_dir_all(base);
    }
}
