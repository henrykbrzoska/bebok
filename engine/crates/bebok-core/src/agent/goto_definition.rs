//! Go-to-definition for the Explorer editor (Ctrl/Cmd+click on a symbol).
//!
//! Purely syntactic, PhpStorm-style "jump to declaration": the clicked
//! identifier is looked up among tree-sitter *definition* nodes (functions,
//! methods, classes, structs, interfaces, traits, enums, types, consts …)
//! across the project files of the same language family. No type
//! inference, so an overloaded name can return several hits — the client
//! shows a picker in that case.
//!
//! Supported: Rust, TypeScript/TSX, JavaScript, PHP, Go and Python. Files are
//! walked gitignore-aware; a cheap `contains(symbol)` pre-filter keeps the
//! parse count to the handful of files that can possibly match.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

/// One definition site.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct DefinitionHit {
    /// Project-relative path with `/` separators.
    pub path: String,
    /// 1-based line of the definition's name.
    pub line: usize,
    /// 1-based column of the definition's name.
    pub column: usize,
    /// Tree-sitter node kind of the definition (`function_item`, `class_declaration`, …).
    pub kind: String,
    /// The source line holding the name, trimmed.
    pub snippet: String,
}

/// Result of [`find_definitions`].
#[derive(Debug, serde::Serialize)]
pub struct DefinitionResponse {
    pub symbol: String,
    pub results: Vec<DefinitionHit>,
    /// Files parsed (after the text pre-filter).
    pub parsed_files: usize,
    /// True when the file cap or `limit` cut the search short.
    pub truncated: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Lang {
    Rust,
    Ts,
    Tsx,
    Js,
    Php,
    Go,
    Python,
}

impl Lang {
    pub(crate) fn from_ext(ext: &str) -> Option<Self> {
        Some(match ext {
            "rs" => Lang::Rust,
            "ts" | "mts" | "cts" => Lang::Ts,
            "tsx" => Lang::Tsx,
            "js" | "jsx" | "mjs" | "cjs" => Lang::Js,
            "php" => Lang::Php,
            "go" => Lang::Go,
            "py" => Lang::Python,
            _ => return None,
        })
    }

    /// Extensions searched when the clicked file is in this language.
    fn family_exts(self) -> &'static [&'static str] {
        match self {
            Lang::Rust => &["rs"],
            Lang::Ts | Lang::Tsx | Lang::Js => {
                &["ts", "mts", "cts", "tsx", "js", "jsx", "mjs", "cjs"]
            }
            Lang::Php => &["php"],
            Lang::Go => &["go"],
            Lang::Python => &["py"],
        }
    }

