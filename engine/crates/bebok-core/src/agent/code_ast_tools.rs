//! `code_ast` tool: AST-aware structural search that delegates to the
//! `bebok-ast` plugin via [`crate::plugin::PluginHost`].
//!
//! This tool lives in `bebok-core` (not `bebok-tools`) to avoid a cyclic
//! dependency (`core → tools → mcp → core`). It follows the same pattern as
//! [`super::code_index_tools`]: implement [`bebok_tools::Tool`], register
//! via `instance_store.rs`.

use async_trait::async_trait;
use bebok_tools::{Tool, ToolCtx, ToolOutput};
use serde::Deserialize;
use serde_json::{Value, json};

/// AST-aware structural code search tool.
pub struct CodeAstSearch;

#[derive(Debug, Deserialize)]
struct AstArgs {
    /// AST node kind to search for (required).
    kind: String,
    /// Optional filters.
    #[serde(default)]
    filters: Value,
    /// Max results to return (default 20).
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    20
}

#[async_trait]
impl Tool for CodeAstSearch {
    fn name(&self) -> &str {
        "code_ast"
    }

    fn description(&self) -> &str {
        "AST-aware structural search: find impl blocks, structs with specific \
         derives, functions returning a given type, annotated items, and test \
         functions. Parameters: kind (impl/struct/fn/enum/trait/test/…), \
         optional filters (trait, derive, return_type, annotation, name_regex, \
         ext, path_regex), optional limit."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["impl", "struct", "fn", "enum", "trait", "type_alias", "const", "static", "mod", "use", "test"],
                    "description": "AST node kind to search for."
                },
                "filters": {
                    "type": "object",
                    "properties": {
                        "trait":       { "type": "string", "description": "For impl: trait name (e.g. 'Display', 'From<String>')" },
                        "derive":      { "type": "string", "description": "For struct: derive attribute contains this (e.g. 'Serialize')" },
                        "return_type": { "type": "string", "description": "For fn/type_alias: return type contains this substring" },
                        "annotation":  { "type": "string", "description": "Attribute contains this (e.g. '#[test]', '#[derive(...)')" },
                        "name_regex":  { "type": "string", "description": "Item name matches this regex" },
                        "ext":         { "type": "string", "description": "File extension filter without dot (e.g. 'rs')" },
                        "path_regex":  { "type": "string", "description": "File path must match this regex" }
                    }
                },
                "limit": { "type": "integer", "default": 20, "description": "Max results to return" }
            },
            "required": ["kind"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let parsed: AstArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                return ToolOutput::new(
                    format!("code_ast: invalid arguments: {e}"),
                    "code_ast",
                );
            }
        };

        let kind = parsed.kind.trim().to_string();
        if kind.is_empty() {
            return ToolOutput::new(
                "code_ast: `kind` must not be empty",
                "code_ast",
            );
        }

        // Check that ast_search is enabled in config.
        let config = crate::config::load(&ctx.root);
        if !config.ast_search.enabled {
            return ToolOutput::new(
                serde_json::to_string(&json!({
                    "ok": false,
                    "error": "ast_search is disabled in project config (set ast_search.enabled = true in .bebok/config.json)"
                }))
                .unwrap_or_default(),
                "code_ast",
            );
        }

        // Cross-project safety: a disabled declaration blocks backend access.
        if crate::plugin_decl::is_disabled(&ctx.root, "bebok-ast") {
            return ToolOutput::new(
                serde_json::to_string(&json!({
                    "ok": false,
                    "error": "bebok-ast plugin is disabled"
                }))
                .unwrap_or_default(),
                "code_ast",
            );
        }

        // Lazy registration: after an engine restart the plugin is only on
        // disk (declaration + slot) until something registers it.
        if !ensure_ast_plugin(&ctx.root).await {
            let error_msg = plugin_not_registered_error("bebok-ast", &ctx.root);
            return ToolOutput::new(
                serde_json::to_string(&json!({
                    "ok": false,
                    "error": error_msg
                }))
                .unwrap_or_default(),
                "code_ast",
            );
        }

        let directory = ctx.root.to_string_lossy().to_string();
        let input = json!({
            "root": directory,
            "kind": kind,
            "filters": parsed.filters,
            "limit": parsed.limit,
        });

        tokio::select! {
            _ = ctx.abort.cancelled() => {
                return ToolOutput::new("code_ast: aborted", "code_ast");
            }
            result = invoke_plugin("bebok-ast", "query", &input) => {
                match result {
                    Some(value) => {
                        let text = serde_json::to_string_pretty(&value)
                            .unwrap_or_else(|_| value.to_string());
                        ToolOutput::new(text, "code_ast")
                    }
                    None => {
                        let error_msg = plugin_not_registered_error("bebok-ast", &ctx.root);
                        ToolOutput::new(
                            serde_json::to_string(&json!({
                                "ok": false,
                                "error": error_msg
                            })).unwrap_or_default(),
                            "code_ast",
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

/// Lazily register the `bebok-ast` plugin on the global host from its
/// on-disk declaration + slot dir.
async fn ensure_ast_plugin(root: &std::path::Path) -> bool {
    const NAME: &str = "bebok-ast";
    if crate::plugin_decl::is_disabled(root, NAME) {
        return false;
    }
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

fn plugin_not_registered_error(name: &str, root: &std::path::Path) -> String {
    let slot = crate::plugin_decl::install_dir(root, name);
    if slot.is_dir() {
        let (_, binary) = crate::plugin_decl::slot_state(root, name, true);
        if binary == "missing" {
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
    fn schema_requires_kind() {
        let tool = CodeAstSearch;
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("kind")));
    }

    #[test]
    fn tool_is_read_only() {
        assert!(CodeAstSearch.is_read_only());
    }

    #[test]
    fn schema_has_all_enums() {
        let tool = CodeAstSearch;
        let schema = tool.parameters_schema();
        let enums = schema["properties"]["kind"]["enum"]
            .as_array()
            .unwrap();
        assert!(enums.contains(&json!("impl")));
        assert!(enums.contains(&json!("struct")));
        assert!(enums.contains(&json!("fn")));
        assert!(enums.contains(&json!("enum")));
        assert!(enums.contains(&json!("trait")));
        assert!(enums.contains(&json!("test")));
    }

    #[tokio::test]
    async fn rejects_missing_kind() {
        let ctx = ToolCtx {
            root: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = CodeAstSearch.execute(ctx, json!({})).await;
        assert!(out.text.contains("invalid arguments"), "got: {}", out.text);
    }

    #[tokio::test]
    async fn returns_disabled_when_ast_search_off() {
        let root =
            std::env::temp_dir().join(format!("bebok-ast-disabled-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".bebok")).unwrap();
        std::fs::write(
            root.join(".bebok").join("config.json"),
            r#"{ "ast_search": { "enabled": false } }"#,
        )
        .unwrap();

        let ctx = ToolCtx {
            root: root.clone(),
            session_id: "test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = CodeAstSearch.execute(ctx, json!({ "kind": "fn" })).await;
        assert!(out.text.contains("disabled"), "got: {}", out.text);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn returns_error_when_plugin_absent() {
        let root =
            std::env::temp_dir().join(format!("bebok-ast-noplugin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".bebok")).unwrap();
        std::fs::write(
            root.join(".bebok").join("config.json"),
            r#"{ "ast_search": { "enabled": true } }"#,
        )
        .unwrap();

        let ctx = ToolCtx {
            root: root.clone(),
            session_id: "test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = CodeAstSearch.execute(ctx, json!({ "kind": "fn" })).await;
        assert!(
            out.text.contains("not registered") || out.text.contains("disabled"),
            "got: {}",
            out.text
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn rejects_empty_kind() {
        let root =
            std::env::temp_dir().join(format!("bebok-ast-empty-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".bebok")).unwrap();
        std::fs::write(
            root.join(".bebok").join("config.json"),
            r#"{ "ast_search": { "enabled": true } }"#,
        )
        .unwrap();

        let ctx = ToolCtx {
            root: root.clone(),
            session_id: "test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = CodeAstSearch
            .execute(ctx, json!({ "kind": "  " }))
            .await;
        assert!(out.text.contains("must not be empty"), "got: {}", out.text);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn returns_disabled_when_declaration_disabled() {
        let root = std::env::temp_dir().join(format!(
            "bebok-ast-decl-disabled-{}",
            uuid::Uuid::new_v4()
        ));
        let plugins_dir = root.join(".bebok").join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        std::fs::create_dir_all(root.join(".bebok")).unwrap();
        std::fs::write(
            root.join(".bebok").join("config.json"),
            r#"{ "ast_search": { "enabled": true } }"#,
        )
        .unwrap();

        // Write a disabled declaration for bebok-ast.
        let decl = crate::plugin_decl::PluginDecl {
            name: "bebok-ast".to_string(),
            repo: "test/repo".to_string(),
            url: "https://example.com/test/repo".to_string(),
            enabled: false,
            asset_url: None,
            asset_sha256: None,
        };
        std::fs::write(
            plugins_dir.join("bebok-ast.json"),
            serde_json::to_string_pretty(&decl).unwrap(),
        )
        .unwrap();

        let ctx = ToolCtx {
            root: root.clone(),
            session_id: "test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = CodeAstSearch.execute(ctx, json!({ "kind": "fn" })).await;
        assert!(out.text.contains("disabled"), "got: {}", out.text);

        let _ = std::fs::remove_dir_all(&root);
    }
}
