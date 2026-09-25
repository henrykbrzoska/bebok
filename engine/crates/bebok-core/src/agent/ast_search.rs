//! Embedded AST-aware structural code search using tree-sitter.
//!
//! This module is built directly into `bebok-core` — no external CLI binary
//! required. It supports Rust (.rs) and TypeScript (.ts/.tsx) files.

use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

/// Filters extracted from the request JSON.
#[derive(Debug, Clone, Default)]
pub struct Filters {
    pub trait_name: Option<String>,
    pub derive: Option<String>,
    pub return_type: Option<String>,
    pub annotation: Option<String>,
    pub name_regex: Option<Regex>,
    pub path_regex: Option<Regex>,
    pub ext: Option<String>,
}

impl Filters {
    pub fn from_value(v: &Value) -> Self {
        let obj = v.as_object();
        let get_str = |key: &str| -> Option<String> {
            obj?.get(key).and_then(|v| v.as_str()).map(str::to_string)
        };
        let get_re = |key: &str| -> Option<Regex> {
            let s = get_str(key)?;
            Regex::new(&s).ok()
        };

        Self {
            trait_name: get_str("trait"),
            derive: get_str("derive"),
            return_type: get_str("return_type"),
            annotation: get_str("annotation"),
            name_regex: get_re("name_regex"),
            path_regex: get_re("path_regex"),
            ext: get_str("ext"),
        }
    }

    /// Returns the file extensions relevant for a given kind,
    /// intersected with the configured `languages` (if non-empty).
    pub fn exts_for_kind(&self, kind: &str, languages: &[String]) -> Vec<String> {
        if let Some(ref ext) = self.ext {
            return vec![ext.clone()];
        }
        let base: &[&str] = match kind {
            "const" | "static" | "mod" | "use" | "trait" => &["rs"],
            "impl" | "struct" | "fn" | "enum" | "type_alias" | "test" => &["rs", "ts", "tsx"],
            _ => &["rs", "ts", "tsx"],
        };
        if languages.is_empty() {
            return base.iter().map(|s| s.to_string()).collect();
        }
        base.iter()
            .filter(|e| languages.iter().any(|l| l == **e))
            .map(|s| s.to_string())
            .collect()
    }

    pub fn matches_name(&self, name: &str) -> bool {
        match &self.name_regex {
            Some(re) => re.is_match(name),
            None => true,
        }
    }

    pub fn matches_path(&self, path: &str) -> bool {
        match &self.path_regex {
            Some(re) => re.is_match(path),
            None => true,
        }
    }

    pub fn matches_annotation(&self, attrs: &str) -> bool {
        match &self.annotation {
            Some(a) => attrs.contains(a.as_str()),
            None => true,
        }
    }

    pub fn matches_derive(&self, attrs: &str) -> bool {
        match &self.derive {
            Some(d) => attrs.contains(d.as_str()),
            None => true,
        }
    }

    pub fn matches_return_type(&self, ret: &str) -> bool {
        match &self.return_type {
            Some(rt) => ret.contains(rt.as_str()),
            None => true,
        }
    }

    pub fn matches_trait(&self, trait_name: &str) -> bool {
        match &self.trait_name {
            Some(t) => trait_name.contains(t.as_str()),
            None => true,
        }
    }
}

/// A single query match result.
#[derive(Debug, Clone)]
pub struct MatchResult {
    pub line: usize,
    pub column: usize,
    pub kind: String,
    pub name: String,
    pub snippet: String,
}

// ---------------------------------------------------------------------------
// File walking
// ---------------------------------------------------------------------------

/// Collected files plus whether the `max_files` cap cut the walk short.
pub struct CollectedFiles {
    pub files: Vec<std::path::PathBuf>,
    pub hit_cap: bool,
}

