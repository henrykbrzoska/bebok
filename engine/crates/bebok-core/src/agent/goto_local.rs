//! Buffer-side half of go-to-definition: what the caret sits on in the
//! (unsaved) file being edited.
//!
//! - local bindings (`const`/`let`/`var` incl. destructuring, parameters,
//!   `catch`, loop variables, PHP `$vars`, Rust `let`, Go `:=`, Python
//!   assignments) resolved by scope: innermost scope that contains the caret,
//!   then the nearest preceding binding (PHP: the first assignment);
//! - the caret on a declaration's own name (the client shows usages then);
//! - imports of the clicked name, turned into path hints for the project
//!   search (TS/JS relative modules, PHP `use`, Python `from … import`,
//!   Rust `use`, Go package selectors) plus Magento-style `Vendor_Module_Class`.
//!
//! Purely syntactic, like the rest of `goto_definition`.

use std::sync::OnceLock;

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator, Tree};

use super::goto_definition::Lang;

/// One local binding site in the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalHit {
    pub line: usize,
    pub column: usize,
    pub kind: String,
    pub snippet: String,
}

/// What the buffer says about the clicked identifier.
#[derive(Debug, Default)]
pub(crate) struct BufferInfo {
    /// The resolved local binding (shadows every project definition).
    pub local: Option<LocalHit>,
    /// The caret is on the name of a declaration (local or top-level).
    pub on_declaration: bool,
    /// `obj.x`, `$this->x`, `Foo::x`, `pkg.X`: never a local, and the
    /// same-file definition must not hide the other candidates.
    pub member: bool,
    /// Member of the enclosing class (`this.x`, `$this->x`, `self::X`,
    /// `self.x`): a same-file definition is the answer.
    pub self_member: bool,
    /// Name to look up in the project (an import alias maps back to the
    /// exported name).
    pub lookup: Option<String>,
    /// Lower-case project-relative path suffixes the definition most likely
    /// lives in (`a/b.ts`, `mage/catalog/model/product.php`); a trailing `/`
    /// marks a directory.
    pub path_hints: Vec<String>,
}

/// Analyse `text` at the UTF-8 byte `offset` for `symbol` (no `$`).
pub(crate) fn analyse(
    lang: Lang,
    from_rel: &str,
    text: &str,
    offset: usize,
    symbol: &str,
) -> BufferInfo {
    let mut info = BufferInfo::default();
    if lang == Lang::Php {
        info.path_hints.extend(magento_hints(symbol));
    }
    let mut parser = Parser::new();
    if parser.set_language(&lang.language()).is_err() {
        return info;
    }
    let Some(tree) = parser.parse(text, None) else {
        return info;
    };
    let src = text.as_bytes();
    let offset = offset.min(text.len());

    let leaf = leaf_at(&tree, src, offset, symbol);
    let mut php_var = false;
    if let Some(leaf) = leaf {
        let (member, operand) = member_access(leaf, src);
        info.member = member;
        info.self_member = member
            && operand
                .as_deref()
                .is_some_and(|o| matches!(o, "this" | "$this" | "self" | "static" | "Self"));
        php_var = lang == Lang::Php && leaf.parent().is_some_and(|p| p.kind() == "variable_name");
        if lang == Lang::Php
            && let Some(q) = leaf.parent().filter(|p| p.kind() == "qualified_name")
            && let Ok(qn) = q.utf8_text(src)
        {
            info.path_hints.extend(namespace_hints(qn));
        }
        if lang == Lang::Go
            && let Some(op) = operand
        {
            info.path_hints
                .extend(go_package_hints(tree.root_node(), src, &op));
        }
    }

    // Locals: only for a plain identifier reference (PHP: only `$vars`).
    let locals_apply = leaf.is_some() && !info.member && (lang != Lang::Php || php_var);
    if locals_apply {
        let bindings = local_bindings(lang, &tree, src, symbol);
        if let Some(here) = bindings
            .iter()
            .find(|b| b.name_start <= offset && offset <= b.name_end)
        {
            // PHP has no declarations: a later `$x = …` jumps to the first one.
            let first = bindings
                .iter()
                .filter(|b| lang == Lang::Php && b.scope == here.scope)
                .min_by_key(|b| b.name_start);
            match first {
                Some(f) if f.name_start != here.name_start => {
                    info.local = Some(LocalHit {
                        line: f.line,
                        column: f.column,
                        kind: f.kind.clone(),
                        snippet: snippet_at(text, f.line),
                    });
                }
                _ => info.on_declaration = true,
            }
            return info;
        }
        if let Some(b) = pick_binding(lang, &bindings, offset) {
            info.local = Some(LocalHit {
                line: b.line,
                column: b.column,
                kind: b.kind.clone(),
                snippet: snippet_at(text, b.line),
            });
            return info;
        }
    }

    if !info.member
        && let Some((lookup, hints)) = import_hint(lang, tree.root_node(), src, from_rel, symbol)
    {
        info.path_hints.extend(hints);
        if lookup != symbol {
            info.lookup = Some(lookup);
        }
    }
    info.path_hints.sort();
    info.path_hints.dedup();
    info
}

