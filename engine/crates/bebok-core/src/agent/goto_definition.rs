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
    /// True when the file cap, time budget or `limit` cut the search short.
    pub truncated: bool,
    /// The caret sits on the declaration itself (the client offers usages).
    pub on_declaration: bool,
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

/// Child directories scanned per ancestor by [`resolve_hints_near`].
const NEAR_CHILD_CAP: usize = 200;

/// `dir/segs…` matched case-insensitively (`bss/return` → `Bss/Return`).
fn resolve_ci(dir: &Path, segs: &[&str]) -> Option<PathBuf> {
    let mut cur = dir.to_path_buf();
    for seg in segs {
        let exact = cur.join(seg);
        if exact.exists() {
            cur = exact;
            continue;
        }
        let found = std::fs::read_dir(&cur).ok()?.flatten().find(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(seg))
        })?;
        cur = found.path();
    }
    cur.is_file().then_some(cur)
}

/// Existing files matching the file `hints`, looked up in the ancestors of
/// `from` and their direct children (source roots such as `src/`, `lib/`),
/// nearest ancestor first; stops at the first ancestor with a match.
fn resolve_hints_near(root: &Path, from: &str, hints: &[String]) -> Vec<String> {
    let files: Vec<Vec<&str>> = hints
        .iter()
        .filter(|h| !h.ends_with('/'))
        .map(|h| h.split('/').filter(|s| !s.is_empty()).collect())
        .collect();
    if files.is_empty() {
        return Vec::new();
    }
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    let mut out: Vec<String> = Vec::new();
    loop {
        let dir = parts.iter().fold(root.to_path_buf(), |d, p| d.join(p));
        let mut bases = vec![dir.clone()];
        if let Ok(rd) = std::fs::read_dir(&dir) {
            bases.extend(
                rd.flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .filter(|e| {
                        !e.file_name()
                            .to_str()
                            .is_some_and(|n| n.starts_with('.') || n == "node_modules")
                    })
                    .take(NEAR_CHILD_CAP)
                    .map(|e| e.path()),
            );
        }
        for base in &bases {
            for segs in &files {
                if let Some(p) = resolve_ci(base, segs)
                    && let Ok(rel) = p.strip_prefix(root)
                {
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    if !out.contains(&rel) {
                        out.push(rel);
                    }
                }
            }
        }
        if !out.is_empty() || parts.pop().is_none() {
            return out;
        }
    }
}

/// Extra input for [`find_definitions_in`].
#[derive(Debug, Default, Clone, Copy)]
pub struct DefinitionContext<'a> {
    /// Unsaved buffer of the clicked file (`from`): locals, imports and
    /// same-file definitions are read from it instead of the disk copy.
    pub text: Option<&'a str>,
    /// UTF-8 byte offset of the caret in `text`.
    pub offset: Option<usize>,
    /// Files a full-text index ranked for the symbol (searched early).
    pub priority: &'a [String],
}

/// Most paths collected by the walk (the read budget is `max_files`).
const WALK_CAP: usize = 500_000;
/// Stop reading once something was found and this much time went by.
const SOFT_BUDGET: std::time::Duration = std::time::Duration::from_millis(1_500);
/// Hard stop for one lookup.
const HARD_BUDGET: std::time::Duration = std::time::Duration::from_secs(8);

/// Find the definitions of `symbol` (no buffer context).
pub fn find_definitions(
    root: &Path,
    from: Option<&str>,
    symbol: &str,
    limit: usize,
    max_files: usize,
) -> DefinitionResponse {
    find_definitions_in(
        root,
        from,
        symbol,
        DefinitionContext::default(),
        limit,
        max_files,
    )
}