/// Collect files matching the given extensions, filtered by `path_regex`
/// *before* the `max_files` cap is applied (so a path filter can never be
/// starved by unrelated files earlier in walk order). The result is sorted
/// for stable output across calls. Uses the `ignore` crate to respect
/// `.gitignore`.
pub fn collect_files(
    root: &Path,
    exts: &[String],
    max_files: usize,
    path_filter: &Filters,
) -> CollectedFiles {
    let ext_set: HashSet<&str> = exts.iter().map(|s| s.as_str()).collect();
    let mut results = Vec::new();
    let mut hit_cap = false;

    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .max_depth(Some(20))
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !ext_set.contains(ext) {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        if !path_filter.matches_path(&rel) {
            continue;
        }
        results.push(path.to_path_buf());
        if results.len() >= max_files {
            hit_cap = true;
            break;
        }
    }

    results.sort();
    CollectedFiles {
        files: results,
        hit_cap,
    }
}

// ---------------------------------------------------------------------------
// Rust language handler
// ---------------------------------------------------------------------------

pub struct RustLang {
    language: Language,
}

impl RustLang {
    pub fn new() -> anyhow::Result<Self> {
        let language = Language::from(tree_sitter_rust::LANGUAGE);
        Ok(Self { language })
    }

    pub fn supports_kind(&self, kind: &str) -> bool {
        matches!(
            kind,
            "impl"
                | "struct"
                | "fn"
                | "enum"
                | "trait"
                | "type_alias"
                | "const"
                | "static"
                | "mod"
                | "use"
                | "test"
        )
    }

    pub fn query(&self, content: &str, kind: &str, filters: &Filters) -> Vec<MatchResult> {
        let mut parser = Parser::new();
        if parser.set_language(&self.language).is_err() {
            return Vec::new();
        }
        let tree = match parser.parse(content, None) {
            Some(t) => t,
            None => return Vec::new(),
        };

        let query_str = rust_query_for_kind(kind);
        let query = match Query::new(&self.language, query_str) {
            Ok(q) => q,
            Err(_) => return Vec::new(),
        };

        let mut cursor = QueryCursor::new();
        let mut results = Vec::new();
        let source_bytes = content.as_bytes();
        let mut matches = cursor.matches(&query, tree.root_node(), source_bytes);

        while let Some(m) = matches.next() {
            if m.captures.is_empty() {
                continue;
            }
            let node = m.captures[0].node;

            let line = node.start_position().row + 1;
            let column = node.start_position().column + 1;

            let name = extract_name(node, source_bytes);
            let attrs = extract_attributes(node, source_bytes);
            let snippet = extract_snippet(content, node.start_position().row, 3);

            if !filters.matches_name(&name) {
                continue;
            }
            if !filters.matches_annotation(&attrs) {
                continue;
            }

            match kind {
                "impl" => {
                    let trait_name = extract_impl_trait(node, source_bytes);
                    if !filters.matches_trait(&trait_name) {
                        continue;
                    }
                }
                "struct" => {
                    if !filters.matches_derive(&attrs) {
                        continue;
                    }
                }
                "fn" => {
                    let ret = extract_return_type(node, source_bytes);
                    if !filters.matches_return_type(&ret) {
                        continue;
                    }
                }
                "test" if !attrs.contains("#[test]") => {
                    continue;
                }
                _ => {}
            }
            results.push(MatchResult {
                line,
                column,
                kind: kind.to_string(),
                name,
                snippet,
            });
        }

        results
    }
}

fn rust_query_for_kind(kind: &str) -> &str {
    match kind {
        "impl" => "(impl_item) @item",
        "struct" => "(struct_item) @item",
        "fn" => "(function_item) @item",
        "enum" => "(enum_item) @item",
        "trait" => "(trait_item) @item",
        "type_alias" => "(type_item) @item",
        "const" => "(const_item) @item",
        "static" => "(static_item) @item",
        "mod" => "(mod_item) @item",
        "use" => "(use_declaration) @item",
        "test" => {
            "[
            (function_item) @item
            (macro_definition) @item
        ]"
        }
        _ => "(function_item) @item",
    }
}

fn extract_name(node: tree_sitter::Node, source: &[u8]) -> String {
    if let Some(child) = node.child_by_field_name("name") {
        return child.utf8_text(source).unwrap_or("").to_string();
    }
    if node.kind() == "use_declaration" {
        return "use ...".to_string();
    }
    if node.kind() == "impl_item" {
        return "impl ...".to_string();
    }
    "<?>".to_string()
}

