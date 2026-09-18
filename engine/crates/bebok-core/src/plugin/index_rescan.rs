use std::collections::HashSet;
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
}
