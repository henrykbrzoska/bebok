use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

use crate::query::{Filters, LanguageHandler, MatchResult};

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
}

impl LanguageHandler for TsLang {
    fn supports_kind(&self, kind: &str) -> bool {
        matches!(kind, "impl" | "struct" | "fn" | "enum" | "type_alias" | "test")
    }

    fn query(&self, content: &str, kind: &str, filters: &Filters) -> Vec<MatchResult> {
        // Determine if this is TSX or plain TS.
        let is_tsx = content.contains("jsx") || kind == "impl";
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

            // Apply name filter.
            if !filters.matches_name(&name) {
                continue;
            }

            // For test kind, only include items whose name starts with "test" or "it" or "describe".
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

            // For struct kind in TS, match class/interface declarations.
            // For impl kind in TS, match class with extends/implements.

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
        "impl" => "[
            (class_declaration) @item
            (abstract_class_declaration) @item
        ]",
        "struct" => "[
            (class_declaration) @item
            (interface_declaration) @item
        ]",
        "fn" => "[
            (function_declaration) @item
            (arrow_function) @item
            (method_definition) @item
        ]",
        "enum" => "(enum_declaration) @item",
        "type_alias" => "(type_alias_declaration) @item",
        "test" => "[
            (call_expression) @item
        ]",
        _ => "(function_declaration) @item",
    }
}

fn extract_ts_name(node: tree_sitter::Node, source: &[u8]) -> String {
    if let Some(child) = node.child_by_field_name("name") {
        return child
            .utf8_text(source)
            .unwrap_or("")
            .to_string();
    }

    // For call_expression (test), extract the function name.
    if node.kind() == "call_expression"
        && let Some(fn_node) = node.child_by_field_name("function")
    {
        return fn_node
            .utf8_text(source)
            .unwrap_or("")
            .to_string();
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