fn extract_impl_trait(node: tree_sitter::Node, source: &[u8]) -> String {
    let text = node.utf8_text(source).unwrap_or("");
    let lower = text.to_lowercase();
    if let Some(idx) = lower.find("impl ") {
        let rest = &text[idx + 5..];
        if let Some(end) = rest.find(" for ") {
            return rest[..end].trim().to_string();
        }
        if let Some(end) = rest.find('{') {
            return rest[..end].trim().to_string();
        }
        return rest.trim().to_string();
    }
    String::new()
}

fn extract_return_type(node: tree_sitter::Node, source: &[u8]) -> String {
    if let Some(ret) = node.child_by_field_name("return_type") {
        return ret.utf8_text(source).unwrap_or("").to_string();
    }
    String::new()
}

fn extract_attributes(node: tree_sitter::Node, source: &[u8]) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let mut prev = node.prev_named_sibling();
    while let Some(sib) = prev {
        if sib.kind() != "attribute_item" {
            break;
        }
        if let Ok(text) = sib.utf8_text(source) {
            parts.push(text);
        }
        prev = sib.prev_named_sibling();
    }
    parts.reverse();
    let mut attrs = parts.join(" ");
    if !attrs.is_empty() {
        attrs.push(' ');
    }
    attrs
}

fn extract_snippet(content: &str, start_row: usize, max_lines: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let end = (start_row + max_lines).min(lines.len());
    if start_row >= lines.len() {
        return String::new();
    }
    lines[start_row..end].join("\n")
}

// ---------------------------------------------------------------------------
// TypeScript language handler
// ---------------------------------------------------------------------------

pub struct TsLang {
    ts_language: Language,
    tsx_language: Language,
}

impl TsLang {
    pub fn new() -> anyhow::Result<Self> {
        let ts_language = Language::from(tree_sitter_typescript::LANGUAGE_TYPESCRIPT);
        let tsx_language = Language::from(tree_sitter_typescript::LANGUAGE_TSX);
        Ok(Self {
            ts_language,
            tsx_language,
        })
    }

    pub fn supports_kind(&self, kind: &str) -> bool {
        matches!(
            kind,
            "impl" | "struct" | "fn" | "enum" | "type_alias" | "test"
        )
    }

    pub fn query(
        &self,
        content: &str,
        kind: &str,
        is_tsx: bool,
        filters: &Filters,
    ) -> Vec<MatchResult> {
        let lang = if is_tsx {
            &self.tsx_language
        } else {
            &self.ts_language
        };

        let mut parser = Parser::new();
        if parser.set_language(lang).is_err() {
            return Vec::new();
        }
        let tree = match parser.parse(content, None) {
            Some(t) => t,
            None => return Vec::new(),
        };

        let query_str = ts_query_for_kind(kind);
        let query = match Query::new(lang, query_str) {
            Ok(q) => q,
            Err(_) => return Vec::new(),
        };

        let mut cursor = QueryCursor::new();
        let mut results = Vec::new();
        let source_bytes = content.as_bytes();
        let mut matches = cursor.matches(&query, tree.root_node(), source_bytes);

        while let Some(m) = matches.next() {
            if m.captures.is_empty() {
                continue;
            }
            let node = m.captures[0].node;

            let line = node.start_position().row + 1;
            let column = node.start_position().column + 1;

            let name = extract_ts_name(node, source_bytes);
            let snippet = extract_ts_snippet(content, node.start_position().row, 3);

            if !filters.matches_name(&name) {
                continue;
            }

            if kind == "test" {
                let lower = name.to_lowercase();
                if !lower.starts_with("test")
                    && !lower.starts_with("it(")
                    && !lower.starts_with("describe(")
                    && !lower.starts_with("it ")
                    && !lower.starts_with("describe ")
                {
                    continue;
                }
            }

            results.push(MatchResult {
                line,
                column,
                kind: kind.to_string(),
                name,
                snippet,
            });
        }

        results
    }
}

fn ts_query_for_kind(kind: &str) -> &str {
    match kind {
        "impl" => {
            "[
            (class_declaration) @item
            (abstract_class_declaration) @item
        ]"
        }
        "struct" => {
            "[
            (class_declaration) @item
            (interface_declaration) @item
        ]"
        }
        "fn" => {
            "[
            (function_declaration) @item
            (arrow_function) @item
            (method_definition) @item
        ]"
        }
        "enum" => "(enum_declaration) @item",
        "type_alias" => "(type_alias_declaration) @item",
        "test" => "(call_expression) @item",
        _ => "(function_declaration) @item",
    }
}

