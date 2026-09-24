use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

use crate::query::{Filters, LanguageHandler, MatchResult};

pub struct RustLang {
    language: Language,
}

impl RustLang {
    pub fn new() -> anyhow::Result<Self> {
        let language = Language::from(tree_sitter_rust::LANGUAGE);
        Ok(Self { language })
    }
}

impl LanguageHandler for RustLang {
    fn supports_kind(&self, kind: &str) -> bool {
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

    fn query(&self, content: &str, kind: &str, filters: &Filters) -> Vec<MatchResult> {
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

            // Apply filters.
            if !filters.matches_name(&name) {
                continue;
            }
            if !filters.matches_path("") {
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
                    // For test kind, only include items with #[test] attribute.
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
            // Match any item that has a #[test] attribute.
            "[
                (function_item) @item
                (macro_definition) @item
            ]"
        }
        _ => "(function_item) @item",
    }
}

fn extract_name(node: tree_sitter::Node, source: &[u8]) -> String {
    // Try common name fields.
    if let Some(child) = node.child_by_field_name("name") {
        return child.utf8_text(source).unwrap_or("").to_string();
    }
    // For use_declaration, extract the full text up to a reasonable length.
    if node.kind() == "use_declaration" {
        return "use ...".to_string();
    }
    // For impl, extract the full impl header.
    if node.kind() == "impl_item" {
        return "impl ...".to_string();
    }
    "<?>".to_string()
}

fn extract_impl_trait(node: tree_sitter::Node, source: &[u8]) -> String {
    let text = node.utf8_text(source).unwrap_or("");
    // Extract trait name from "impl Trait for Type" or "impl Trait".
    let lower = text.to_lowercase();
    if let Some(idx) = lower.find("impl ") {
        let rest = &text[idx + 5..];
        if let Some(end) = rest.find(" for ") {
            return rest[..end].trim().to_string();
        }
        // "impl Trait" without "for" — take until '{' or whitespace boundary.
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
    // In the Rust grammar an attribute (`#[derive(...)]`, `#[test]`, …) is a
    // preceding sibling of the item it decorates, not a child node — walk
    // backwards over a contiguous attribute block.
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