/// Trimmed source line `line` (1-based), max 200 chars.
pub(crate) fn snippet_at(text: &str, line: usize) -> String {
    text.lines()
        .nth(line.saturating_sub(1))
        .unwrap_or("")
        .trim()
        .chars()
        .take(200)
        .collect()
}

/// 1-based (line, byte column) of `offset`.
pub(crate) fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let head = &text.as_bytes()[..offset.min(text.len())];
    let line = head.iter().filter(|b| **b == b'\n').count() + 1;
    let line_start = head.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    (line, offset.min(text.len()) - line_start + 1)
}

/// Does `hint` (see [`BufferInfo::path_hints`]) match the project path `rel`?
pub(crate) fn hint_matches(rel: &str, hint: &str) -> bool {
    let rel = rel.to_ascii_lowercase();
    if let Some(dir) = hint.strip_suffix('/') {
        let parent = rel.rsplit_once('/').map_or("", |(p, _)| p);
        parent == dir || parent.ends_with(&format!("/{dir}"))
    } else {
        rel == hint || rel.ends_with(&format!("/{hint}"))
    }
}

// --- leaf / member access ------------------------------------------------------

/// The named node spelling `symbol` at (or right before) `offset`.
fn leaf_at<'t>(tree: &'t Tree, src: &[u8], offset: usize, symbol: &str) -> Option<Node<'t>> {
    let root = tree.root_node();
    for at in [offset, offset.saturating_sub(1)] {
        let Some(mut node) = root.named_descendant_for_byte_range(at, at) else {
            continue;
        };
        // Caret on PHP's `$`: use the `name` inside `variable_name`.
        if node.kind() == "variable_name"
            && let Some(name) = node.named_child(0)
        {
            node = name;
        }
        if node.utf8_text(src).ok().map(|t| t.trim_start_matches('$')) == Some(symbol) {
            return Some(node);
        }
    }
    None
}

/// Parent kinds where a non-leading child is a member name.
const MEMBER_PARENTS: &[&str] = &[
    "member_expression",
    "attribute",
    "field_expression",
    "selector_expression",
    "scoped_identifier",
    "scoped_type_identifier",
    "member_access_expression",
    "nullsafe_member_access_expression",
    "member_call_expression",
    "nullsafe_member_call_expression",
    "scoped_property_access_expression",
    "scoped_call_expression",
    "class_constant_access_expression",
    "qualified_name",
];

/// `(is member access, operand text)` for the clicked leaf.
fn member_access(leaf: Node<'_>, src: &[u8]) -> (bool, Option<String>) {
    // PHP `$x` → `name` inside `variable_name`: judge the variable node.
    let mut node = leaf;
    if let Some(p) = node.parent()
        && p.kind() == "variable_name"
    {
        node = p;
    }
    let Some(parent) = node.parent() else {
        return (false, None);
    };
    if !MEMBER_PARENTS.contains(&parent.kind()) || node.start_byte() <= parent.start_byte() {
        return (false, None);
    }
    let operand = parent
        .named_child(0)
        .filter(|c| *c != node)
        .and_then(|c| c.utf8_text(src).ok())
        .map(str::to_string);
    (true, operand)
}

// --- local bindings ------------------------------------------------------------

struct Binding {
    name_start: usize,
    name_end: usize,
    line: usize,
    column: usize,
    kind: String,
    scope: (usize, usize),
}