    pub(crate) fn language(self) -> Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::Js => tree_sitter_javascript::LANGUAGE.into(),
            Lang::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
        }
    }

    /// Definition patterns: `@name` is the identifier, `@def` the declaration.
    /// Each pattern is compiled on its own and dropped when the grammar does
    /// not know a node/field, so a grammar bump degrades instead of breaking.
    fn patterns(self) -> &'static [&'static str] {
        match self {
            Lang::Rust => &[
                "(function_item name: (_) @name) @def",
                "(function_signature_item name: (_) @name) @def",
                "(struct_item name: (_) @name) @def",
                "(enum_item name: (_) @name) @def",
                "(union_item name: (_) @name) @def",
                "(trait_item name: (_) @name) @def",
                "(type_item name: (_) @name) @def",
                "(associated_type name: (_) @name) @def",
                "(const_item name: (_) @name) @def",
                "(static_item name: (_) @name) @def",
                "(mod_item name: (_) @name) @def",
                "(macro_definition name: (_) @name) @def",
                "(enum_variant name: (_) @name) @def",
                "(field_declaration name: (_) @name) @def",
            ],
            Lang::Ts | Lang::Tsx | Lang::Js => &[
                "(function_declaration name: (_) @name) @def",
                "(generator_function_declaration name: (_) @name) @def",
                "(class_declaration name: (_) @name) @def",
                "(abstract_class_declaration name: (_) @name) @def",
                "(interface_declaration name: (_) @name) @def",
                "(type_alias_declaration name: (_) @name) @def",
                "(enum_declaration name: (_) @name) @def",
                "(internal_module name: (_) @name) @def",
                "(method_definition name: (_) @name) @def",
                "(method_signature name: (_) @name) @def",
                "(abstract_method_signature name: (_) @name) @def",
                "(public_field_definition name: (_) @name) @def",
                "(field_definition property: (_) @name) @def",
                "(property_signature name: (_) @name) @def",
                // Top-level / exported `const x = …` (locals would only add noise).
                "(program (lexical_declaration (variable_declarator name: (identifier) @name) @def))",
                "(program (variable_declaration (variable_declarator name: (identifier) @name) @def))",
                "(export_statement (lexical_declaration (variable_declarator name: (identifier) @name) @def))",
            ],
            Lang::Php => &[
                "(function_definition name: (_) @name) @def",
                "(method_declaration name: (_) @name) @def",
                "(class_declaration name: (_) @name) @def",
                "(interface_declaration name: (_) @name) @def",
                "(trait_declaration name: (_) @name) @def",
                "(enum_declaration name: (_) @name) @def",
                "(enum_case name: (_) @name) @def",
                "(const_element (name) @name) @def",
                "(property_element (variable_name (name) @name)) @def",
                "(property_promotion_parameter name: (variable_name (name) @name)) @def",
            ],
            Lang::Go => &[
                "(function_declaration name: (_) @name) @def",
                "(method_declaration name: (_) @name) @def",
                "(type_spec name: (_) @name) @def",
                "(type_alias name: (_) @name) @def",
                "(const_spec name: (_) @name) @def",
                "(source_file (var_declaration (var_spec name: (_) @name) @def))",
                "(field_declaration name: (_) @name) @def",
                "(method_elem name: (_) @name) @def",
                "(method_spec name: (_) @name) @def",
            ],
            Lang::Python => &[
                "(function_definition name: (_) @name) @def",
                "(class_definition name: (_) @name) @def",
                "(module (expression_statement (assignment left: (identifier) @name) @def))",
            ],
        }
    }
}

/// A compiled definition query plus the indexes of its two captures.
struct DefQuery {
    query: Query,
    name_idx: u32,
    def_idx: u32,
}

fn build_query(lang: Lang) -> Option<DefQuery> {
    let language = lang.language();
    let source = lang
        .patterns()
        .iter()
        .filter(|p| Query::new(&language, p).is_ok())
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if source.is_empty() {
        return None;
    }
    let query = Query::new(&language, &source).ok()?;
    let name_idx = query.capture_index_for_name("name")?;
    let def_idx = query.capture_index_for_name("def")?;
    Some(DefQuery {
        query,
        name_idx,
        def_idx,
    })
}

/// Lazily compiled, process-wide query per language.
fn query_for(lang: Lang) -> Option<&'static DefQuery> {
    static RUST: OnceLock<Option<DefQuery>> = OnceLock::new();
    static TS: OnceLock<Option<DefQuery>> = OnceLock::new();
    static TSX: OnceLock<Option<DefQuery>> = OnceLock::new();
    static JS: OnceLock<Option<DefQuery>> = OnceLock::new();
    static PHP: OnceLock<Option<DefQuery>> = OnceLock::new();
    static GO: OnceLock<Option<DefQuery>> = OnceLock::new();
    static PY: OnceLock<Option<DefQuery>> = OnceLock::new();
    let cell = match lang {
        Lang::Rust => &RUST,
        Lang::Ts => &TS,
        Lang::Tsx => &TSX,
        Lang::Js => &JS,
        Lang::Php => &PHP,
        Lang::Go => &GO,
        Lang::Python => &PY,
    };
    cell.get_or_init(|| build_query(lang)).as_ref()
}

/// Definitions of `symbol` inside one source text.
fn definitions_in(lang: Lang, content: &str, symbol: &str) -> Vec<(usize, usize, String, String)> {
    definitions_matching(lang, content, &|name| name == symbol)
        .into_iter()
        .map(|(_, l, c, k, s)| (l, c, k, s))
        .collect()
}