/// Find the definitions of `symbol`.
///
/// `from` is the project-relative file the click happened in: it picks the
/// language family (all supported families when absent / unknown). With a
/// buffer + caret in `ctx`, a local binding in scope wins outright, the caret
/// on a declaration reports `on_declaration`, and a bare identifier defined
/// in the same file resolves there. Otherwise files are ranked: import /
/// class-name path hints → a file named after the symbol → index hits →
/// nearest directory → path order. `max_files` bounds the files *read*.
pub fn find_definitions_in(
    root: &Path,
    from: Option<&str>,
    symbol: &str,
    ctx: DefinitionContext<'_>,
    limit: usize,
    max_files: usize,
) -> DefinitionResponse {
    let started = std::time::Instant::now();
    // PHP `$var` / `$this->x`: definitions carry the bare name.
    let symbol = symbol.trim_start_matches('$');
    let mut resp = DefinitionResponse {
        symbol: symbol.to_string(),
        results: Vec::new(),
        parsed_files: 0,
        truncated: false,
        on_declaration: false,
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
        None => source_exts(),
    };

    // 1. The buffer: locals, declaration under the caret, imports, same file.
    let mut lookup = symbol.to_string();
    let mut hints: Vec<String> = Vec::new();
    let mut buffer_done = false;
    let mut self_member = false;
    if let (Some(text), Some(lang), Some(rel)) = (ctx.text, from_lang, from.as_deref()) {
        let mut offset = ctx.offset.unwrap_or(0).min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        let info = super::goto_local::analyse(lang, rel, text, offset, symbol);
        if info.on_declaration {
            resp.on_declaration = true;
            return resp;
        }
        if let Some(local) = info.local {
            resp.results.push(DefinitionHit {
                path: rel.to_string(),
                line: local.line,
                column: local.column,
                kind: local.kind,
                snippet: local.snippet,
            });
            return resp;
        }
        if let Some(l) = info.lookup {
            lookup = l;
        }
        hints = info.path_hints;
        let (caret_line, caret_col) = super::goto_local::line_col(text, offset);
        let same = definitions_in(lang, text, &lookup);
        if same.iter().any(|(l, c, _, _)| {
            *l == caret_line && caret_col >= *c && caret_col <= *c + lookup.len()
        }) {
            resp.on_declaration = true;
            return resp;
        }
        for (line, column, kind, snippet) in same {
            resp.results.push(DefinitionHit {
                path: rel.to_string(),
                line,
                column,
                kind,
                snippet,
            });
        }
        // `foo()` / `Foo` defined right here: no need to look further
        // (member access `x.foo` may still mean another class's `foo`).
        if !resp.results.is_empty() && (!info.member || info.self_member) {
            return resp;
        }
        self_member = info.self_member;
        buffer_done = true;
    } else if from_lang == Some(Lang::Php) || from_lang.is_none() {
        hints = super::goto_local::analyse(Lang::Php, "", "", 0, symbol).path_hints;
    }
    let lookup = lookup.as_str();

    // 2. Fast path: resolve file hints next to the clicked file (no walk).
    if let Some(from) = from.as_deref() {
        let near = resolve_hints_near(root, from, &hints);
        for rel in &near {
            if buffer_done && rel == from {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(root.join(rel)) else {
                continue;
            };
            let Some(lang) = Path::new(rel)
                .extension()
                .and_then(|e| e.to_str())
                .and_then(|e| Lang::from_ext(&e.to_ascii_lowercase()))
            else {
                continue;
            };
            resp.parsed_files += 1;
            for (line, column, kind, snippet) in definitions_in(lang, &content, lookup) {
                resp.results.push(DefinitionHit {
                    path: rel.clone(),
                    line,
                    column,
                    kind,
                    snippet,
                });
            }
        }
        if !resp.results.is_empty() {
            return resp;
        }
        // `$this->foo()` not defined here: walk up the `extends` chain.
        if self_member
            && from_lang == Some(Lang::Php)
            && let Some(text) = ctx.text
        {
            let mut text = text.to_string();
            let mut at = from.to_string();
            for _ in 0..12 {
                let Some((_, parent_hints)) = super::goto_local::php_parent_hints(&text) else {
                    break;
                };
                let Some(parent) = resolve_hints_near(root, &at, &parent_hints)
                    .into_iter()
                    .next()
                else {
                    break;
                };
                let Ok(content) = std::fs::read_to_string(root.join(&parent)) else {
                    break;
                };
                resp.parsed_files += 1;
                for (line, column, kind, snippet) in definitions_in(Lang::Php, &content, lookup) {
                    resp.results.push(DefinitionHit {
                        path: parent.clone(),
                        line,
                        column,
                        kind,
                        snippet,
                    });
                }
                if !resp.results.is_empty() {
                    return resp;
                }
                text = content;
                at = parent;
            }
        }
    }

    // 3. The project.
    let (mut files, walk_truncated) = source_files(root, &exts, WALK_CAP);
    resp.truncated = walk_truncated;
    let priority: std::collections::HashSet<&str> =
        ctx.priority.iter().map(String::as_str).collect();
    let lower = lookup.to_ascii_lowercase();
    let rank = |rel: &str| -> (u8, std::cmp::Reverse<usize>) {
        if from.as_deref() == Some(rel) {
            return (0, std::cmp::Reverse(usize::MAX));
        }
        let near = from.as_deref().map(|f| shared_dirs(f, rel)).unwrap_or(0);
        if hints
            .iter()
            .any(|h| super::goto_local::hint_matches(rel, h))
        {
            return (1, std::cmp::Reverse(near));
        }
        let stem = Path::new(rel)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let tier = if stem == lower || stem.replace(['-', '_', '.'], "") == lower.replace('_', "") {
            2
        } else if priority.contains(rel) {
            3
        } else {
            4
        };
        (tier, std::cmp::Reverse(near))
    };
    let mut ranked: Vec<((u8, std::cmp::Reverse<usize>), PathBuf, String)> =
        files.drain(..).map(|(p, r)| (rank(&r), p, r)).collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));

    let mut read = 0usize;
    let mut best_tier: Option<u8> = None;
    'files: for ((tier, _), path, rel) in &ranked {
        if buffer_done && from.as_deref() == Some(rel.as_str()) {
            continue;
        }
        // A hit in a hinted file (import / class path) is the answer.
        if best_tier == Some(1) && *tier > 1 {
            break;
        }
        let elapsed = started.elapsed();
        if read >= max_files
            || elapsed > HARD_BUDGET
            || (!resp.results.is_empty() && elapsed > SOFT_BUDGET)
        {
            resp.truncated = true;
            break;
        }
        read += 1;
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        if !content.contains(lookup) {
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
        for (line, column, kind, snippet) in definitions_in(lang, &content, lookup) {
            best_tier = Some(best_tier.map_or(*tier, |b| b.min(*tier)));
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
    // The hinted class exists in several copies (deploy snapshots, backups):
    // keep the copy closest to the clicked file instead of a picker.
    if best_tier == Some(1)
        && let Some(from) = from.as_deref()
    {
        let hinted = |rel: &str| {
            hints
                .iter()
                .any(|h| super::goto_local::hint_matches(rel, h))
        };
        let best = resp
            .results
            .iter()
            .filter(|h| hinted(&h.path))
            .map(|h| shared_dirs(from, &h.path))
            .max()
            .unwrap_or(0);
        resp.results
            .retain(|h| hinted(&h.path) && shared_dirs(from, &h.path) == best);
    }
    // Nothing defines it, but an import pointed at an existing module file
    // (`import * as m`, default export under another name): open that file.
    if resp.results.is_empty()
        && let Some((_, _, rel)) = ranked.iter().find(|(t, _, r)| {
            t.0 == 1
                && hints
                    .iter()
                    .any(|h| !h.ends_with('/') && r.to_ascii_lowercase() == *h)
        })
    {
        resp.results.push(DefinitionHit {
            path: rel.clone(),
            line: 1,
            column: 1,
            kind: "module".to_string(),
            snippet: String::new(),
        });
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

    /// Two rules that hold for every language, pinned on Rust: the clicked
    /// file is searched first, and a call site is not a definition.
    #[test]
    fn same_file_wins_and_call_sites_are_not_definitions() {
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
        assert_eq!(hits(dir.path(), "src/a.rs", "main").len(), 1);
    }

    /// One mixed-language project instead of one project per language: each
    /// language's query must find its own definition kinds (PHP class +
    /// method, TS interface + const across the TS/JS family, Go type + func
    /// + method). `every_language_query_compiles` covers the rest.
    #[test]
    fn finds_definitions_in_every_language() {
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
        for (from, symbol, want) in [
            // PHP: the class and one of its methods.
            (
                "src/Controller.php",
                "UserService",
                ("src/Service/UserService.php", 3),
            ),
            (
                "src/Controller.php",
                "find",
                ("src/Service/UserService.php", 4),
            ),
            // TS/JS are one family: a `.ts` file resolves into a `.js` one.
            ("app/main.ts", "slugify", ("lib/util.js", 1)),
            ("app/main.ts", "Opts", ("app/main.ts", 2)),
            ("app/main.ts", "LIMIT", ("app/main.ts", 3)),
            // Go: type, method, constructor.
            ("cmd/main.go", "Store", ("pkg/store/store.go", 3)),
            ("cmd/main.go", "Get", ("pkg/store/store.go", 5)),
            ("cmd/main.go", "New", ("pkg/store/store.go", 7)),
        ] {
            assert_eq!(
                hits(dir.path(), from, symbol),
                vec![(want.0.to_string(), want.1)],
                "{from} -> {symbol}"
            );
        }
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