fn extract_ts_name(node: tree_sitter::Node, source: &[u8]) -> String {
    if let Some(child) = node.child_by_field_name("name") {
        return child.utf8_text(source).unwrap_or("").to_string();
    }
    if node.kind() == "call_expression"
        && let Some(fn_node) = node.child_by_field_name("function")
    {
        return fn_node.utf8_text(source).unwrap_or("").to_string();
    }
    "<?>".to_string()
}

fn extract_ts_snippet(content: &str, start_row: usize, max_lines: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let end = (start_row + max_lines).min(lines.len());
    if start_row >= lines.len() {
        return String::new();
    }
    lines[start_row..end].join("\n")
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Response shape for `code_ast`.
#[derive(Debug, serde::Serialize)]
pub struct AstResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<AstResult>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub parsed_files: usize,
    /// Files matching the extension filter (before `path_regex`).
    pub scanned_files: usize,
    /// Project-relative search root the query ran against.
    pub root: String,
    /// True when `max_files` or `limit` cut the result set short.
    pub truncated: bool,
    pub duration_ms: u128,
}

/// One match result.
#[derive(Debug, serde::Serialize)]
pub struct AstResult {
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub kind: String,
    pub name: String,
    pub snippet: String,
}

/// Run an AST search over `root` for the given `kind` and `filters`.
pub fn search(
    root: &Path,
    kind: &str,
    filters: &Filters,
    limit: usize,
    max_files: usize,
    languages: &[String],
) -> AstResponse {
    let start = std::time::Instant::now();

    if !is_known_kind(kind) {
        return AstResponse {
            ok: false,
            results: None,
            error: Some(format!(
                "unknown kind '{kind}'; expected one of: impl, struct, fn, enum, trait, type_alias, const, static, mod, use, test"
            )),
            parsed_files: 0,
            scanned_files: 0,
            root: root.to_string_lossy().to_string(),
            truncated: false,
            duration_ms: start.elapsed().as_millis(),
        };
    }

    let exts = filters.exts_for_kind(kind, languages);
    let collected = collect_files(root, &exts, max_files, filters);
    let files = collected.files;
    // Full denominator walk only when the cap actually cut the walk short;
    // otherwise everything matching was collected.
    let scanned_files = if collected.hit_cap {
        count_ext_files(root, &exts)
    } else {
        files.len()
    };

    let rust_lang = match RustLang::new() {
        Ok(l) => l,
        Err(e) => {
            return AstResponse {
                ok: false,
                results: None,
                error: Some(format!("failed to init Rust parser: {e}")),
                parsed_files: 0,
                scanned_files: 0,
                root: root.to_string_lossy().to_string(),
                truncated: false,
                duration_ms: 0,
            };
        }
    };
    let ts_lang = match TsLang::new() {
        Ok(l) => l,
        Err(e) => {
            return AstResponse {
                ok: false,
                results: None,
                error: Some(format!("failed to init TS parser: {e}")),
                parsed_files: 0,
                scanned_files: 0,
                root: root.to_string_lossy().to_string(),
                truncated: false,
                duration_ms: 0,
            };
        }
    };

    let mut results = Vec::new();
    let mut parsed_files = 0usize;
    // The walk stopped at the cap: more matching files may exist.
    let mut truncated = collected.hit_cap;

    for path in &files {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let is_tsx = ext == "tsx";
        let matched = match ext {
            "rs" => rust_lang.query(&content_or_warn(path), kind, filters),
            "ts" | "tsx" => ts_lang.query(&content_or_warn(path), kind, is_tsx, filters),
            _ => continue,
        };

        parsed_files += 1;

        for m in matched {
            results.push(AstResult {
                path: rel.clone(),
                line: m.line,
                column: m.column,
                kind: m.kind,
                name: m.name,
                snippet: m.snippet,
            });
            if results.len() >= limit {
                truncated = true;
                break;
            }
        }
        if results.len() >= limit {
            truncated = true;
            break;
        }
    }

    let duration_ms = start.elapsed().as_millis();

    AstResponse {
        ok: true,
        results: Some(results),
        error: None,
        parsed_files,
        scanned_files,
        root: root.to_string_lossy().to_string(),
        truncated,
        duration_ms,
    }
}

