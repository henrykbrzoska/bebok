//! Editor completion without AI (`POST /fs/complete`, Explorer editor).
//!
//! Ranked identifiers for the caret position, from the **unsaved buffer**
//! (tree-sitter) and the project symbol index ([`super::symbol_index`]):
//!
//! 1. `local`   — variables and parameters declared before the caret in the
//!    enclosing function (or at file level);
//! 2. `file`    — definitions of the file being edited;
//! 3. `project` — indexed definitions of the other project files;
//! 4. `keyword` — static per-language keyword list.
//!
//! Items are filtered by the identifier prefix before the caret, deduplicated
//! by label (the best source wins) and capped by `limit`.
//!
//! Purely syntactic: the scope is the nearest enclosing function (block
//! scoping is ignored), there is no type inference and no member completion.
//! `offset` is a UTF-8 byte offset into `text`.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use super::goto_definition::{self, Lang, SymbolHit};
use super::symbol_index::SymbolIndex;

pub const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;

/// Request body of `POST /fs/complete?directory=`.
#[derive(Debug, Deserialize)]
pub struct CompleteRequest {
    /// Project-relative path of the buffer (picks the language).
    pub path: String,
    /// Unsaved buffer content.
    pub text: String,
    /// UTF-8 byte offset of the caret in `text`.
    pub offset: usize,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One suggestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompleteItem {
    pub label: String,
    /// `variable`, `parameter`, `function`, `class`, `interface`, `enum`,
    /// `type`, `constant`, `property`, `module`, `symbol`, `keyword`.
    pub kind: String,
    /// Tree-sitter node kind for symbols.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    /// `local` | `file` | `project` | `keyword`.
    pub source: &'static str,
}

#[derive(Debug, Serialize)]
pub struct CompleteResponse {
    /// Identifier typed before the caret (PHP keeps the `$`).
    pub prefix: String,
    pub items: Vec<CompleteItem>,
}

/// Identifier (PHP: with leading `$`) that ends at `offset`.
pub fn prefix_at(text: &str, offset: usize, php: bool) -> String {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return String::new();
    }
    let head = &text[..offset];
    let start = head
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_' || (php && *c == '$'))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(offset);
    head[start..].to_string()
}

/// Ranked completion for one caret position.
pub fn complete(root: &Path, index: &SymbolIndex, req: &CompleteRequest) -> CompleteResponse {
    let limit = req.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let rel = req.path.replace('\\', "/");
    let text = req.text.as_str();
    let offset = req.offset.min(text.len());
    let lang = Path::new(&rel)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|e| Lang::from_ext(&e.to_ascii_lowercase()));
    let php = lang == Some(Lang::Php);
    let prefix = prefix_at(text, offset, php);
    let key = prefix.trim_start_matches('$').to_lowercase();

    let mut cands: Vec<CompleteItem> = Vec::new();
    if let Some(lang) = lang {
        for (name, kind) in visible_locals(lang, text, offset) {
            cands.push(CompleteItem {
                label: if php { format!("${name}") } else { name },
                kind: kind.to_string(),
                detail: None,
                path: None,
                line: None,
                source: "local",
            });
        }
        for h in goto_definition::symbols_in_file(&rel, text) {
            cands.push(symbol_item(&h, "file"));
        }
    }
    index.refresh(root);
    for h in index.starts_with(&key, Some(&rel), limit * 4) {
        cands.push(symbol_item(&h, "project"));
    }
    if let Some(lang) = lang {
        for kw in keywords(lang) {
            cands.push(CompleteItem {
                label: (*kw).to_string(),
                kind: "keyword".to_string(),
                detail: None,
                path: None,
                line: None,
                source: "keyword",
            });
        }
    }

    cands.retain(|c| {
        c.label
            .trim_start_matches('$')
            .to_lowercase()
            .starts_with(&key)
    });
    let mut seen: HashSet<String> = HashSet::new();
    cands.retain(|c| seen.insert(c.label.clone()));
    cands.truncate(limit);
    CompleteResponse {
        prefix,
        items: cands,
    }
}

fn symbol_item(h: &SymbolHit, source: &'static str) -> CompleteItem {
    CompleteItem {
        label: h.name.clone(),
        kind: symbol_kind(&h.kind).to_string(),
        detail: Some(h.kind.clone()),
        path: Some(h.path.clone()),
        line: Some(h.line),
        source,
    }
}