fn local_patterns(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Ts | Lang::Tsx | Lang::Js => &[
            "(variable_declarator name: (identifier) @name) @def",
            "(object_pattern (shorthand_property_identifier_pattern) @name) @def",
            "(object_pattern (pair_pattern value: (identifier) @name)) @def",
            "(object_pattern (object_assignment_pattern left: (shorthand_property_identifier_pattern) @name)) @def",
            "(array_pattern (identifier) @name) @def",
            "(array_pattern (assignment_pattern left: (identifier) @name)) @def",
            "(rest_pattern (identifier) @name) @def",
            "(required_parameter pattern: (identifier) @name) @def",
            "(optional_parameter pattern: (identifier) @name) @def",
            "(formal_parameters (identifier) @name) @def",
            "(formal_parameters (assignment_pattern left: (identifier) @name)) @def",
            "(arrow_function parameter: (identifier) @name) @def",
            "(catch_clause parameter: (identifier) @name) @def",
            "(for_in_statement left: (identifier) @name) @def",
        ],
        Lang::Php => &[
            "(assignment_expression left: (variable_name (name) @name)) @def",
            "(simple_parameter name: (variable_name (name) @name)) @def",
            "(variadic_parameter name: (variable_name (name) @name)) @def",
            "(property_promotion_parameter name: (variable_name (name) @name)) @def",
            "(foreach_statement (variable_name (name) @name)) @def",
            "(foreach_statement (pair (variable_name (name) @name))) @def",
            "(foreach_statement (by_ref (variable_name (name) @name))) @def",
            "(foreach_statement (pair (by_ref (variable_name (name) @name)))) @def",
            "(catch_clause name: (variable_name (name) @name)) @def",
            "(global_declaration (variable_name (name) @name)) @def",
            "(static_variable_declaration name: (variable_name (name) @name)) @def",
            "(anonymous_function_use_clause (variable_name (name) @name)) @def",
            "(list_literal (variable_name (name) @name)) @def",
        ],
        Lang::Rust => &[
            "(let_declaration pattern: (identifier) @name) @def",
            "(parameter pattern: (identifier) @name) @def",
            "(closure_parameters (identifier) @name) @def",
            "(for_expression pattern: (identifier) @name) @def",
            "(tuple_pattern (identifier) @name) @def",
            "(tuple_struct_pattern (identifier) @name) @def",
            "(reference_pattern (identifier) @name) @def",
            "(mut_pattern (identifier) @name) @def",
            "(captured_pattern (identifier) @name) @def",
            "(field_pattern (shorthand_field_identifier) @name) @def",
            "(field_pattern pattern: (identifier) @name) @def",
            "(slice_pattern (identifier) @name) @def",
        ],
        Lang::Go => &[
            "(short_var_declaration left: (expression_list (identifier) @name)) @def",
            "(var_spec name: (identifier) @name) @def",
            "(const_spec name: (identifier) @name) @def",
            "(parameter_declaration name: (identifier) @name) @def",
            "(variadic_parameter_declaration name: (identifier) @name) @def",
            "(range_clause left: (expression_list (identifier) @name)) @def",
            "(type_switch_statement alias: (expression_list (identifier) @name)) @def",
        ],
        Lang::Python => &[
            "(assignment left: (identifier) @name) @def",
            "(assignment left: (pattern_list (identifier) @name)) @def",
            "(assignment left: (tuple_pattern (identifier) @name)) @def",
            "(parameters (identifier) @name) @def",
            "(lambda_parameters (identifier) @name) @def",
            "(default_parameter name: (identifier) @name) @def",
            "(typed_parameter (identifier) @name) @def",
            "(typed_default_parameter name: (identifier) @name) @def",
            "(list_splat_pattern (identifier) @name) @def",
            "(dictionary_splat_pattern (identifier) @name) @def",
            "(for_statement left: (identifier) @name) @def",
            "(for_statement left: (pattern_list (identifier) @name)) @def",
            "(for_in_clause left: (identifier) @name) @def",
            "(as_pattern alias: (as_pattern_target (identifier) @name)) @def",
            "(named_expression name: (identifier) @name) @def",
        ],
    }
}