/// Definitions whose name satisfies `accept`, as `(name, line, column, kind, snippet)`.
fn definitions_matching(
    lang: Lang,
    content: &str,
    accept: &dyn Fn(&str) -> bool,
) -> Vec<(String, usize, usize, String, String)> {
    let Some(dq) = query_for(lang) else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&lang.language()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };
    let src = content.as_bytes();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&dq.query, tree.root_node(), src);
    let mut out: Vec<(String, usize, usize, String, String)> = Vec::new();
    while let Some(m) = matches.next() {
        let mut name_node = None;
        let mut def_node = None;
        for c in m.captures {
            if c.index == dq.name_idx {
                name_node = Some(c.node);
            } else if c.index == dq.def_idx {
                def_node = Some(c.node);
            }
        }
        let (Some(name), Some(def)) = (name_node, def_node) else {
            continue;
        };
        let Ok(name_text) = name.utf8_text(src) else {
            continue;
        };
        if !accept(name_text) {
            continue;
        }
        let pos = name.start_position();
        let line = pos.row + 1;
        if out
            .iter()
            .any(|(_, l, c, _, _)| *l == line && *c == pos.column + 1)
        {
            continue;
        }
        let snippet = content
            .lines()
            .nth(pos.row)
            .unwrap_or("")
            .trim()
            .chars()
            .take(200)
            .collect();
        out.push((
            name_text.to_string(),
            line,
            pos.column + 1,
            def.kind().to_string(),
            snippet,
        ));
    }
    out
}

/// Is `symbol` a plausible identifier (and not something that would turn the
/// search into a full-project parse)?
pub fn is_valid_symbol(symbol: &str) -> bool {
    !symbol.is_empty()
        && symbol.len() <= 200
        && symbol
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        && !symbol.chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// Length of the common directory prefix of two `/`-separated paths.
fn shared_dirs(a: &str, b: &str) -> usize {
    a.split('/')
        .zip(b.split('/'))
        .take_while(|(x, y)| x == y)
        .count()
}

/// Find the definitions of `symbol`.
///
/// `from` is the project-relative file the click happened in: it picks the
/// language family (all supported families when absent / unknown) and is
/// searched first. Ranking: same file → a file named after the symbol
/// (`Foo.php`, `foo.rs`, `Foo.tsx`) → nearest directory → path order.
pub fn find_definitions(
    root: &Path,
    from: Option<&str>,
    symbol: &str,
    limit: usize,
    max_files: usize,
) -> DefinitionResponse {
    // PHP `$var` / `$this->x`: definitions carry the bare name.
    let symbol = symbol.trim_start_matches('$');
    let mut resp = DefinitionResponse {
        symbol: symbol.to_string(),
        results: Vec::new(),
        parsed_files: 0,
        truncated: false,
    };
    if !is_valid_symbol(symbol) {
        return resp;
    }
    let from = from.map(|f| f.replace('\\', "/"));
    let from_lang = from
        .as_deref()
        .and_then(|f| Path::new(f).extension().and_then(|e| e.to_str()))
        .and_then(|e| Lang::from_ext(&e.to_ascii_lowercase()));
    let exts: Vec<&str> = match from_lang {
        Some(l) => l.family_exts().to_vec(),
        None => [Lang::Rust, Lang::Ts, Lang::Php, Lang::Go, Lang::Python]
            .iter()
            .flat_map(|l| l.family_exts().iter().copied())
            .collect(),
    };

    let (mut files, truncated) = source_files(root, &exts, max_files);
    resp.truncated = truncated;

    let lower = symbol.to_ascii_lowercase();
    let rank = |rel: &str| -> (u8, std::cmp::Reverse<usize>) {
        if from.as_deref() == Some(rel) {
            return (0, std::cmp::Reverse(usize::MAX));
        }
        let stem = Path::new(rel)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let near = from.as_deref().map(|f| shared_dirs(f, rel)).unwrap_or(0);
        let tier = if stem == lower || stem.replace(['-', '_'], "") == lower {
            1
        } else {
            2
        };
        (tier, std::cmp::Reverse(near))
    };
    files.sort_by(|a, b| rank(&a.1).cmp(&rank(&b.1)).then_with(|| a.1.cmp(&b.1)));

    'files: for (path, rel) in &files {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        if !content.contains(symbol) {
            continue;
        }
        let ext = Path::new(rel)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let Some(lang) = Lang::from_ext(&ext) else {
            continue;
        };
        resp.parsed_files += 1;
        for (line, column, kind, snippet) in definitions_in(lang, &content, symbol) {
            resp.results.push(DefinitionHit {
                path: rel.clone(),
                line,
                column,
                kind,
                snippet,
            });
            if resp.results.len() >= limit {
                resp.truncated = true;
                break 'files;
            }
        }
    }
    resp
}