/// Count files matching the extension filter (ignores `max_files` and
/// `path_regex`): the denominator for `truncated`.
fn count_ext_files(root: &Path, exts: &[String]) -> usize {
    let ext_set: HashSet<&str> = exts.iter().map(|s| s.as_str()).collect();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .max_depth(Some(20))
        .build();
    walker
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_some_and(|ft| ft.is_file()))
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| ext_set.contains(x))
        })
        .count()
}

fn content_or_warn(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("code_ast: skipping unreadable file {}: {e}", path.display());
            String::new()
        }
    }
}

/// Returns true for a supported `kind` value.
pub fn is_known_kind(kind: &str) -> bool {
    matches!(
        kind,
        "impl"
            | "struct"
            | "fn"
            | "enum"
            | "trait"
            | "type_alias"
            | "const"
            | "static"
            | "mod"
            | "use"
            | "test"
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_from_value_parses_all_fields() {
        let v = serde_json::json!({
            "trait": "Display",
            "derive": "Serialize",
            "return_type": "Result",
            "annotation": "#[test]",
            "name_regex": "main",
            "path_regex": "src/",
            "ext": "rs",
        });
        let f = Filters::from_value(&v);
        assert_eq!(f.trait_name, Some("Display".to_string()));
        assert_eq!(f.derive, Some("Serialize".to_string()));
        assert_eq!(f.return_type, Some("Result".to_string()));
        assert_eq!(f.annotation, Some("#[test]".to_string()));
        assert!(f.name_regex.is_some());
        assert!(f.path_regex.is_some());
        assert_eq!(f.ext, Some("rs".to_string()));
    }

    #[test]
    fn filters_exts_for_kind_rust_only() {
        let f = Filters::default();
        assert_eq!(f.exts_for_kind("trait", &[]), vec!["rs"]);
        assert_eq!(f.exts_for_kind("const", &[]), vec!["rs"]);
        assert_eq!(f.exts_for_kind("fn", &[]), vec!["rs", "ts", "tsx"]);
    }

    #[test]
    fn filters_exts_for_kind_explicit_ext() {
        let f = Filters {
            ext: Some("rs".to_string()),
            ..Default::default()
        };
        assert_eq!(f.exts_for_kind("fn", &[]), vec!["rs"]);
    }

    #[test]
    fn filters_matches_name() {
        let f = Filters {
            name_regex: Some(Regex::new(r"^main").unwrap()),
            ..Default::default()
        };
        assert!(f.matches_name("main"));
        assert!(!f.matches_name("foo_main"));
    }

    #[test]
    fn filters_matches_path() {
        let f = Filters {
            path_regex: Some(Regex::new(r"src/").unwrap()),
            ..Default::default()
        };
        assert!(f.matches_path("src/foo.rs"));
        assert!(!f.matches_path("tests/foo.rs"));
    }

    #[test]
    fn filters_matches_annotation() {
        let f = Filters {
            annotation: Some("#[test]".to_string()),
            ..Default::default()
        };
        assert!(f.matches_annotation("#[test] fn foo()"));
        assert!(!f.matches_annotation("#[derive(Serialize)] fn foo()"));
    }

    #[test]
    fn filters_matches_derive() {
        let f = Filters {
            derive: Some("Serialize".to_string()),
            ..Default::default()
        };
        assert!(f.matches_derive("#[derive(Serialize, Deserialize)]"));
        assert!(!f.matches_derive("#[derive(Debug)]"));
    }

    #[test]
    fn filters_matches_return_type() {
        let f = Filters {
            return_type: Some("Result".to_string()),
            ..Default::default()
        };
        assert!(f.matches_return_type("Result<(), Error>"));
        assert!(!f.matches_return_type("String"));
    }

    #[test]
    fn filters_matches_trait() {
        let f = Filters {
            trait_name: Some("Display".to_string()),
            ..Default::default()
        };
        assert!(f.matches_trait("Display"));
        assert!(!f.matches_trait("Debug"));
    }

    #[test]
    fn rust_lang_supports_kinds() {
        let lang = RustLang::new().unwrap();
        assert!(lang.supports_kind("impl"));
        assert!(lang.supports_kind("struct"));
        assert!(lang.supports_kind("fn"));
        assert!(lang.supports_kind("enum"));
        assert!(lang.supports_kind("trait"));
        assert!(lang.supports_kind("type_alias"));
        assert!(lang.supports_kind("const"));
        assert!(lang.supports_kind("static"));
        assert!(lang.supports_kind("mod"));
        assert!(lang.supports_kind("use"));
        assert!(lang.supports_kind("test"));
        assert!(!lang.supports_kind("class"));
    }

    #[test]
    fn ts_lang_supports_kinds() {
        let lang = TsLang::new().unwrap();
        assert!(lang.supports_kind("impl"));
        assert!(lang.supports_kind("struct"));
        assert!(lang.supports_kind("fn"));
        assert!(lang.supports_kind("enum"));
        assert!(lang.supports_kind("type_alias"));
        assert!(lang.supports_kind("test"));
        assert!(!lang.supports_kind("trait"));
    }

    #[test]
    fn rust_query_returns_results_for_fn() {
        let lang = RustLang::new().unwrap();
        let code = r#"
fn main() {}
pub fn foo(x: i32) -> i32 { x }
"#;
        let filters = Filters::default();
        let results = lang.query(code, "fn", &filters);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "main");
        assert_eq!(results[1].name, "foo");
    }

    #[test]
    fn rust_query_returns_results_for_struct() {
        let lang = RustLang::new().unwrap();
        let code = r#"
#[derive(Serialize, Deserialize)]
pub struct Foo {
    pub name: String,
}
"#;
        let filters = Filters::default();
        let results = lang.query(code, "struct", &filters);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Foo");
    }

    #[test]
    fn rust_query_filters_by_derive() {
        let lang = RustLang::new().unwrap();
        let code = r#"
#[derive(Serialize)]
pub struct A {}

#[derive(Debug)]
pub struct B {}
"#;
        let filters = Filters {
            derive: Some("Serialize".to_string()),
            ..Default::default()
        };
        let results = lang.query(code, "struct", &filters);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "A");
    }

    #[test]
    fn rust_query_filters_by_return_type() {
        let lang = RustLang::new().unwrap();
        let code = r#"
fn foo() -> Result<(), Error> { Ok(()) }
fn bar() -> String { "hi".to_string() }
"#;
        let filters = Filters {
            return_type: Some("Result".to_string()),
            ..Default::default()
        };
        let results = lang.query(code, "fn", &filters);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "foo");
    }

    #[test]
    fn rust_query_returns_test_items() {
        let lang = RustLang::new().unwrap();
        let code = r#"
#[test]
fn test_foo() {}

fn not_a_test() {}
"#;
        let filters = Filters::default();
        let results = lang.query(code, "test", &filters);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "test_foo");
    }

    #[test]
    fn ts_query_returns_results_for_fn() {
        let lang = TsLang::new().unwrap();
        let code = r#"
function main() {}
export function foo(x: number): number { return x; }
"#;
        let filters = Filters::default();
        let results = lang.query(code, "fn", false, &filters);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn ts_query_returns_results_for_class() {
        let lang = TsLang::new().unwrap();
        let code = r#"
class MyClass {}
interface MyInterface {}
"#;
        let filters = Filters::default();
        let results = lang.query(code, "struct", false, &filters);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn search_returns_ok_response() {
        let root = std::env::temp_dir().join(format!("bebok-ast-search-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("test.rs"),
            r#"
fn hello() {}
pub fn world() -> i32 { 42 }
"#,
        )
        .unwrap();

        let filters = Filters::default();
        let resp = search(&root, "fn", &filters, 10, 500, &[]);
        assert!(resp.ok);
        assert!(resp.results.is_some());
        let results = resp.results.unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "hello");
        assert_eq!(results[1].name, "world");
        assert!(resp.parsed_files > 0);
        assert_eq!(resp.scanned_files, 1);
        assert!(!resp.truncated);
        assert_eq!(resp.root, root.to_string_lossy().to_string());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn search_respects_limit() {
        let root = std::env::temp_dir().join(format!("bebok-ast-limit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("test.rs"),
            r#"
fn a() {}
fn b() {}
fn c() {}
"#,
        )
        .unwrap();

        let filters = Filters::default();
        let resp = search(&root, "fn", &filters, 2, 500, &[]);
        assert!(resp.ok);
        assert_eq!(resp.results.as_ref().unwrap().len(), 2);
        assert!(resp.truncated, "limit cut must set truncated");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn search_skips_non_matching_extensions() {
        let root = std::env::temp_dir().join(format!("bebok-ast-ext-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("test.py"), "def foo(): pass\n").unwrap();

        let filters = Filters::default();
        let resp = search(&root, "fn", &filters, 10, 500, &[]);
        assert!(resp.ok);
        // Python files are skipped by default (no Python parser).
        assert_eq!(resp.results.as_ref().map(|r| r.len()).unwrap_or(0), 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn search_rejects_unknown_kind() {
        let root = std::env::temp_dir().join(format!("bebok-ast-kind-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();

        let filters = Filters::default();
        let resp = search(&root, "class", &filters, 10, 500, &[]);
        assert!(!resp.ok);
        assert!(resp.error.as_ref().unwrap().contains("unknown kind"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn search_filters_by_path_regex() {
        let root = std::env::temp_dir().join(format!("bebok-ast-path-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("src").join("a.rs"), "fn in_src() {}\n").unwrap();
        std::fs::write(root.join("tests").join("b.rs"), "fn in_tests() {}\n").unwrap();

        let filters = Filters {
            path_regex: Some(Regex::new("src/").unwrap()),
            ..Default::default()
        };
        let resp = search(&root, "fn", &filters, 10, 500, &[]);
        assert!(resp.ok);
        let results = resp.results.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "in_src");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn search_path_regex_survives_max_files_cap() {
        // Regression: path_regex used to be applied AFTER the max_files cut,
        // so a matching file beyond the cap was silently missed.
        let root = std::env::temp_dir().join(format!("bebok-ast-cap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("aaa")).unwrap();
        std::fs::create_dir_all(root.join("zzz")).unwrap();
        for i in 0..5 {
            std::fs::write(
                root.join("aaa").join(format!("f{i}.rs")),
                format!("fn noise_{i}() {{}}\n"),
            )
            .unwrap();
        }
        std::fs::write(root.join("zzz").join("target.rs"), "fn wanted() {}\n").unwrap();

        let filters = Filters {
            path_regex: Some(Regex::new("zzz/").unwrap()),
            ..Default::default()
        };
        // max_files smaller than the noise, but the filter must still win.
        let resp = search(&root, "fn", &filters, 10, 2, &[]);
        assert!(resp.ok);
        let results = resp.results.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "wanted");
        assert!(!resp.truncated);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn search_max_files_cap_sets_truncated() {
        let root = std::env::temp_dir().join(format!("bebok-ast-trunc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..5 {
            std::fs::write(root.join(format!("f{i}.rs")), format!("fn f{i}() {{}}\n")).unwrap();
        }

        let filters = Filters::default();
        let resp = search(&root, "fn", &filters, 100, 2, &[]);
        assert!(resp.ok);
        assert_eq!(resp.scanned_files, 5);
        assert_eq!(resp.parsed_files, 2);
        assert!(resp.truncated, "max_files cut must set truncated");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn search_respects_languages_config() {
        let root = std::env::temp_dir().join(format!("bebok-ast-lang-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.rs"), "fn rust_fn() {}\n").unwrap();
        std::fs::write(root.join("b.ts"), "function ts_fn() {}\n").unwrap();

        let filters = Filters::default();
        let langs = vec!["rs".to_string()];
        let resp = search(&root, "fn", &filters, 10, 500, &langs);
        assert!(resp.ok);
        let results = resp.results.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "rust_fn");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn exts_for_kind_intersects_languages() {
        let f = Filters::default();
        let langs = vec!["ts".to_string()];
        // "trait" is Rust-only, so intersecting with ["ts"] yields nothing.
        assert!(f.exts_for_kind("trait", &langs).is_empty());
        // "fn" supports both; intersection keeps only ts.
        assert_eq!(f.exts_for_kind("fn", &langs), vec!["ts"]);
    }
}