/// Node kinds that open a scope for local bindings.
fn scope_kinds(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Ts | Lang::Tsx | Lang::Js => &[
            "program",
            "statement_block",
            "function_declaration",
            "function_expression",
            "function",
            "generator_function_declaration",
            "generator_function",
            "arrow_function",
            "method_definition",
            "for_statement",
            "for_in_statement",
            "catch_clause",
            "class_body",
        ],
        Lang::Php => &[
            "program",
            "function_definition",
            "method_declaration",
            "anonymous_function",
            "anonymous_function_creation_expression",
            "arrow_function",
        ],
        Lang::Rust => &[
            "source_file",
            "block",
            "function_item",
            "closure_expression",
            "for_expression",
            "match_arm",
            "if_expression",
            "while_expression",
        ],
        Lang::Go => &[
            "source_file",
            "block",
            "function_declaration",
            "method_declaration",
            "func_literal",
            "for_statement",
            "if_statement",
            "expression_switch_statement",
            "type_switch_statement",
            "expression_case",
            "type_case",
            "communication_case",
        ],
        Lang::Python => &[
            "module",
            "function_definition",
            "lambda",
            "class_definition",
            "list_comprehension",
            "dictionary_comprehension",
            "set_comprehension",
            "generator_expression",
        ],
    }
}

struct LocalQuery {
    query: Query,
    name_idx: u32,
    def_idx: u32,
}

fn local_query(lang: Lang) -> Option<&'static LocalQuery> {
    static CELLS: [OnceLock<Option<LocalQuery>>; 7] = [const { OnceLock::new() }; 7];
    let idx = match lang {
        Lang::Rust => 0,
        Lang::Ts => 1,
        Lang::Tsx => 2,
        Lang::Js => 3,
        Lang::Php => 4,
        Lang::Go => 5,
        Lang::Python => 6,
    };
    CELLS[idx]
        .get_or_init(|| {
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
            let query = Query::new(&language, &source).ok()?;
            Some(LocalQuery {
                name_idx: query.capture_index_for_name("name")?,
                def_idx: query.capture_index_for_name("def")?,
                query,
            })
        })
        .as_ref()
}

fn local_bindings(lang: Lang, tree: &Tree, src: &[u8], symbol: &str) -> Vec<Binding> {
    let Some(lq) = local_query(lang) else {
        return Vec::new();
    };
    let scopes = scope_kinds(lang);
    let root = tree.root_node();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&lq.query, root, src);
    let mut out: Vec<Binding> = Vec::new();
    while let Some(m) = matches.next() {
        let name = m
            .captures
            .iter()
            .find(|c| c.index == lq.name_idx)
            .map(|c| c.node);
        let def = m
            .captures
            .iter()
            .find(|c| c.index == lq.def_idx)
            .map(|c| c.node);
        let (Some(name), Some(def)) = (name, def) else {
            continue;
        };
        if name.utf8_text(src).ok() != Some(symbol) {
            continue;
        }
        if out.iter().any(|b| b.name_start == name.start_byte()) {
            continue;
        }
        let Some(parent) = name.parent() else {
            continue;
        };
        // `Some(x)`: `Some` is the pattern's type, not a binding.
        if parent.child_by_field_name("type") == Some(name) {
            continue;
        }
        // PHP `foreach ($items as $item)`: `$items` is the subject.
        if def.kind() == "foreach_statement"
            && parent.parent() == Some(def)
            && parent.prev_sibling().is_none_or(|s| s.kind() != "as")
        {
            continue;
        }
        if lang == Lang::Php && symbol == "this" {
            continue;
        }
        // Scope: nearest scope-opening ancestor of the name.
        let mut cur = Some(parent);
        let mut scope = root;
        while let Some(n) = cur {
            if scopes.contains(&n.kind()) {
                scope = n;
                break;
            }
            cur = n.parent();
        }
        let pos = name.start_position();
        out.push(Binding {
            name_start: name.start_byte(),
            name_end: name.end_byte(),
            line: pos.row + 1,
            column: pos.column + 1,
            kind: def.kind().to_string(),
            scope: (scope.start_byte(), scope.end_byte()),
        });
    }
    out
}

/// Innermost scope containing the caret, then the nearest preceding binding
/// (PHP: the first assignment). A binding later in the scope (hoisting) is
/// used only when nothing precedes the caret.
fn pick_binding(lang: Lang, bindings: &[Binding], offset: usize) -> Option<&Binding> {
    let visible: Vec<&Binding> = bindings
        .iter()
        .filter(|b| b.scope.0 <= offset && offset <= b.scope.1)
        .collect();
    let span = |b: &Binding| b.scope.1 - b.scope.0;
    let preceding: Vec<&Binding> = visible
        .iter()
        .copied()
        .filter(|b| b.name_start <= offset)
        .collect();
    if !preceding.is_empty() {
        let inner = preceding.iter().map(|b| span(b)).min()?;
        let in_scope = preceding.into_iter().filter(|b| span(b) == inner);
        return if lang == Lang::Php {
            in_scope.min_by_key(|b| b.name_start)
        } else {
            in_scope.max_by_key(|b| b.name_start)
        };
    }
    let inner = visible.iter().map(|b| span(b)).min()?;
    visible
        .into_iter()
        .filter(|b| span(b) == inner)
        .min_by_key(|b| b.name_start)
}