/// Map a tree-sitter definition node kind to a coarse item kind.
fn symbol_kind(ts: &str) -> &'static str {
    if ts.contains("function") || ts.contains("method") || ts == "func_literal" {
        "function"
    } else if ts.contains("class") || ts.contains("struct") {
        "class"
    } else if ts.contains("interface") || ts.contains("trait") {
        "interface"
    } else if ts.contains("enum") {
        "enum"
    } else if ts.contains("type") {
        "type"
    } else if ts.contains("const") || ts.contains("static") {
        "constant"
    } else if ts.contains("field") || ts.contains("property") {
        "property"
    } else if ts.contains("mod") || ts.contains("namespace") {
        "module"
    } else {
        "symbol"
    }
}

/// Nodes that open a new local scope (the function a declaration belongs to).
fn scope_kinds(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust => &["function_item", "closure_expression"],
        Lang::Ts | Lang::Tsx | Lang::Js => &[
            "function_declaration",
            "generator_function_declaration",
            "function_expression",
            "arrow_function",
            "method_definition",
        ],
        Lang::Php => &[
            "function_definition",
            "method_declaration",
            "anonymous_function",
            "arrow_function",
        ],
        Lang::Go => &["function_declaration", "method_declaration", "func_literal"],
        Lang::Python => &["function_definition", "lambda"],
    }
}

/// Local declaration patterns; `@name` is the identifier. Each pattern is
/// dropped when the grammar does not know it (a grammar bump degrades).
fn local_patterns(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust => &[
            "(let_declaration pattern: (identifier) @name)",
            "(parameter pattern: (identifier) @name)",
            "(closure_parameters (identifier) @name)",
        ],
        Lang::Ts | Lang::Tsx | Lang::Js => &[
            "(variable_declarator name: (identifier) @name)",
            "(required_parameter pattern: (identifier) @name)",
            "(optional_parameter pattern: (identifier) @name)",
            "(formal_parameters (identifier) @name)",
        ],
        Lang::Php => &[
            "(simple_parameter (variable_name (name) @name))",
            "(assignment_expression left: (variable_name (name) @name))",
        ],
        Lang::Go => &[
            "(short_var_declaration left: (expression_list (identifier) @name))",
            "(var_spec name: (identifier) @name)",
            "(parameter_declaration name: (identifier) @name)",
        ],
        Lang::Python => &[
            "(assignment left: (identifier) @name)",
            "(parameters (identifier) @name)",
            "(default_parameter name: (identifier) @name)",
            "(typed_parameter (identifier) @name)",
            "(typed_default_parameter name: (identifier) @name)",
        ],
    }
}

fn local_query(lang: Lang) -> Option<Query> {
    let language = lang.language();
    let source = local_patterns(lang)
        .iter()
        .filter(|p| Query::new(&language, p).is_ok())
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if source.is_empty() {
        return None;
    }
    Query::new(&language, &source).ok()
}

/// Innermost enclosing scope node of `node` (`None` = file level) and whether
/// the declaration sits in a parameter list.
fn scope_of<'t>(node: Node<'t>, scopes: &[&str]) -> (Option<Node<'t>>, bool) {
    let mut is_param = false;
    let mut cur = node.parent();
    while let Some(n) = cur {
        if scopes.contains(&n.kind()) {
            return (Some(n), is_param);
        }
        if n.kind().contains("param") {
            is_param = true;
        }
        cur = n.parent();
    }
    (None, is_param)
}

/// Locals and parameters visible at `offset`: declared before it, inside the
/// enclosing scope (or at file level). Returns `(name, "variable"|"parameter")`.
fn visible_locals(lang: Lang, text: &str, offset: usize) -> Vec<(String, &'static str)> {
    let Some(query) = local_query(lang) else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&lang.language()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else {
        return Vec::new();
    };
    let Some(name_idx) = query.capture_index_for_name("name") else {
        return Vec::new();
    };
    let src = text.as_bytes();
    let scopes = scope_kinds(lang);
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), src);
    let mut out: Vec<(String, &'static str)> = Vec::new();
    while let Some(m) = matches.next() {
        for c in m.captures.iter().filter(|c| c.index == name_idx) {
            let node = c.node;
            // Not yet declared at the caret; this also skips the word being typed.
            if node.end_byte() >= offset {
                continue;
            }
            let (scope, is_param) = scope_of(node, scopes);
            if scope.is_some_and(|s| offset < s.start_byte() || offset > s.end_byte()) {
                continue;
            }
            let Ok(name) = node.utf8_text(src) else {
                continue;
            };
            out.push((
                name.to_string(),
                if is_param { "parameter" } else { "variable" },
            ));
        }
    }
    out
}

