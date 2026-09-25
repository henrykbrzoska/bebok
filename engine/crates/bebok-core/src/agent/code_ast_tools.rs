//! `code_ast` tool: AST-aware structural search using the embedded
//! [`super::ast_search`] module (tree-sitter, no external binary).

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
    /// Optional filters (nested object).
    #[serde(default)]
    filters: Value,
    /// Max results to return (default 20).
    #[serde(default = "default_limit")]
    limit: usize,
    /// Flat filter fallback: also accepted at the top level for callers
    /// that do not nest them under `filters`. `filters` wins on conflict.
    #[serde(default)]
    r#trait: Option<String>,
    #[serde(default)]
    derive: Option<String>,
    #[serde(default)]
    return_type: Option<String>,
    #[serde(default)]
    annotation: Option<String>,
    #[serde(default)]
    name_regex: Option<String>,
    #[serde(default)]
    path_regex: Option<String>,
    #[serde(default)]
    ext: Option<String>,
}

impl AstArgs {
    /// Merge flat top-level filter keys into `filters` (nested wins).
    fn merged_filters(&self) -> Value {
        let mut obj = self.filters.as_object().cloned().unwrap_or_default();
        for (key, val) in [
            ("trait", &self.r#trait),
            ("derive", &self.derive),
            ("return_type", &self.return_type),
            ("annotation", &self.annotation),
            ("name_regex", &self.name_regex),
            ("path_regex", &self.path_regex),
            ("ext", &self.ext),
        ] {
            if let Some(v) = val {
                obj.entry(key.to_string())
                    .or_insert(Value::String(v.clone()));
            }
        }
        Value::Object(obj)
    }
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
        "AST-aware structural search over the session directory (ctx.root — \
         not necessarily the project root; the response echoes it as `root`): \
         find impl blocks, structs with specific derives, functions returning \
         a given type, annotated items, and test functions. Parameters: kind \
         (impl/struct/fn/enum/trait/test/…), optional filters object (trait, \
         derive, return_type, annotation, name_regex, ext, path_regex — also \
         accepted flat at the top level), optional limit."
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
                return ToolOutput::new(format!("code_ast: invalid arguments: {e}"), "code_ast");
            }
        };

        let kind = parsed.kind.trim().to_string();
        if kind.is_empty() {
            return ToolOutput::new("code_ast: `kind` must not be empty", "code_ast");
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

        // The `ast_search.enabled` config flag is the only gate: the tool is
        // registered conditionally (see instance_store) and refuses to run
        // when disabled. There is no plugin-declaration gate — `code_ast` is
        // a built-in tool, not an external plugin.
        let root = ctx.root.clone();
        let filters = crate::agent::ast_search::Filters::from_value(&parsed.merged_filters());
        let limit = parsed.limit;
        let max_files = config.ast_search.max_files;
        let languages = config.ast_search.languages.clone();

        tokio::select! {
            _ = ctx.abort.cancelled() => {
                return ToolOutput::new("code_ast: aborted", "code_ast");
            }
            result = tokio::task::spawn_blocking(move || {
                crate::agent::ast_search::search(&root, &kind, &filters, limit, max_files, &languages)
            }) => {
                match result {
                    Ok(resp) => {
                        let text = serde_json::to_string_pretty(&resp).unwrap_or_default();
                        ToolOutput::new(text, "code_ast")
                    }
                    Err(e) => {
                        ToolOutput::new(
                            serde_json::to_string(&json!({
                                "ok": false,
                                "error": format!("search panicked: {e}")
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
        let enums = schema["properties"]["kind"]["enum"].as_array().unwrap();
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
    async fn ignores_stale_plugin_declaration() {
        // `code_ast` is a built-in tool gated only by `ast_search.enabled`:
        // even a disabled `bebok-ast` declaration file must not block it.
        let root =
            std::env::temp_dir().join(format!("bebok-ast-decl-ignored-{}", uuid::Uuid::new_v4()));
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
        std::fs::write(root.join("test.rs"), "fn hello() {}\n").unwrap();

        let ctx = ToolCtx {
            root: root.clone(),
            session_id: "test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = CodeAstSearch.execute(ctx, json!({ "kind": "fn" })).await;
        assert!(out.text.contains("\"ok\": true"), "got: {}", out.text);
        assert!(out.text.contains("hello"), "got: {}", out.text);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn rejects_empty_kind() {
        let root = std::env::temp_dir().join(format!("bebok-ast-empty-{}", uuid::Uuid::new_v4()));
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
        let out = CodeAstSearch.execute(ctx, json!({ "kind": "  " })).await;
        assert!(out.text.contains("must not be empty"), "got: {}", out.text);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn returns_results_when_enabled() {
        let root = std::env::temp_dir().join(format!("bebok-ast-results-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".bebok")).unwrap();
        std::fs::write(
            root.join(".bebok").join("config.json"),
            r#"{ "ast_search": { "enabled": true } }"#,
        )
        .unwrap();
        std::fs::write(
            root.join("test.rs"),
            r#"
fn hello() {}
pub fn world() -> i32 { 42 }
"#,
        )
        .unwrap();

        let ctx = ToolCtx {
            root: root.clone(),
            session_id: "test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = CodeAstSearch.execute(ctx, json!({ "kind": "fn" })).await;
        assert!(out.text.contains("\"ok\": true"), "got: {}", out.text);
        assert!(out.text.contains("hello"), "got: {}", out.text);
        assert!(out.text.contains("world"), "got: {}", out.text);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn flat_filters_merge_into_filters_object() {
        // Callers may pass filter keys flat at the top level; they land in
        // the merged filters object.
        let args: AstArgs = serde_json::from_value(json!({
            "kind": "fn",
            "name_regex": "^hello$",
            "limit": 5,
        }))
        .unwrap();
        let merged = args.merged_filters();
        assert_eq!(merged["name_regex"], json!("^hello$"));
    }

    #[test]
    fn nested_filters_win_over_flat_ones() {
        let args: AstArgs = serde_json::from_value(json!({
            "kind": "fn",
            "filters": { "name_regex": "^nested$" },
            "name_regex": "^flat$",
        }))
        .unwrap();
        let merged = args.merged_filters();
        assert_eq!(merged["name_regex"], json!("^nested$"));
    }

    #[tokio::test]
    async fn flat_name_regex_filters_end_to_end() {
        // Regression: flat filter keys used to be silently dropped, so a
        // `name_regex` query returned every function in the tree.
        let root = std::env::temp_dir().join(format!("bebok-ast-flat-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".bebok")).unwrap();
        std::fs::write(
            root.join(".bebok").join("config.json"),
            r#"{ "ast_search": { "enabled": true } }"#,
        )
        .unwrap();
        std::fs::write(root.join("test.rs"), "fn hello() {}\nfn world() {}\n").unwrap();

        let ctx = ToolCtx {
            root: root.clone(),
            session_id: "test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = CodeAstSearch
            .execute(ctx, json!({ "kind": "fn", "name_regex": "^hello$" }))
            .await;
        let v: serde_json::Value = serde_json::from_str(&out.text).unwrap();
        let names: Vec<&str> = v["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["hello"], "got: {}", out.text);

        let _ = std::fs::remove_dir_all(&root);
    }
}