// --- imports -> path hints ----------------------------------------------------

/// `(name to look up, path hints)` when `symbol` is imported in the buffer.
fn import_hint(
    lang: Lang,
    root: Node<'_>,
    src: &[u8],
    from_rel: &str,
    symbol: &str,
) -> Option<(String, Vec<String>)> {
    match lang {
        Lang::Ts | Lang::Tsx | Lang::Js => ts_import(root, src, from_rel, symbol),
        Lang::Php => php_use(root, src, symbol),
        Lang::Python => py_import(root, src, from_rel, symbol),
        Lang::Rust => rust_use(root, src, symbol),
        Lang::Go => None,
    }
}

fn text<'a>(node: Node<'_>, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

/// Every named descendant of `node` (bounded walk).
fn descendants(node: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if out.len() > 4_000 {
            break;
        }
        let mut cursor = n.walk();
        for c in n.named_children(&mut cursor) {
            out.push(c);
            stack.push(c);
        }
    }
    out
}

fn ts_import(
    root: Node<'_>,
    src: &[u8],
    from_rel: &str,
    symbol: &str,
) -> Option<(String, Vec<String>)> {
    let mut cursor = root.walk();
    for stmt in root.named_children(&mut cursor) {
        if stmt.kind() != "import_statement" {
            continue;
        }
        let Some(source) = stmt.child_by_field_name("source") else {
            continue;
        };
        let module = text(source, src).trim_matches(|c| c == '"' || c == '\'' || c == '`');
        let mut found: Option<String> = None;
        for n in descendants(stmt) {
            match n.kind() {
                "import_specifier" => {
                    let name = n
                        .child_by_field_name("name")
                        .map(|x| text(x, src))
                        .unwrap_or("");
                    let local = n
                        .child_by_field_name("alias")
                        .map(|x| text(x, src))
                        .unwrap_or(name);
                    if local == symbol && !name.is_empty() {
                        found = Some(name.trim_matches(|c| c == '"' || c == '\'').to_string());
                    }
                }
                "identifier" => {
                    let p = n.parent().map(|p| p.kind());
                    if matches!(p, Some("import_clause" | "namespace_import"))
                        && text(n, src) == symbol
                    {
                        found = Some(symbol.to_string());
                    }
                }
                _ => {}
            }
            if found.is_some() {
                break;
            }
        }
        if let Some(name) = found {
            return Some((name, ts_module_hints(from_rel, module)));
        }
    }
    None
}

/// Candidate files for a TS/JS module specifier (relative ones only).
fn ts_module_hints(from_rel: &str, module: &str) -> Vec<String> {
    if !module.starts_with('.') {
        // Path alias / package: the last two segments still narrow it down.
        let segs: Vec<&str> = module
            .split('/')
            .filter(|s| !s.is_empty() && !s.starts_with('@'))
            .collect();
        if segs.len() >= 2 {
            let tail = segs[segs.len() - 2..].join("/").to_ascii_lowercase();
            return ["ts", "tsx", "js", "jsx"]
                .iter()
                .map(|e| format!("{tail}.{e}"))
                .collect();
        }
        return Vec::new();
    }
    let mut parts: Vec<&str> = from_rel.split('/').collect();
    parts.pop();
    for seg in module.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let base = parts.join("/").to_ascii_lowercase();
    let stem = ["js", "mjs", "cjs", "jsx"]
        .iter()
        .find_map(|e| base.strip_suffix(&format!(".{e}")))
        .unwrap_or(&base)
        .to_string();
    let mut out = vec![base.clone()];
    for e in ["ts", "tsx", "mts", "js", "jsx", "mjs", "cjs", "d.ts"] {
        out.push(format!("{stem}.{e}"));
        out.push(format!("{stem}/index.{e}"));
    }
    out
}

