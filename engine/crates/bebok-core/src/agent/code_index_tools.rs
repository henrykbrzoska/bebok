//! `code_index_status` and `code_index_search` tools: in-process tools that
//! delegate to the `bebok-index` plugin via [`crate::plugin::PluginHost`].
//!
//! These tools live in `bebok-core` (not `bebok-tools`) to avoid a cyclic
//! dependency (`core → tools → mcp → core`). They follow the same pattern as
//! [`super::task_tool::TaskTool`]: implement [`bebok_tools::Tool`], register
//! via `instance_store.rs`.

use async_trait::async_trait;
use bebok_tools::{Tool, ToolCtx, ToolOutput};
use serde::Deserialize;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// code_index_status
// ---------------------------------------------------------------------------

/// Returns the status of the local code-index plugin.
pub struct CodeIndexStatus;

#[async_trait]
impl Tool for CodeIndexStatus {
    fn name(&self) -> &str {
        "code_index_status"
    }

    fn description(&self) -> &str {
        "Check the status of the local code index (tantivy). Returns whether \
         the index is ready, how many files and symbols it covers, or an error \
         if the plugin is absent."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, _args: Value) -> ToolOutput {
        // Cross-project safety: a disabled declaration blocks backend access.
        if crate::plugin_decl::is_disabled(&ctx.root, crate::plugin_decl::KNOWN_PLUGIN_NAME) {
            return ToolOutput::new(
                serde_json::to_string(&json!({
                    "ok": false,
                    "error": "bebok-index plugin is disabled"
                }))
                .unwrap_or_default(),
                "code_index_status",
            );
        }

        // Lazy registration: after an engine restart the plugin is only on
        // disk (declaration + slot) until something registers it.
        if !ensure_index_plugin(&ctx.root).await {
            let error_msg = plugin_not_registered_error("bebok-index", &ctx.root);
            return ToolOutput::new(
                serde_json::to_string(&json!({
                    "ok": false,
                    "error": error_msg
                }))
                .unwrap_or_default(),
                "code_index_status",
            );
        }

        let directory = ctx.root.to_string_lossy().to_string();
        let input = json!({ "directory": directory });

        tokio::select! {
            _ = ctx.abort.cancelled() => {
                return ToolOutput::new("code_index_status: aborted", "code_index_status");
            }
            result = invoke_plugin("bebok-index", "status", &input) => {
                match result {
                    Some(value) => {
                        let text = serde_json::to_string_pretty(&value)
                            .unwrap_or_else(|_| value.to_string());
                        ToolOutput::new(text, "code_index_status")
                    }
                    None => {
                        let error_msg = plugin_not_registered_error("bebok-index", &ctx.root);
                        ToolOutput::new(
                            serde_json::to_string(&json!({
                                "ok": false,
                                "error": error_msg
                            })).unwrap_or_default(),
                            "code_index_status",
                        )
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// code_index_search
// ---------------------------------------------------------------------------

/// Search the local code index.
pub struct CodeIndexSearch;

#[derive(Debug, Deserialize)]
struct SearchArgs {
    /// The search query (required, non-empty).
    query: String,
    /// Maximum number of results (default 5).
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    5
}

#[async_trait]
impl Tool for CodeIndexSearch {
    fn name(&self) -> &str {
        "code_index_search"
    }

    fn description(&self) -> &str {
        "Search the local code index (tantivy) for files matching a query. \
         Returns ranked results with file paths and scores. Prefer 1-3 \
         distinctive words; file-name fragments also work."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query (1-3 distinctive words rank best)."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results (default 5).",
                    "default": 5
                }
            },
            "required": ["query"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let parsed: SearchArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                return ToolOutput::new(
                    format!("code_index_search: invalid arguments: {e}"),
                    "code_index_search",
                );
            }
        };

        let query = parsed.query.trim().to_string();
        if query.is_empty() {
            return ToolOutput::new(
                "code_index_search: `query` must not be empty",
                "code_index_search",
            );
        }

        // Cross-project safety: a disabled declaration blocks backend access.
        if crate::plugin_decl::is_disabled(&ctx.root, crate::plugin_decl::KNOWN_PLUGIN_NAME) {
            return ToolOutput::new(
                serde_json::to_string(&json!({
                    "ok": false,
                    "error": "bebok-index plugin is disabled"
                }))
                .unwrap_or_default(),
                "code_index_search",
            );
        }

        // Lazy registration: after an engine restart the plugin is only on
        // disk (declaration + slot) until something registers it.
        if !ensure_index_plugin(&ctx.root).await {
            let error_msg = plugin_not_registered_error("bebok-index", &ctx.root);
            return ToolOutput::new(
                serde_json::to_string(&json!({
                    "ok": false,
                    "error": error_msg
                }))
                .unwrap_or_default(),
                "code_index_search",
            );
        }

        let directory = ctx.root.to_string_lossy().to_string();
        let input = json!({ "directory": directory, "query": query, "limit": parsed.limit });

        tokio::select! {
            _ = ctx.abort.cancelled() => {
                return ToolOutput::new("code_index_search: aborted", "code_index_search");
            }
            result = invoke_plugin("bebok-index", "search", &input) => {
                match result {
                    Some(value) => {
                        let text = serde_json::to_string_pretty(&value)
                            .unwrap_or_else(|_| value.to_string());
                        ToolOutput::new(text, "code_index_search")
                    }
                    None => {
                        let error_msg = plugin_not_registered_error("bebok-index", &ctx.root);
                        ToolOutput::new(
                            serde_json::to_string(&json!({
                                "ok": false,
                                "error": error_msg
                            })).unwrap_or_default(),
                            "code_index_search",
                        )
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn invoke_plugin(name: &str, action: &str, input: &Value) -> Option<Value> {
    crate::plugin::PluginHost::global()
        .invoke(name, action, input)
        .await
}

/// Lazily register the `bebok-index` plugin on the global host from its
/// on-disk declaration + slot dir (mirrors the server's
/// `ensure_plugin_registered`: the agent tools must work right after an
/// engine restart, without a prior `GET /plugins/{name}/status` call).
/// Returns `false` when the plugin must not be invoked (disabled /
/// undeclared / slot missing) — the caller then reports "not registered".
async fn ensure_index_plugin(root: &std::path::Path) -> bool {
    const NAME: &str = "bebok-index";
    // Fail closed like the rest of the stack: a broken declaration counts
    // as disabled.
    if crate::plugin_decl::is_disabled(root, NAME) {
        return false;
    }
    // No declaration at all → nothing to register from.
    let decl_path = crate::plugin_decl::decl_path(root, NAME);
    if !decl_path.is_file() {
        return false;
    }
    let host = crate::plugin::PluginHost::global();
    if host.names().await.iter().any(|n| n == NAME) {
        return true;
    }
    let slot_dir = crate::plugin_decl::install_dir(root, NAME);
    if !slot_dir.is_dir() {
        return false;
    }
    match crate::plugin_process::load_dynamic_plugin(&slot_dir) {
        Ok(dyn_plugin) => {
            tracing::info!(
                "lazy-registering plugin '{NAME}' for agent tools from {}",
                slot_dir.display()
            );
            host.register(std::sync::Arc::new(dyn_plugin)).await;
            true
        }
        Err(e) => {
            tracing::warn!(
                "cannot load plugin '{NAME}' from {}: {e}",
                slot_dir.display()
            );
            false
        }
    }
}

/// Build a smart error message when a plugin is not registered: use the
/// shared [`crate::plugin_decl::slot_state`] logic (manifest + platform
/// entrypoint + real binary presence) instead of guessing from the
/// entrypoint alone, so a crashed process with an existing binary is not
/// misreported as "binary missing".
fn plugin_not_registered_error(name: &str, root: &std::path::Path) -> String {
    let slot = crate::plugin_decl::install_dir(root, name);
    if slot.is_dir() {
        // `installed` derives from the same `is_dir()` check in the listing.
        let (_, binary) = crate::plugin_decl::slot_state(root, name, true);
        if binary == "missing" {
            // Binary missing — point the user to update.
            return format!("{name} plugin binary is missing — run Update in Settings");
        }
        format!("{name} plugin is not registered")
    } else {
        format!("{name} plugin is not registered")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn search_rejects_empty_query() {
        let args = json!({ "query": "  " });
        let parsed: SearchArgs = serde_json::from_value(args).unwrap();
        assert!(parsed.query.trim().is_empty());
    }

    #[test]
    fn search_defaults_limit_to_five() {
        let args = json!({ "query": "foo" });
        let parsed: SearchArgs = serde_json::from_value(args).unwrap();
        assert_eq!(parsed.limit, 5);
    }

    #[test]
    fn search_respects_custom_limit() {
        let args = json!({ "query": "foo", "limit": 10 });
        let parsed: SearchArgs = serde_json::from_value(args).unwrap();
        assert_eq!(parsed.limit, 10);
    }

    #[test]
    fn status_schema_has_no_required_args() {
        let tool = CodeIndexStatus;
        let schema = tool.parameters_schema();
        let required = schema.get("required").and_then(|v| v.as_array());
        assert!(
            required.is_none_or(|r| r.is_empty()),
            "status should have no required args"
        );
    }

    #[test]
    fn search_schema_requires_query() {
        let tool = CodeIndexSearch;
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("query")));
    }

    #[test]
    fn both_tools_are_read_only() {
        assert!(CodeIndexStatus.is_read_only());
        assert!(CodeIndexSearch.is_read_only());
    }

    #[tokio::test]
    async fn status_returns_ok_false_when_plugin_absent() {
        // PluginHost::global() is a OnceLock — calling it is safe.
        // When no plugin is registered, invoke returns None → ok:false.
        let ctx = ToolCtx {
            root: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = CodeIndexStatus.execute(ctx, json!({})).await;
        assert!(out.text.contains("not registered"), "got: {}", out.text);
    }

    #[tokio::test]
    async fn search_returns_ok_false_when_plugin_absent() {
        let ctx = ToolCtx {
            root: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = CodeIndexSearch
            .execute(ctx, json!({ "query": "test" }))
            .await;
        assert!(out.text.contains("not registered"), "got: {}", out.text);
    }

    #[tokio::test]
    async fn search_rejects_empty_query_at_execute() {
        let ctx = ToolCtx {
            root: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = CodeIndexSearch
            .execute(ctx, json!({ "query": "   " }))
            .await;
        assert!(out.text.contains("must not be empty"), "got: {}", out.text);
    }

    #[tokio::test]
    async fn search_rejects_missing_query() {
        let ctx = ToolCtx {
            root: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = CodeIndexSearch.execute(ctx, json!({})).await;
        assert!(out.text.contains("invalid arguments"), "got: {}", out.text);
    }

    #[tokio::test]
    async fn status_returns_disabled_when_declaration_disabled() {
        let root =
            std::env::temp_dir().join(format!("bebok-cit-disabled-{}", uuid::Uuid::new_v4()));
        let plugins_dir = root.join(".bebok").join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        let decl = crate::plugin_decl::PluginDecl {
            name: crate::plugin_decl::KNOWN_PLUGIN_NAME.to_string(),
            repo: "test/repo".to_string(),
            url: "https://example.com/test/repo".to_string(),
            enabled: false,
            asset_url: None,
            asset_sha256: None,
        };
        std::fs::write(
            plugins_dir.join(format!("{}.json", crate::plugin_decl::KNOWN_PLUGIN_NAME)),
            serde_json::to_string_pretty(&decl).unwrap(),
        )
        .unwrap();

        let ctx = ToolCtx {
            root: root.clone(),
            session_id: "test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = CodeIndexStatus.execute(ctx, json!({})).await;
        assert!(out.text.contains("disabled"), "got: {}", out.text);

        // Cleanup.
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn search_returns_disabled_when_declaration_disabled() {
        let root = std::env::temp_dir().join(format!(
            "bebok-cit-search-disabled-{}",
            uuid::Uuid::new_v4()
        ));
        let plugins_dir = root.join(".bebok").join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        let decl = crate::plugin_decl::PluginDecl {
            name: crate::plugin_decl::KNOWN_PLUGIN_NAME.to_string(),
            repo: "test/repo".to_string(),
            url: "https://example.com/test/repo".to_string(),
            enabled: false,
            asset_url: None,
            asset_sha256: None,
        };
        std::fs::write(
            plugins_dir.join(format!("{}.json", crate::plugin_decl::KNOWN_PLUGIN_NAME)),
            serde_json::to_string_pretty(&decl).unwrap(),
        )
        .unwrap();

        let ctx = ToolCtx {
            root: root.clone(),
            session_id: "test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = CodeIndexSearch
            .execute(ctx, json!({ "query": "test" }))
            .await;
        assert!(out.text.contains("disabled"), "got: {}", out.text);

        // Cleanup.
        let _ = std::fs::remove_dir_all(&root);
    }
}