/// Gitignore-aware walk for source files with one of `exts`, as
/// `(absolute, project-relative with '/')`; `true` when `max_files` cut it.
pub(crate) fn source_files(
    root: &Path,
    exts: &[&str],
    max_files: usize,
) -> (Vec<(PathBuf, String)>, bool) {
    let mut files: Vec<(PathBuf, String)> = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .max_depth(Some(24))
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !matches!(name.as_ref(), ".git" | "node_modules" | "target" | ".bebok")
        })
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !exts.is_empty() && !exts.contains(&ext.as_str()) {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        files.push((path.to_path_buf(), rel));
        if files.len() >= max_files {
            return (files, true);
        }
    }
    (files, false)
}

/// Extensions of every language that has definition queries.
pub(crate) fn source_exts() -> Vec<&'static str> {
    [Lang::Rust, Lang::Ts, Lang::Php, Lang::Go, Lang::Python]
        .iter()
        .flat_map(|l| l.family_exts().iter().copied())
        .collect()
}

/// Every definition in `content` (`rel` picks the grammar and fills `path`).
/// Used for the file being edited (unsaved buffer) and by the project symbol index.
pub(crate) fn symbols_in_file(rel: &str, content: &str) -> Vec<SymbolHit> {
    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let Some(lang) = Lang::from_ext(&ext) else {
        return Vec::new();
    };
    definitions_matching(lang, content, &|_| true)
        .into_iter()
        .map(|(name, line, column, kind, snippet)| SymbolHit {
            name,
            path: rel.to_string(),
            line,
            column,
            kind,
            snippet,
        })
        .collect()
}

/// One symbol found by [`find_symbols`].
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct SymbolHit {
    pub name: String,
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub kind: String,
    pub snippet: String,
}