/// `use App\Foo\Bar;` / `use App\Foo\Bar as Baz;`.
fn php_use(root: Node<'_>, src: &[u8], symbol: &str) -> Option<(String, Vec<String>)> {
    for n in descendants(root) {
        if n.kind() != "namespace_use_clause" {
            continue;
        }
        let clause = text(n, src).trim();
        let (qualified, alias) = match clause
            .split_once(" as ")
            .or_else(|| clause.split_once(" AS "))
        {
            Some((q, a)) => (q.trim(), Some(a.trim())),
            None => (clause, None),
        };
        let qualified = qualified.trim_start_matches('\\');
        let last = qualified.rsplit('\\').next().unwrap_or(qualified);
        if alias.unwrap_or(last) == symbol {
            return Some((last.to_string(), namespace_hints(qualified)));
        }
    }
    None
}

/// `App\Foo\Bar` → `app/foo/bar.php`, `foo/bar.php` (PSR-4 roots drop a prefix).
fn namespace_hints(qualified: &str) -> Vec<String> {
    let segs: Vec<String> = qualified
        .trim_start_matches('\\')
        .split('\\')
        .filter(|s| !s.is_empty() && *s != "namespace")
        .map(str::to_ascii_lowercase)
        .collect();
    (0..segs.len().saturating_sub(1))
        .map(|i| format!("{}.php", segs[i..].join("/")))
        .collect()
}

/// Magento 1 / PEAR class names: `Mage_Catalog_Model_Product` →
/// `mage/catalog/model/product.php`; controllers live in `controllers/`.
fn magento_hints(symbol: &str) -> Vec<String> {
    if !symbol.contains('_') || !symbol.starts_with(|c: char| c.is_ascii_uppercase()) {
        return Vec::new();
    }
    let segs: Vec<String> = symbol
        .split('_')
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if segs.len() < 2 {
        return Vec::new();
    }
    let mut out = vec![format!("{}.php", segs.join("/"))];
    if segs.len() >= 3 && symbol.ends_with("Controller") {
        out.push(format!(
            "{}/{}/controllers/{}.php",
            segs[0],
            segs[1],
            segs[2..].join("/")
        ));
    }
    out
}

/// `from pkg.mod import x` / `from .mod import x as y` / `import pkg.mod as m`.
fn py_import(
    root: Node<'_>,
    src: &[u8],
    from_rel: &str,
    symbol: &str,
) -> Option<(String, Vec<String>)> {
    let mut cursor = root.walk();
    for stmt in root.named_children(&mut cursor) {
        if stmt.kind() != "import_from_statement" {
            continue;
        }
        let Some(module) = stmt.child_by_field_name("module_name") else {
            continue;
        };
        let module = text(module, src);
        let mut c2 = stmt.walk();
        for name in stmt.children_by_field_name("name", &mut c2) {
            let (orig, local) = if name.kind() == "aliased_import" {
                let o = name
                    .child_by_field_name("name")
                    .map(|x| text(x, src))
                    .unwrap_or("");
                let a = name
                    .child_by_field_name("alias")
                    .map(|x| text(x, src))
                    .unwrap_or(o);
                (o, a)
            } else {
                (text(name, src), text(name, src))
            };
            if local == symbol {
                let orig = orig.rsplit('.').next().unwrap_or(orig).to_string();
                return Some((orig, py_module_hints(from_rel, module)));
            }
        }
    }
    None
}

fn py_module_hints(from_rel: &str, module: &str) -> Vec<String> {
    let dots = module.chars().take_while(|c| *c == '.').count();
    let rest = module[dots..].replace('.', "/").to_ascii_lowercase();
    let base = if dots > 0 {
        let mut parts: Vec<&str> = from_rel.split('/').collect();
        parts.pop();
        for _ in 1..dots {
            parts.pop();
        }
        let dir = parts.join("/").to_ascii_lowercase();
        match (dir.is_empty(), rest.is_empty()) {
            (true, _) => rest,
            (false, true) => dir,
            (false, false) => format!("{dir}/{rest}"),
        }
    } else {
        rest
    };
    if base.is_empty() {
        return Vec::new();
    }
    vec![format!("{base}.py"), format!("{base}/__init__.py")]
}