fn keywords(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust => &[
            "as", "async", "await", "break", "const", "continue", "crate", "else", "enum",
            "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
            "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
            "trait", "true", "type", "unsafe", "use", "where", "while",
        ],
        Lang::Ts | Lang::Tsx | Lang::Js => &[
            "async",
            "await",
            "break",
            "case",
            "catch",
            "class",
            "const",
            "continue",
            "debugger",
            "default",
            "delete",
            "do",
            "else",
            "export",
            "extends",
            "false",
            "finally",
            "for",
            "from",
            "function",
            "if",
            "import",
            "in",
            "instanceof",
            "let",
            "new",
            "null",
            "return",
            "static",
            "switch",
            "this",
            "throw",
            "true",
            "try",
            "typeof",
            "undefined",
            "var",
            "void",
            "while",
            "yield",
        ],
        Lang::Php => &[
            "abstract",
            "array",
            "as",
            "break",
            "case",
            "catch",
            "class",
            "const",
            "continue",
            "default",
            "do",
            "echo",
            "else",
            "elseif",
            "extends",
            "false",
            "final",
            "finally",
            "fn",
            "for",
            "foreach",
            "function",
            "if",
            "implements",
            "instanceof",
            "interface",
            "namespace",
            "new",
            "null",
            "private",
            "protected",
            "public",
            "return",
            "static",
            "switch",
            "throw",
            "trait",
            "true",
            "try",
            "use",
            "var",
            "while",
            "yield",
        ],
        Lang::Go => &[
            "break",
            "case",
            "chan",
            "const",
            "continue",
            "default",
            "defer",
            "else",
            "fallthrough",
            "for",
            "func",
            "go",
            "goto",
            "if",
            "import",
            "interface",
            "map",
            "package",
            "range",
            "return",
            "select",
            "struct",
            "switch",
            "type",
            "var",
        ],
        Lang::Python => &[
            "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
            "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
            "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return",
            "True", "try", "while", "with", "yield",
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Local names visible right after the first occurrence of `marker`.
    fn locals(lang: Lang, text: &str, marker: &str) -> Vec<String> {
        let at = text.find(marker).expect("marker in source") + marker.len();
        let mut names: Vec<String> = visible_locals(lang, text, at)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        names.sort();
        names
    }

    /// Parameters plus the bindings declared *before* the caret in the same
    /// function, for every supported language in one table - the per-language
    /// syntax is the only thing that differs, so one case each is enough.
    #[test]
    fn params_and_earlier_bindings_are_visible_in_every_language() {
        for (lang, src, want) in [
            (
                Lang::Rust,
                "fn main(arg: i32) {\n    let total = 1;\n    let later = 2;\n    XX\n}\nfn other() { let hidden = 3; }\n",
                vec!["arg", "later", "total"],
            ),
            (
                Lang::Ts,
                "function run(input: string) {\n  const local = 1;\n  let other = 2;\n  XX\n}\nconst outside = 4;\n",
                vec!["input", "local", "other"],
            ),
            (
                Lang::Python,
                "def f(a, b=2):\n    total = 1\n    XX\n\ndef g():\n    hidden = 0\n",
                vec!["a", "b", "total"],
            ),
            (
                Lang::Go,
                "package main\n\nfunc f(p int) {\n\tv := 1\n\tXX\n}\n",
                vec!["p", "v"],
            ),
            (
                Lang::Php,
                "<?php\nfunction f($req, $n) {\n  $tmp = 1;\n  XX\n}\n",
                vec!["n", "req", "tmp"],
            ),
        ] {
            assert_eq!(locals(lang, src, "XX"), want, "{lang:?}");
        }
    }

    /// The scope rule the completion list depends on: a binding declared
    /// after the caret is not in scope yet.
    #[test]
    fn declarations_after_the_caret_are_not_visible() {
        let src = "fn main() {\n    XX\n    let after = 1;\n}\n";
        assert!(locals(Lang::Rust, src, "XX").is_empty());
    }

    #[test]
    fn prefix_is_identifier_before_caret() {
        assert_eq!(prefix_at("foo.ba", 6, false), "ba");
        assert_eq!(prefix_at("echo $us", 8, true), "$us");
        assert_eq!(prefix_at("echo $us", 8, false), "us");
        assert_eq!(prefix_at("abc", 0, false), "");
    }
}