/// Project-wide symbol search (Find in Files dialog, "Symbols" tab):
/// every definition whose name contains `query` (case-insensitive) across
/// all supported languages. Ranking: exact name → prefix → substring, then
/// shorter names, then path.
pub fn find_symbols(root: &Path, query: &str, limit: usize, max_files: usize) -> Vec<SymbolHit> {
    let query = query.trim().trim_start_matches('$');
    if !is_valid_symbol(query) {
        return Vec::new();
    }
    let needle = query.to_lowercase();
    let (files, _) = source_files(root, &source_exts(), max_files);
    let mut out: Vec<SymbolHit> = Vec::new();
    for (path, rel) in &files {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        if !content.to_lowercase().contains(&needle) {
            continue;
        }
        let ext = Path::new(rel)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let Some(lang) = Lang::from_ext(&ext) else {
            continue;
        };
        let accept = |name: &str| name.to_lowercase().contains(&needle);
        for (name, line, column, kind, snippet) in definitions_matching(lang, &content, &accept) {
            out.push(SymbolHit {
                name,
                path: rel.clone(),
                line,
                column,
                kind,
                snippet,
            });
        }
        // Hard stop so a one-letter query cannot balloon the response.
        if out.len() >= limit * 20 {
            break;
        }
    }
    let tier = |name: &str| {
        let n = name.to_lowercase();
        if n == needle {
            0
        } else if n.starts_with(&needle) {
            1
        } else {
            2
        }
    };
    out.sort_by(|a, b| {
        tier(&a.name)
            .cmp(&tier(&b.name))
            .then(a.name.len().cmp(&b.name.len()))
            .then_with(|| a.path.cmp(&b.path))
            .then(a.line.cmp(&b.line))
    });
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Self-cleaning scratch project root.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("bebok-gotodef-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn hits(root: &Path, from: &str, sym: &str) -> Vec<(String, usize)> {
        find_definitions(root, Some(from), sym, 50, 10_000)
            .results
            .into_iter()
            .map(|h| (h.path, h.line))
            .collect()
    }

    #[test]
    fn every_language_query_compiles() {
        for lang in [
            Lang::Rust,
            Lang::Ts,
            Lang::Tsx,
            Lang::Js,
            Lang::Php,
            Lang::Go,
            Lang::Python,
        ] {
            assert!(query_for(lang).is_some(), "{lang:?}");
        }
    }

    #[test]
    fn rust_fn_struct_and_same_file_first() {
        let dir = TempDir::new();
        write(
            dir.path(),
            "src/a.rs",
            "fn helper() {}\nfn main() { helper(); }\n",
        );
        write(
            dir.path(),
            "src/b.rs",
            "pub fn helper() {}\npub struct Widget;\n",
        );
        let h = hits(dir.path(), "src/a.rs", "helper");
        assert_eq!(h[0], ("src/a.rs".into(), 1));
        assert!(h.contains(&("src/b.rs".into(), 1)));
        assert_eq!(
            hits(dir.path(), "src/a.rs", "Widget"),
            vec![("src/b.rs".into(), 2)]
        );
        // A call site is not a definition.
        assert!(hits(dir.path(), "src/a.rs", "main").len() == 1);
    }

    #[test]
    fn php_class_method_and_file_named_after_class() {
        let dir = TempDir::new();
        write(
            dir.path(),
            "src/Service/UserService.php",
            "<?php\nnamespace App;\nclass UserService {\n  public function find($id) {}\n}\n",
        );
        write(
            dir.path(),
            "src/Controller.php",
            "<?php\n$s = new UserService();\n$s->find(1);\n",
        );
        assert_eq!(
            hits(dir.path(), "src/Controller.php", "UserService"),
            vec![("src/Service/UserService.php".into(), 3)]
        );
        assert_eq!(
            hits(dir.path(), "src/Controller.php", "find"),
            vec![("src/Service/UserService.php".into(), 4)]
        );
    }

    #[test]
    fn ts_and_js_share_a_family() {
        let dir = TempDir::new();
        write(
            dir.path(),
            "lib/util.js",
            "export function slugify(s) { return s; }\n",
        );
        write(
            dir.path(),
            "app/main.ts",
            "import { slugify } from '../lib/util';\ninterface Opts { x: number }\nexport const LIMIT = 3;\n",
        );
        assert_eq!(
            hits(dir.path(), "app/main.ts", "slugify"),
            vec![("lib/util.js".into(), 1)]
        );
        assert_eq!(
            hits(dir.path(), "app/main.ts", "Opts"),
            vec![("app/main.ts".into(), 2)]
        );
        assert_eq!(
            hits(dir.path(), "app/main.ts", "LIMIT"),
            vec![("app/main.ts".into(), 3)]
        );
    }

    #[test]
    fn go_func_type_and_method() {
        let dir = TempDir::new();
        write(
            dir.path(),
            "pkg/store/store.go",
            "package store\n\ntype Store struct{ n int }\n\nfunc (s *Store) Get() int { return s.n }\n\nfunc New() *Store { return &Store{} }\n",
        );
        write(
            dir.path(),
            "cmd/main.go",
            "package main\nfunc main() { store.New().Get() }\n",
        );
        assert_eq!(
            hits(dir.path(), "cmd/main.go", "Store"),
            vec![("pkg/store/store.go".into(), 3)]
        );
        assert_eq!(
            hits(dir.path(), "cmd/main.go", "Get"),
            vec![("pkg/store/store.go".into(), 5)]
        );
        assert_eq!(
            hits(dir.path(), "cmd/main.go", "New"),
            vec![("pkg/store/store.go".into(), 7)]
        );
    }

    #[test]
    fn rejects_non_identifiers_and_strips_php_dollar() {
        let dir = TempDir::new();
        write(dir.path(), "a.php", "<?php\nclass A { public $total; }\n");
        assert!(hits(dir.path(), "a.php", "a b").is_empty());
        assert!(hits(dir.path(), "a.php", "").is_empty());
        assert_eq!(
            hits(dir.path(), "a.php", "$total"),
            vec![("a.php".into(), 2)]
        );
    }

    #[test]
    fn find_symbols_ranks_exact_then_prefix_then_substring() {
        let dir = TempDir::new();
        write(
            dir.path(),
            "src/a.rs",
            "fn user_load() {}\nstruct User;\nfn current_user() {}\n",
        );
        write(dir.path(), "web/b.ts", "export class UserService {}\n");
        let names: Vec<String> = find_symbols(dir.path(), "user", 10, 1000)
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert_eq!(names[0], "User");
        assert!(names.contains(&"UserService".to_string()));
        assert!(names.contains(&"current_user".to_string()));
        assert_eq!(names.last().map(String::as_str), Some("current_user"));
        assert!(find_symbols(dir.path(), "a b", 10, 1000).is_empty());
    }
}
