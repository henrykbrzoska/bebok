use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use serde_json;
use tokio::sync::mpsc;

use crate::plugin::{BebokPlugin, Hook, HookResult, PluginHost};

/// Plugin that debounces file-write events and requests a rebuild of the
/// search index for the affected project directory.
///
/// Multiple writes within a short window are coalesced into a single
/// rebuild request per directory (3-second debounce from the first event).
pub struct IndexRescanPlugin {
    tx: tokio::sync::mpsc::UnboundedSender<String>,
}

impl Default for IndexRescanPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexRescanPlugin {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(Self::debounce_loop(rx));
        Self { tx }
    }
    async fn debounce_loop(mut rx: mpsc::UnboundedReceiver<String>) {
        let mut pending: HashSet<String> = HashSet::new();
        loop {
            // Wait for the first write of the next batch.
            let first = match rx.recv().await {
                Some(dir) => dir,
                None => return,
            };
            pending.insert(first);

            let deadline = tokio::time::sleep(Duration::from_secs(3));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    recv = rx.recv() => {
                        match recv {
                            Some(dir) => {
                                pending.insert(dir);
                            }
                            None => break,
                        }
                    }
                    _ = &mut deadline => break,
                }
            }
            if rx.is_closed() && pending.is_empty() {
                return;
            }

            for dir in pending.drain() {
                let dir = dir.clone();
                tokio::spawn(async move {
                    ensure_index_plugin_registered(&dir).await;
                    let result = PluginHost::global()
                        .invoke(
                            "bebok-index",
                            "rebuild",
                            &serde_json::json!({ "directory": dir }),
                        )
                        .await;
                    if result.is_none() {
                        tracing::warn!(
                            "index-rescan: bebok-index plugin or action missing for directory {dir:?}"
                        );
                    }
                });
            }
        }
    }
}

/// Lazily register the `bebok-index` plugin on the global host from its
/// on-disk declaration + slot dir, mirroring the server's
/// `ensure_plugin_registered` (routes/plugins.rs) and the agent tools'
/// `ensure_index_plugin` (agent/code_index_tools.rs).
///
/// The auto-rescan path (`after.file_write` hook → debounce → `invoke`)
/// never passes through an HTTP route, so without this the `invoke` call
/// below silently returns `None` after every engine restart until the user
/// happens to open `GET /plugins/{name}/status` for the project — the
/// reindex "never fires despite the plugin being enabled".
async fn ensure_index_plugin_registered(dir: &str) {
    const NAME: &str = "bebok-index";
    let root = Path::new(dir);
    // Fail closed like the rest of the stack: a broken declaration counts
    // as disabled.
    if crate::plugin_decl::is_disabled(root, NAME) {
        return;
    }
    // No declaration at all → nothing to register from.
    if !crate::plugin_decl::decl_path(root, NAME).is_file() {
        return;
    }
    let host = PluginHost::global();
    if host.names().await.iter().any(|n| n == NAME) {
        return;
    }
    let slot_dir = crate::plugin_decl::install_dir(root, NAME);
    if !slot_dir.is_dir() {
        return;
    }
    match crate::plugin_process::load_dynamic_plugin(&slot_dir) {
        Ok(dyn_plugin) => {
            tracing::info!(
                "index-rescan: lazy-registering plugin '{NAME}' from {}",
                slot_dir.display()
            );
            host.register(std::sync::Arc::new(dyn_plugin)).await;
        }
        Err(e) => {
            tracing::warn!(
                "index-rescan: cannot load plugin '{NAME}' from {}: {e}",
                slot_dir.display()
            );
        }
    }
}

#[async_trait::async_trait]
impl BebokPlugin for IndexRescanPlugin {
    fn name(&self) -> &str {
        "index-rescan"
    }

    async fn on_hook(&self, hook: Hook, payload: &mut serde_json::Value) -> HookResult {
        if hook == Hook::AFTER_FILE_WRITE
            && let Some(dir) = payload.get("directory").and_then(|v| v.as_str())
        {
            let _ = self.tx.send(dir.to_string());
        }
        HookResult::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn plugin_name_is_index_rescan() {
        let plugin = IndexRescanPlugin::new();
        assert_eq!(plugin.name(), "index-rescan");
    }

    #[tokio::test]
    async fn on_hook_after_file_write_with_directory_continues() {
        let plugin = IndexRescanPlugin::new();
        let mut payload = serde_json::json!({ "directory": "/some/project" });
        let result = plugin.on_hook(Hook::AFTER_FILE_WRITE, &mut payload).await;
        assert!(matches!(result, HookResult::Continue));
    }

    #[tokio::test]
    async fn on_hook_other_hook_ignores_payload() {
        let plugin = IndexRescanPlugin::new();
        let mut payload = serde_json::json!({ "directory": "/some/project" });
        let result = plugin.on_hook(Hook::TURN_END, &mut payload).await;
        assert!(matches!(result, HookResult::Continue));
    }

    /// Regression: the auto-rescan path must lazily register `bebok-index`
    /// from disk (declaration + slot dir) instead of silently skipping the
    /// rebuild when the host has no such plugin yet (e.g. right after an
    /// engine restart, before any `GET /plugins/{name}/status` call).
    /// A missing declaration must stay a no-op (nothing to register from).
    #[tokio::test]
    async fn ensure_registers_nothing_without_declaration() {
        let dir =
            std::env::temp_dir().join(format!("bebok-index-rescan-noreg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        PluginHost::global().unregister("bebok-index").await;
        ensure_index_plugin_registered(&dir.to_string_lossy()).await;
        assert!(
            !PluginHost::global()
                .names()
                .await
                .iter()
                .any(|n| n == "bebok-index"),
            "no declaration on disk → must not register anything"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