/// `use crate::a::b::Foo;`, `use a::b::{Foo, Bar as Baz};`.
fn rust_use(root: Node<'_>, src: &[u8], symbol: &str) -> Option<(String, Vec<String>)> {
    for n in descendants(root) {
        if n.kind() != "use_declaration" {
            continue;
        }
        let raw = text(n, src).trim();
        let raw = match raw.strip_prefix("pub") {
            Some(r) => {
                let r = r.trim_start();
                if r.starts_with('(') {
                    r.split_once(')').map_or(r, |(_, x)| x)
                } else {
                    r
                }
            }
            None => raw,
        };
        let Some(raw) = raw.trim_start().strip_prefix("use") else {
            continue;
        };
        // `Foo as Bar` → `Foo@Bar` before the whitespace goes.
        let body: String = raw
            .replace(" as ", "@")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let body = body.trim_end_matches(';');
        let (prefix, items): (&str, Vec<&str>) = match body.split_once("::{") {
            Some((p, rest)) => (p, rest.trim_end_matches('}').split(',').collect()),
            None => match body.rsplit_once("::") {
                Some((p, item)) => (p, vec![item]),
                None => continue,
            },
        };
        for item in items {
            let (orig, local) = item.split_once('@').unwrap_or((item, item));
            if local != symbol {
                continue;
            }
            let segs: Vec<String> = prefix
                .split("::")
                .filter(|s| !matches!(*s, "" | "crate" | "self" | "super"))
                .map(str::to_ascii_lowercase)
                .collect();
            let mut hints = Vec::new();
            if !segs.is_empty() {
                let p = segs.join("/");
                hints.push(format!("{p}.rs"));
                hints.push(format!("{p}/mod.rs"));
                hints.push(format!("{p}/lib.rs"));
            }
            return Some((orig.to_string(), hints));
        }
    }
    None
}

