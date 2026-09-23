//! Single-pass Rust source parser for the code graph.
//!
//! Parses `mod` declarations, `use` statements, and public export names
//! without a full AST — fast enough for thousands of files.

use std::path::Path;

use super::{DepEdge, DepKind, ModuleId, ModuleNode};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse a Rust source file's text content into a module node + dependency edges.
///
/// This is a single-pass, line-oriented parser that:
/// - strips `//` and `/* */` comments (state machine respecting string literals)
/// - detects `mod <name>;` (ModDecl) and `mod <name> {` (ModInline) declarations
/// - detects `use` statements including braced forms and `as` aliases
/// - heuristic: skips `#[cfg(test)]` mod blocks
/// - counts lines and collects `pub fn`, `pub struct`, `pub enum`, `pub trait` names
pub fn parse_file_content(
    text: &str,
    rel_path: &str,
    crate_name: &str,
) -> (ModuleNode, Vec<DepEdge>) {
    let cleaned = strip_comments(text);
    let lines = text.lines().count();

    let mut exports: Vec<String> = Vec::new();
    let mut edges: Vec<DepEdge> = Vec::new();
    // Outstanding open braces of a #[cfg(test)] mod block being skipped.
    let mut skip_cfg_test_braces: usize = 0;
    // Set when a bare `#[cfg(test)]` line was seen and the following
    // `mod ...` line must be skipped instead of parsed.
    let mut cfg_test_pending = false;

    // We need to track brace depth for cfg(test) skip and inline mods.
    let mut brace_depth: usize = 0;
    // For `mod name { ... }` — track what depth the brace opened at.
    let mut inline_mod_depth: Option<(String, usize)> = None;

    let from_id = ModuleId {
        path: rel_path.to_string(),
        crate_name: crate_name.to_string(),
    };

    for line in cleaned.lines() {
        let trimmed = line.trim();
        let opens = trimmed.chars().filter(|&c| c == '{').count();
        let closes = trimmed.chars().filter(|&c| c == '}').count();

        // Inside a #[cfg(test)] mod block: count braces, skip everything.
        if skip_cfg_test_braces > 0 || (cfg_test_pending && opens > 0) {
            cfg_test_pending = false;
            // The `{` on the `mod ... {` line opens the block; every further
            // `{` nests deeper, every `}` closes one level.
            let opened = skip_cfg_test_braces + opens;
            skip_cfg_test_braces = opened.saturating_sub(closes);
            continue;
        }

        // Bare `#[cfg(test)]` on its own line: the following mod line is skipped.
        if trimmed == "#[cfg(test)]" {
            cfg_test_pending = true;
            continue;
        }

        // `#[cfg(test)] mod ...` on one line.
        if trimmed.starts_with("#[cfg(test)]") {
            cfg_test_pending = false;
            if opens > 0 {
                // Inline `#[cfg(test)] mod tests { ...`: skip until braces balance.
                skip_cfg_test_braces = opens - closes.min(opens);
                if skip_cfg_test_braces == 0 && closes == 0 {
                    skip_cfg_test_braces = 1;
                }
            }
            // `#[cfg(test)] mod name;` (no braces): just skip the line.
            continue;
        }

        // A pending cfg(test) followed by a non-mod line: clear the flag.
        if cfg_test_pending && !trimmed.starts_with("mod ") && !trimmed.starts_with("pub ") {
            cfg_test_pending = false;
        }
        if cfg_test_pending && (trimmed.starts_with("mod ") || trimmed.contains(" mod ")) {
            cfg_test_pending = false;
            if opens > 0 {
                skip_cfg_test_braces = opens - closes.min(opens);
                if skip_cfg_test_braces == 0 {
                    skip_cfg_test_braces = 1;
                }
            }
            continue;
        }

        // Track brace depth for inline mod blocks (report ModInline edge when we see it).
        if inline_mod_depth.is_some() {
            // We already emitted the edge; track braces to know when we leave.
            for ch in trimmed.chars() {
                match ch {
                    '{' => brace_depth += 1,
                    '}' => {
                        if brace_depth == 0 {
                            // Done with this inline mod.
                            inline_mod_depth = None;
                        } else {
                            brace_depth -= 1;
                        }
                    }
                    _ => {}
                }
            }
            if inline_mod_depth.is_some() && brace_depth == 0 {
                inline_mod_depth = None;
                brace_depth = 0;
            }
            continue;
        }

        // --- Collect public exports ---
        if let Some(name) = extract_pub_export(trimmed) {
            exports.push(name);
        }

        // --- mod declarations ---
        if let Some(mod_edge) = try_parse_mod(trimmed, &from_id) {
            edges.push(mod_edge);
            continue;
        }

        // --- use declarations ---
        if let Some(mut use_edges) = try_parse_use(trimmed, &from_id, crate_name) {
            edges.append(&mut use_edges);
            continue;
        }
    }

    let node = ModuleNode {
        id: from_id,
        lines,
        exports,
    };

    (node, edges)
}

/// Parse a file from disk.
pub fn parse_file(
    root: &Path,
    rel_path: &str,
    crate_name: &str,
) -> Result<(ModuleNode, Vec<DepEdge>), String> {
    let full_path = root.join(rel_path);
    let content = std::fs::read_to_string(&full_path)
        .map_err(|e| format!("failed to read {}: {e}", full_path.display()))?;
    Ok(parse_file_content(&content, rel_path, crate_name))
}

// ---------------------------------------------------------------------------
// Comment stripping (state machine)
// ---------------------------------------------------------------------------

/// Strip `//` line comments and `/* */` block comments from source text,
/// respecting string literals and character literals.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut in_char = false;
    let mut escape_next = false;

    while let Some(ch) = chars.next() {
        if escape_next {
            out.push(ch);
            escape_next = false;
            continue;
        }
        if ch == '\\' && (in_string || in_char) {
            out.push(ch);
            escape_next = true;
            continue;
        }
        if ch == '"' && !in_char {
            in_string = !in_string;
            out.push(ch);
            continue;
        }
        if ch == '\'' && !in_string {
            in_char = !in_char;
            out.push(ch);
            continue;
        }
        if in_string || in_char {
            out.push(ch);
            continue;
        }
        // Not inside a literal.
        if ch == '/' {
            match chars.peek() {
                Some('/') => {
                    // Line comment — skip until end of line.
                    for c in chars.by_ref() {
                        if c == '\n' {
                            out.push('\n'); // preserve line count
                            break;
                        }
                    }
                }
                Some('*') => {
                    chars.next(); // consume '*'
                    // Block comment — skip until closing `*/`.
                    let mut depth = 1u32;
                    let mut prev = '*';
                    for c in chars.by_ref() {
                        if prev == '*' && c == '/' {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        } else if prev == '/' && c == '*' {
                            depth += 1;
                        }
                        prev = c;
                    }
                }
                _ => {
                    out.push(ch);
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Line parsing helpers
// ---------------------------------------------------------------------------

/// Try to parse a `mod` declaration from a trimmed line.
/// Returns `Some(DepEdge)` if the line is a mod declaration.
fn try_parse_mod(line: &str, from_id: &ModuleId) -> Option<DepEdge> {
    // Match: [pub] [pub(crate)] mod <name> [;|{...]
    let rest = strip_mod_prefix(line)?;
    // Strip trailing `;` or anything after the name + optional `{`.
    let rest = rest.trim_end_matches(';').trim();
    let name = rest.split_whitespace().next()?;
    // Reject if name looks wrong (include `{` check since inline mods have it).
    let name = name.trim_end_matches('{');
    if !is_valid_ident(name) {
        return None;
    }

    let is_inline = rest.contains('{');

    let to_path = compute_mod_path(&from_id.path, name, is_inline);

    let to_id = ModuleId {
        path: to_path,
        crate_name: from_id.crate_name.clone(),
    };

    Some(DepEdge {
        from: from_id.clone(),
        to: to_id,
        kind: if is_inline {
            DepKind::ModInline
        } else {
            DepKind::ModDecl
        },
    })
}

/// Strip leading `pub`, `pub(crate)`, `pub(super)`, `pub(in path)` and `mod` keyword.
/// Returns the rest of the line after `mod `.
fn strip_mod_prefix(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let line = if let Some(r) = line.strip_prefix("pub") {
        let r = r.trim_start();
        // pub(crate), pub(super), pub(in path)
        if r.starts_with('(') {
            if let Some(end) = r.find(')') {
                r[end + 1..].trim_start()
            } else {
                return None;
            }
        } else {
            r
        }
    } else {
        line
    };
    let line = line.strip_prefix("mod")?.trim_start();
    Some(line)
}

/// Compute the child module path given a parent's rel_path and child name.
/// For `ModDecl` (`mod foo;`), the path is `parent_dir/foo.rs` or `parent_dir/foo/mod.rs`.
/// For `ModInline`, the path is `parent_path::foo` (convention for inlines).
fn compute_mod_path(parent_path: &str, child_name: &str, is_inline: bool) -> String {
    if is_inline {
        // Inline: "parent::child" convention
        let parent = parent_path.trim_end_matches(".rs");
        format!("{parent}::{child_name}")
    } else {
        // File-based: compute sibling path.
        if let Some(parent_dir) = parent_path.rsplit_once('/') {
            let dir = parent_dir.0;
            format!("{dir}/{child_name}.rs")
        } else {
            format!("{child_name}.rs")
        }
    }
}

// ---------------------------------------------------------------------------
// Use parsing
// ---------------------------------------------------------------------------

/// Try to parse one or more `use` statements from a trimmed line.
/// Handles: `use a::b;`, `pub use a::b;`, `use a::{B, C as D};`
fn try_parse_use(line: &str, from_id: &ModuleId, crate_name: &str) -> Option<Vec<DepEdge>> {
    let line = if let Some(r) = line.strip_prefix("pub ") {
        r
    } else {
        line
    };
    let line = line.strip_prefix("use ")?.trim();
    let line = line.trim_end_matches(';').trim();

    let paths = expand_braced_use(line);
    let mut edges = Vec::new();

    for path in &paths {
        let resolved = resolve_use_path(path);
        let kind = if is_external_use(path, crate_name) {
            DepKind::UseExternal
        } else {
            DepKind::Use
        };
        let to_id = ModuleId {
            path: resolved,
            crate_name: from_id.crate_name.clone(),
        };
        edges.push(DepEdge {
            from: from_id.clone(),
            to: to_id,
            kind,
        });
    }

    if edges.is_empty() { None } else { Some(edges) }
}

/// Expand braced `use` statements.
///
/// `a::{B, C as D}` → `["a::B", "a::D"]`
/// `a::b::c` → `["a::b::c"]`
/// Handles nested braces by tracking depth.
pub fn expand_braced_use(path: &str) -> Vec<String> {
    // Find the first `{` at depth 0.
    let mut depth = 0usize;
    let mut brace_pos = None;

    for (i, ch) in path.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    brace_pos = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0
                    && let Some(bp) = brace_pos
                {
                    let prefix = &path[..bp];
                    let inner = &path[bp + 1..i];
                    let suffix = &path[i + 1..];
                    // Split inner by commas (respecting nested braces).
                    let items = split_braced_items(inner);
                    let mut result = Vec::new();
                    for item in items {
                        let item = item.trim();
                        let item_stripped = strip_use_alias(item);
                        // Recurse for nested braces.
                        let expanded =
                            expand_braced_use(&format!("{prefix}{item_stripped}{suffix}"));
                        result.extend(expanded);
                    }
                    return result;
                }
            }
            _ => {}
        }
    }

    // No braces — just a simple path (possibly with `as` alias).
    let stripped = strip_use_alias(path);
    vec![stripped.to_string()]
}

/// Split items inside braces by commas, respecting nested braces.
fn split_braced_items(inner: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;

    for (i, ch) in inner.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                items.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    items.push(&inner[start..]);
    items
}

/// Strip `as <alias>` from a use item.
/// `"Foo as Bar"` → `"Foo"`, `"Foo"` → `"Foo"`.
fn strip_use_alias(item: &str) -> &str {
    if let Some(as_pos) = item.find(" as ") {
        item[..as_pos].trim()
    } else {
        item.trim()
    }
}

/// Resolve a `use` path to a file-path-like string.
///
/// Strips the first segment if it's `crate`, `self`, `super`, or the crate name.
/// Converts `::` to `/`.
pub fn resolve_use_path(path: &str) -> String {
    let resolved = strip_mod_prefix_use(path);
    resolved.replace("::", "/")
}

/// Strip leading `crate`, `self`, `super`, or crate-name prefix from a use path.
fn strip_mod_prefix_use(path: &str) -> &str {
    let first = path.split("::").next().unwrap_or("");
    match first {
        "crate" | "self" | "super" => {
            let skip = first.len();
            if path.as_bytes().get(skip) == Some(&b':')
                && path.as_bytes().get(skip + 1) == Some(&b':')
            {
                &path[skip + 2..]
            } else {
                path
            }
        }
        _ => {
            // Check if it's the crate name (caller should handle, but we keep it simple).
            path
        }
    }
}

/// Determine if a use path refers to an external crate.
/// Works with unresolved `::`-separated paths.
fn is_external_use(path: &str, crate_name: &str) -> bool {
    let first = path.split("::").next().unwrap_or("");
    first != "crate" && first != "self" && first != "super" && first != crate_name
}

// ---------------------------------------------------------------------------
// Export extraction
// ---------------------------------------------------------------------------

/// If the line is a public export (pub fn/struct/enum/trait incl.
/// pub(crate)/pub(super)/pub(in path) variants), return the name.
fn extract_pub_export(line: &str) -> Option<String> {
    let line = line.strip_prefix("pub")?;
    let line = line
        .strip_prefix('(')
        .map(|r| {
            if let Some(end) = r.find(')') {
                r[end + 1..].trim_start()
            } else {
                ""
            }
        })
        .unwrap_or_else(|| line.strip_prefix(' ').unwrap_or(line));
    let line = line.strip_prefix("pub ").unwrap_or(line);

    for keyword in &["fn", "struct", "enum", "trait"] {
        if let Some(rest) = line.strip_prefix(*keyword) {
            let name = rest
                .trim_start()
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()?;
            if is_valid_ident(name) {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Check if a string is a valid Rust identifier (simplified).
fn is_valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .map(|c| c.is_alphabetic() || c == '_')
            .unwrap_or(false)
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- mod declarations ---

    #[test]
    fn mod_decl_simple() {
        let (node, edges) = parse_file_content("mod foo;", "src/lib.rs", "my_crate");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, DepKind::ModDecl);
        assert_eq!(edges[0].to.path, "src/foo.rs");
        assert_eq!(node.lines, 1);
    }

    #[test]
    fn pub_mod_decl() {
        let (_node, edges) = parse_file_content("pub mod bar;", "src/lib.rs", "c");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, DepKind::ModDecl);
        assert_eq!(edges[0].to.path, "src/bar.rs");
    }

    #[test]
    fn pub_crate_mod_decl() {
        let (_node, edges) = parse_file_content("pub(crate) mod inner;", "src/foo.rs", "c");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, DepKind::ModDecl);
        assert_eq!(edges[0].to.path, "src/inner.rs");
    }

    #[test]
    fn inline_mod() {
        let (_node, edges) = parse_file_content(
            "mod utils {\n    pub fn helper() {}\n}",
            "src/lib.rs",
            "my_crate",
        );
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, DepKind::ModInline);
        assert_eq!(edges[0].to.path, "src/lib::utils");
    }

    // --- use declarations ---

    #[test]
    fn use_simple() {
        let (_node, edges) = parse_file_content("use crate::foo;", "src/main.rs", "my_crate");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, DepKind::Use);
        assert_eq!(edges[0].to.path, "foo");
    }

    #[test]
    fn use_braced() {
        let (_node, edges) = parse_file_content("use crate::{Foo, Bar};", "src/main.rs", "c");
        assert_eq!(edges.len(), 2);
        let mut paths: Vec<&str> = edges.iter().map(|e| e.to.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, vec!["Bar", "Foo"]);
    }

    #[test]
    fn use_braced_nested() {
        let (_node, edges) = parse_file_content(
            "use std::collections::{HashMap, HashSet};",
            "src/main.rs",
            "my_crate",
        );
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().all(|e| e.kind == DepKind::UseExternal));
    }

    #[test]
    fn use_alias() {
        let (_node, edges) = parse_file_content("use crate::foo::Bar as Baz;", "src/main.rs", "c");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to.path, "foo/Bar");
        assert_eq!(edges[0].kind, DepKind::Use);
    }

    #[test]
    fn use_crate_path() {
        let (_node, edges) = parse_file_content(
            "use crate::config::loader;",
            "src/agent/mod.rs",
            "bebok_core",
        );
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to.path, "config/loader");
        assert_eq!(edges[0].kind, DepKind::Use);
    }

    #[test]
    fn use_external() {
        let (_node, edges) = parse_file_content(
            "use serde::{Serialize, Deserialize};",
            "src/main.rs",
            "my_crate",
        );
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().all(|e| e.kind == DepKind::UseExternal));
    }

    #[test]
    fn pub_use() {
        let (_node, edges) = parse_file_content("pub use crate::thing;", "src/lib.rs", "c");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, DepKind::Use);
    }

    // --- comments ---

    #[test]
    fn line_comment_ignored() {
        let (_node, edges) =
            parse_file_content("// use crate::foo;\nuse crate::bar;", "src/main.rs", "c");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to.path, "bar");
    }

    #[test]
    fn block_comment_ignored() {
        let (_node, edges) =
            parse_file_content("/* use crate::foo; */\nuse crate::bar;", "src/main.rs", "c");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to.path, "bar");
    }

    #[test]
    fn nested_block_comment() {
        let (_node, edges) = parse_file_content(
            "/* /* inner */ use crate::foo; */\nuse crate::bar;",
            "src/main.rs",
            "c",
        );
        assert_eq!(edges.len(), 1);
    }

    #[test]
    fn comment_inside_string_not_stripped() {
        let (node, _edges) =
            parse_file_content(r#"let s = "http://example.com";"#, "src/main.rs", "c");
        // No edges, just checking it doesn't panic and preserves lines.
        assert_eq!(node.lines, 1);
    }

    // --- cfg(test) ---

    #[test]
    fn cfg_test_inline_mod_ignored() {
        let (_node, edges) = parse_file_content(
            "#[cfg(test)]\nmod tests {\n    use crate::foo;\n}",
            "src/main.rs",
            "c",
        );
        assert!(
            edges.is_empty(),
            "cfg(test) mod block should not produce edges, got {edges:?}"
        );
    }

    #[test]
    fn cfg_test_file_mod_ignored() {
        let (_node, edges) = parse_file_content("#[cfg(test)] mod tests;", "src/main.rs", "c");
        assert!(edges.is_empty(), "cfg(test) mod; should not produce edges");
    }

    // --- exports ---

    #[test]
    fn collect_pub_fn() {
        let (node, _edges) = parse_file_content(
            "pub fn do_stuff() {}\npub fn also_this() {}",
            "src/lib.rs",
            "c",
        );
        assert_eq!(node.exports, vec!["do_stuff", "also_this"]);
    }

    #[test]
    fn collect_pub_struct_enum_trait() {
        let (node, _edges) = parse_file_content(
            "pub struct Foo;\npub enum Bar {}\npub trait Baz {}",
            "src/lib.rs",
            "c",
        );
        assert_eq!(node.exports, vec!["Foo", "Bar", "Baz"]);
    }

    #[test]
    fn private_not_collected() {
        let (node, _edges) =
            parse_file_content("fn private() {}\nstruct Internal;", "src/lib.rs", "c");
        assert!(node.exports.is_empty());
    }

    #[test]
    fn pub_crate_export_collected() {
        let (node, _edges) = parse_file_content("pub(crate) fn helper() {}", "src/lib.rs", "c");
        assert_eq!(node.exports, vec!["helper"]);
    }

    // --- line counting ---

    #[test]
    fn line_count_matches() {
        let (node, _) = parse_file_content("line1\nline2\nline3\n", "x.rs", "c");
        assert_eq!(node.lines, 3); // str::lines() ignores trailing newline
    }

    // --- helpers ---

    #[test]
    fn expand_braced_simple() {
        let r = expand_braced_use("a::{B, C}");
        assert_eq!(r, vec!["a::B", "a::C"]);
    }

    #[test]
    fn expand_braced_with_alias() {
        let r = expand_braced_use("a::{B as X, C}");
        assert_eq!(r, vec!["a::B", "a::C"]);
    }

    #[test]
    fn expand_no_braces() {
        let r = expand_braced_use("a::b::c");
        assert_eq!(r, vec!["a::b::c"]);
    }

    #[test]
    fn expand_nested_braces() {
        let r = expand_braced_use("std::{collections::{HashMap, HashSet}, io}");
        let mut sorted = r;
        sorted.sort();
        assert_eq!(
            sorted,
            vec![
                "std::collections::HashMap",
                "std::collections::HashSet",
                "std::io"
            ]
        );
    }

    #[test]
    fn strip_alias() {
        assert_eq!(strip_use_alias("Foo as Bar"), "Foo");
        assert_eq!(strip_use_alias("Foo"), "Foo");
    }

    #[test]
    fn resolve_use_path_crate() {
        assert_eq!(resolve_use_path("crate::foo::bar"), "foo/bar");
    }

    #[test]
    fn resolve_use_path_plain() {
        assert_eq!(resolve_use_path("serde::Serialize"), "serde/Serialize");
    }

    #[test]
    fn strip_mod_prefix_pub() {
        assert_eq!(strip_mod_prefix("pub mod foo;"), Some("foo;"));
    }

    #[test]
    fn strip_mod_prefix_plain() {
        assert_eq!(strip_mod_prefix("mod bar;"), Some("bar;"));
    }

    #[test]
    fn strip_mod_prefix_pub_crate() {
        assert_eq!(strip_mod_prefix("pub(crate) mod x;"), Some("x;"));
    }

    // --- parse_file (disk) ---

    #[test]
    fn parse_file_reads_disk() {
        let dir = std::env::temp_dir().join(format!("bebok-parser-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lib.rs"), "use crate::foo;\npub fn bar() {}\n").unwrap();

        let result = parse_file(&dir, "lib.rs", "test_crate");
        assert!(result.is_ok());
        let (node, edges) = result.unwrap();
        assert_eq!(node.lines, 2);
        assert_eq!(node.exports, vec!["bar"]);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to.path, "foo");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_file_missing_returns_error() {
        let result = parse_file(Path::new("/nonexistent"), "nope.rs", "c");
        assert!(result.is_err());
    }

    // --- edge cases ---

    #[test]
    fn empty_content() {
        let (node, edges) = parse_file_content("", "empty.rs", "c");
        assert_eq!(node.lines, 0);
        assert!(node.exports.is_empty());
        assert!(edges.is_empty());
    }

    #[test]
    fn multiple_use_lines() {
        let text = "use crate::a;\nuse crate::b;\nuse crate::c;";
        let (_node, edges) = parse_file_content(text, "src/main.rs", "c");
        assert_eq!(edges.len(), 3);
        let mut paths: Vec<&str> = edges.iter().map(|e| e.to.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, vec!["a", "b", "c"]);
    }

    #[test]
    fn mod_and_use_together() {
        let text = "mod foo;\nuse crate::bar;";
        let (_node, edges) = parse_file_content(text, "src/lib.rs", "c");
        assert_eq!(edges.len(), 2);
        let kinds: Vec<DepKind> = edges.iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&DepKind::ModDecl));
        assert!(kinds.contains(&DepKind::Use));
    }
}