/// `pkg.Func`: the directory of the imported package `pkg`.
fn go_package_hints(root: Node<'_>, src: &[u8], operand: &str) -> Vec<String> {
    for n in descendants(root) {
        if n.kind() != "import_spec" {
            continue;
        }
        let Some(path) = n.child_by_field_name("path") else {
            continue;
        };
        let path = text(path, src).trim_matches('"');
        let alias = n.child_by_field_name("name").map(|x| text(x, src));
        let last = path.rsplit('/').next().unwrap_or(path);
        if alias.unwrap_or(last) == operand {
            let segs: Vec<&str> = path.split('/').collect();
            let tail = segs[segs.len().saturating_sub(2)..]
                .join("/")
                .to_ascii_lowercase();
            return vec![
                format!("{tail}/"),
                format!("{}/", last.to_ascii_lowercase()),
            ];
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, marker: &str) -> usize {
        text.find(marker).unwrap()
    }

    #[test]
    fn ts_local_const_in_function_and_shadowing() {
        let src = "const x = 1;\nfunction f() {\n  const x = 2;\n  return x + 1;\n}\n";
        let off = at(src, "x + 1");
        let info = analyse(Lang::Ts, "a.ts", src, off, "x");
        assert_eq!(info.local.unwrap().line, 3);
    }

    #[test]
    fn ts_destructured_param_and_for_of() {
        let src =
            "function f({ a, b: c }, [d]) {\n  for (const item of a) { use(item, c, d); }\n}\n";
        assert_eq!(
            analyse(Lang::Ts, "a.ts", src, at(src, "item, c"), "item")
                .local
                .unwrap()
                .line,
            2
        );
        assert_eq!(
            analyse(Lang::Ts, "a.ts", src, at(src, "c, d)"), "c")
                .local
                .unwrap()
                .line,
            1
        );
        assert_eq!(
            analyse(Lang::Ts, "a.ts", src, at(src, "d);"), "d")
                .local
                .unwrap()
                .line,
            1
        );
        assert_eq!(
            analyse(Lang::Ts, "a.ts", src, at(src, "a) {"), "a")
                .local
                .unwrap()
                .line,
            1
        );
    }

    #[test]
    fn ts_member_is_not_local_and_declaration_is_detected() {
        let src = "const mode = 1;\nthis.mode = mode;\n";
        let member = analyse(Lang::Ts, "a.ts", src, at(src, "mode = mode"), "mode");
        assert!(member.member && member.local.is_none());
        let decl = analyse(Lang::Ts, "a.ts", src, at(src, "mode = 1"), "mode");
        assert!(decl.on_declaration);
    }

    #[test]
    fn ts_import_alias_and_relative_hints() {
        let src = "import { slugify as slug } from '../lib/util';\nslug('x');\n";
        let info = analyse(Lang::Ts, "app/main.ts", src, at(src, "slug('x"), "slug");
        assert_eq!(info.lookup.as_deref(), Some("slugify"));
        assert!(info.path_hints.contains(&"lib/util.ts".to_string()));
        assert!(hint_matches("lib/util.js", "lib/util.js"));
    }

    #[test]
    fn php_var_first_assignment_and_foreach() {
        let src = "<?php\nfunction f($items) {\n  $total = 0;\n  foreach ($items as $item) {\n    $total = $total + $item;\n  }\n  return $total;\n}\n";
        let t = analyse(Lang::Php, "a.php", src, at(src, "total;\n}"), "total");
        assert_eq!(t.local.unwrap().line, 3);
        let i = analyse(Lang::Php, "a.php", src, at(src, "item;"), "item");
        assert_eq!(i.local.unwrap().line, 4);
        let p = analyse(Lang::Php, "a.php", src, at(src, "items as"), "items");
        assert_eq!(p.local.unwrap().line, 2);
    }

    #[test]
    fn php_reassignment_jumps_to_first_and_dollar_caret() {
        let src = "<?php\nfunction f() {\n  $n = 1;\n  $n = 2;\n  return $n;\n}\n";
        let re = analyse(Lang::Php, "a.php", src, at(src, "n = 2"), "n");
        assert_eq!(re.local.unwrap().line, 3);
        let first = analyse(Lang::Php, "a.php", src, at(src, "n = 1"), "n");
        assert!(first.on_declaration);
        let dollar = analyse(Lang::Php, "a.php", src, at(src, "$n;"), "n");
        assert_eq!(dollar.local.unwrap().line, 3);
    }

    #[test]
    fn self_member_is_flagged() {
        let src = "class A { x = 1; f() { return this.x; } }\n";
        let info = analyse(Lang::Ts, "a.ts", src, at(src, "x; }"), "x");
        assert!(info.member && info.self_member);
    }

    #[test]
    fn php_member_and_use_and_magento_hints() {
        let src =
            "<?php\nuse App\\Service\\UserService as Users;\n$u = new Users();\n$u->find();\n";
        let use_ = analyse(Lang::Php, "a.php", src, at(src, "Users()"), "Users");
        assert_eq!(use_.lookup.as_deref(), Some("UserService"));
        assert!(
            use_.path_hints
                .contains(&"service/userservice.php".to_string())
        );
        let m = analyse(Lang::Php, "a.php", src, at(src, "find()"), "find");
        assert!(m.member && m.local.is_none());
        let mage = analyse(
            Lang::Php,
            "a.php",
            "<?php\nMage_Catalog_Model_Product::x();\n",
            7,
            "Mage_Catalog_Model_Product",
        );
        assert!(
            mage.path_hints
                .contains(&"mage/catalog/model/product.php".to_string())
        );
        assert!(hint_matches(
            "htdocs/app/code/core/Mage/Catalog/Model/Product.php",
            "mage/catalog/model/product.php"
        ));
    }

    #[test]
    fn rust_let_and_match_binding() {
        let src =
            "fn f(o: Option<u8>) {\n  let n = 1;\n  match o { Some(v) => v + n, None => n };\n}\n";
        assert_eq!(
            analyse(Lang::Rust, "a.rs", src, at(src, "v + n"), "v")
                .local
                .unwrap()
                .line,
            3
        );
        assert_eq!(
            analyse(Lang::Rust, "a.rs", src, at(src, "n, None"), "n")
                .local
                .unwrap()
                .line,
            2
        );
        assert_eq!(
            analyse(Lang::Rust, "a.rs", src, at(src, "o {"), "o")
                .local
                .unwrap()
                .line,
            1
        );
        assert!(
            analyse(Lang::Rust, "a.rs", src, at(src, "Some(v)"), "Some")
                .local
                .is_none()
        );
    }

    #[test]
    fn python_and_go_locals() {
        let py = "def f(a, b=1):\n    for i in a:\n        x = i + b\n    return x\n";
        assert_eq!(
            analyse(Lang::Python, "a.py", py, at(py, "x\n"), "x")
                .local
                .unwrap()
                .line,
            3
        );
        assert_eq!(
            analyse(Lang::Python, "a.py", py, at(py, "b\n"), "b")
                .local
                .unwrap()
                .line,
            1
        );
        let go = "package m\nfunc f(n int) int {\n  s := n\n  for i := range n { s += i }\n  return s\n}\n";
        assert_eq!(
            analyse(Lang::Go, "a.go", go, at(go, "s\n}"), "s")
                .local
                .unwrap()
                .line,
            3
        );
        assert_eq!(
            analyse(Lang::Go, "a.go", go, at(go, "i }"), "i")
                .local
                .unwrap()
                .line,
            4
        );
    }
}
