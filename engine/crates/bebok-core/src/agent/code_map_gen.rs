//! Project code map generation: scan the project tree, derive one sentence
//! per directory and cache the result in `<project>/.bebok/code-map.json`.
//!
//! The map answers "what is where" for the model up front (orientation),
//! complementing the code index which answers "where is X?" at query time.
//! Descriptions come, in priority order, from `Cargo.toml` -> `package.json`
//! -> `README.md` -> `AGENTS.md` -> `mod.rs`/`lib.rs`/`index.rs` module docs
//! -> a seeded title-case fallback derived from the directory name; manual
//! `code_map.overrides` from the config win over all of them.
//!
//! Kept in its own module (pure, synchronous) so `code_map_prompt.rs` only
//! does read-or-generate -> truncate -> render, and the HTTP route can force
//! a regeneration without touching the prompt path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::CodeMapConfig;

/// Cache format version (bump when the on-disk shape changes).
pub const CACHE_VERSION: u32 = 1;
/// Cache file name inside `<project>/.bebok/`.
pub const CACHE_FILE: &str = "code-map.json";
/// Heading rendered above the entries (re-exported as
/// `code_map_prompt::SECTION_HEADING`).
pub const SECTION_HEADING: &str = "Project code map";
/// Longest description we keep from a file (README first lines can be long).
const MAX_DESCRIPTION_LEN: usize = 240;
/// Directories never worth mapping (VCS state, deps, build output, own data).
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".bebok",
    ".github",
    ".vscode",
    ".idea",
    "node_modules",
    "target",
    "dist",
    "build",
    "__pycache__",
    ".next",
    ".nuxt",
    ".cache",
    ".venv",
    "venv",
    "coverage",
    "vendor",
];
/// Directory-name -> description seeds for names whose title-case would be
/// bland nonsense ("src" -> "Source").
const SEEDS: &[(&str, &str)] = &[
    ("src", "Source"),
    ("lib", "Library"),
    ("tests", "Tests"),
    ("test", "Tests"),
    ("docs", "Documentation"),
    ("doc", "Documentation"),
    ("scripts", "Scripts"),
    ("crates", "Crates"),
    ("bin", "Binaries"),
    ("examples", "Examples"),
    ("benches", "Benchmarks"),
    ("config", "Configuration"),
    ("assets", "Assets"),
    ("public", "Static assets"),
    ("styles", "Styles"),
    ("components", "Components"),
];

/// One directory entry of the map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeMapEntry {
    /// Project-relative path with a trailing `/` (`engine/crates/bebok-core/`).
    pub path: String,
    /// One-sentence description of what lives there.
    pub description: String,
}

/// On-disk cache shape (`<project>/.bebok/code-map.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeMapCache {
    pub version: u32,
    /// RFC 3339 UTC timestamp of the scan (drives the staleness check).
    pub generated_at: String,
    /// Absolute project root the map was generated for.
    pub root: String,
    pub entries: Vec<CodeMapEntry>,
}

/// The cache file path for a project root.
pub fn cache_path(root: &Path) -> PathBuf {
    root.join(".bebok").join(CACHE_FILE)
}

/// Rough token estimate (chars / 4), ceiling so short lines cost >= 1.
pub fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() + 3) / 4
}

/// Scan `root` up to `config.max_depth` and build the map. Returns `None`
/// when `root` is not a directory; an empty project yields a cache with an
/// empty `entries` list (never a crash).
pub fn generate_code_map(root: &Path, config: &CodeMapConfig) -> Option<CodeMapCache> {
    if !root.is_dir() {
        return None;
    }
    let mut entries = Vec::new();
    walk(root, "", 1, config.max_depth, &mut entries);
    apply_overrides(&mut entries, &config.overrides);
    Some(CodeMapCache {
        version: CACHE_VERSION,
        generated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        root: root.to_string_lossy().into_owned(),
        entries,
    })
}

/// Depth-first scan: every non-empty, non-skipped child directory up to
/// `max_depth` (root children are depth 1) becomes one entry, sorted by name.
fn walk(dir: &Path, rel: &str, depth: usize, max_depth: usize, out: &mut Vec<CodeMapEntry>) {
    if depth > max_depth {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut subdirs: Vec<(String, PathBuf)> = Vec::new();
    for child in rd.flatten() {
        let name = child.file_name().to_string_lossy().into_owned();
        let path = child.path();
        if path.is_dir() {
            if is_skipped(&name) {
                continue;
            }
            subdirs.push((name, path));
        }
    }
    subdirs.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, path) in subdirs {
        let child_rel = format!("{rel}{name}/");
        let (grandchildren, child_files) = dir_counts(&path);
        // Empty directories carry no information: skip them.
        if child_files == 0 && grandchildren == 0 {
            continue;
        }
        let description = describe(&path, &name);
        out.push(CodeMapEntry {
            path: child_rel.clone(),
            description,
        });
        walk(&path, &child_rel, depth + 1, max_depth, out);
    }
}

/// Directories skipped everywhere (dotfiles + deps + build output).
fn is_skipped(name: &str) -> bool {
    name.starts_with('.') || SKIP_DIRS.contains(&name)
}

/// (kept subdirectory count, file count) of a directory — decides emptiness.
fn dir_counts(dir: &Path) -> (usize, usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut dirs = 0;
    let mut files = 0;
    for child in rd.flatten() {
        let name = child.file_name().to_string_lossy().into_owned();
        if child.path().is_dir() {
            if !is_skipped(&name) {
                dirs += 1;
            }
        } else {
            files += 1;
        }
    }
    (dirs, files)
}

/// Derive the description for one directory (file-based, then fallback).
fn describe(dir: &Path, name: &str) -> String {
    read_cargo_description(dir)
        .or_else(|| read_package_description(dir))
        .or_else(|| read_readme_line(dir))
        .or_else(|| read_agents_line(dir))
        .or_else(|| read_module_doc(dir))
        .unwrap_or_else(|| auto_description(name))
}

/// `description = "..."` from a `Cargo.toml` (crate metadata).
fn read_cargo_description(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    for line in text.lines() {
        let l = line.trim();
        let Some(rest) = l.strip_prefix("description") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let raw = rest.trim();
        let val = raw
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(raw)
            .trim();
        if !val.is_empty() {
            return Some(clean_description(val));
        }
    }
    None
}

/// `"description": "..."` from a `package.json`.
fn read_package_description(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let desc = value.get("description")?.as_str()?.trim();
    if desc.is_empty() {
        return None;
    }
    Some(clean_description(desc))
}

/// First non-empty line of a `README.md`, with the `#` heading stripped.
fn read_readme_line(dir: &Path) -> Option<String> {
    for name in ["README.md", "readme.md", "README.MD", "README"] {
        if let Some(line) = first_content_line(&dir.join(name)) {
            return Some(line);
        }
    }
    None
}

/// First content line of an `AGENTS.md` after its leading heading.
fn read_agents_line(dir: &Path) -> Option<String> {
    let path = dir.join("AGENTS.md");
    let text = std::fs::read_to_string(path).ok()?;
    let mut seen_heading = false;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t.starts_with('#') {
            if seen_heading {
                continue;
            }
            seen_heading = true;
            continue;
        }
        let t = t.trim_start_matches(['-', '*', ' ']).trim();
        if !t.is_empty() {
            return Some(clean_description(t));
        }
    }
    None
}

/// First `//!` module doc line of `mod.rs` / `lib.rs` / `index.rs`.
fn read_module_doc(dir: &Path) -> Option<String> {
    for name in ["mod.rs", "lib.rs", "index.rs"] {
        let Ok(text) = std::fs::read_to_string(dir.join(name)) else {
            continue;
        };
        for line in text.lines() {
            let t = line.trim();
            if let Some(doc) = t.strip_prefix("//!") {
                let doc = doc.trim().trim_start_matches(['!', '/']).trim();
                if !doc.is_empty() {
                    return Some(clean_description(doc));
                }
            }
        }
    }
    None
}

/// First meaningful line of a file (heading markers stripped).
fn first_content_line(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let t = line.trim().trim_start_matches('#').trim();
        if !t.is_empty() {
            return Some(clean_description(t));
        }
    }
    None
}

/// Collapse whitespace and cap length so one entry stays one sentence-ish
/// line in the prompt.
fn clean_description(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = collapsed.chars().take(MAX_DESCRIPTION_LEN).collect();
    if out.chars().count() < collapsed.chars().count() {
        while !out.ends_with(char::is_whitespace) && out.chars().count() > MAX_DESCRIPTION_LEN - 1 {
            out.pop();
        }
        out.push('…');
    }
    out.trim().to_string()
}

/// Last-resort description derived from the directory name:
/// `"bebok-core"` -> `"Bebok core"`, `"code_index_tools"` -> `"Code index
/// tools"`, `"codeMap"` -> `"Code map"` (seeded names win over title-case).
pub fn auto_description(dir_name: &str) -> String {
    if let Some((_, desc)) = SEEDS.iter().find(|(k, _)| k.eq_ignore_ascii_case(dir_name)) {
        return desc.to_string();
    }
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for ch in dir_name.chars() {
        if ch == '-' || ch == '_' || ch == '.' || ch.is_whitespace() {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
        } else if ch.is_uppercase() {
            if !cur.is_empty() && prev_lower {
                tokens.push(std::mem::take(&mut cur));
            }
            cur.push(ch.to_ascii_lowercase());
            prev_lower = false;
        } else {
            cur.push(ch);
            prev_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    if tokens.is_empty() {
        return dir_name.to_string();
    }
    let joined = tokens.join(" ");
    let mut chars = joined.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => joined,
    }
}

/// Apply `code_map.overrides` (key = path without trailing slash).
pub fn apply_overrides(entries: &mut [CodeMapEntry], overrides: &HashMap<String, String>) {
    if overrides.is_empty() {
        return;
    }
    for entry in entries.iter_mut() {
        let key = entry.path.trim_end_matches('/');
        if let Some(desc) = overrides.get(key) {
            entry.description = desc.clone();
        }
    }
}

/// Read the cache file; `None` on missing/corrupt/foreign-version caches.
pub fn read_cache(path: &Path) -> Option<CodeMapCache> {
    let text = std::fs::read_to_string(path).ok()?;
    let cache: CodeMapCache = serde_json::from_str(&text).ok()?;
    if cache.version != CACHE_VERSION {
        return None;
    }
    Some(cache)
}

/// Atomic cache write (tmp + rename), same pattern as the JSONC config
/// writers. Creates `<project>/.bebok/` when missing.
pub fn write_cache(path: &Path, cache: &CodeMapCache) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(cache).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// A cache older than 24 hours (or with an unreadable timestamp) is stale.
pub fn is_stale(cache: &CodeMapCache) -> bool {
    match chrono::DateTime::parse_from_rfc3339(&cache.generated_at) {
        Ok(dt) => {
            let age = chrono::Utc::now().signed_duration_since(dt.with_timezone(&chrono::Utc));
            age.num_seconds().abs() > 24 * 60 * 60
        }
        Err(_) => true,
    }
}

/// Rendered depth of an entry (number of path components).
fn depth_of(path: &str) -> usize {
    path.trim_end_matches('/').matches('/').count()
}

/// One rendered line: `- <indent>- path/ → description` (an empty `path`
/// renders as a plain bullet, used for the truncation marker).
fn render_line(entry: &CodeMapEntry) -> String {
    if entry.path.is_empty() {
        return format!("- {}", entry.description);
    }
    format!(
        "{}- {} → {}",
        "  ".repeat(depth_of(&entry.path)),
        entry.path,
        entry.description
    )
}

/// Trim the entry list to `max_tokens` (rough estimate). Deepest entries go
/// first, then the shortest descriptions; root-level entries are removed
/// last and the first one is never dropped, so a top-level view always
/// survives. When anything was cut, a `… (N more entries omitted)` marker
/// is appended.
pub fn truncate_to_budget(entries: &[CodeMapEntry], max_tokens: usize) -> Vec<CodeMapEntry> {
    if entries.is_empty() {
        return Vec::new();
    }
    let used: usize = entries
        .iter()
        .map(|e| estimate_tokens(&render_line(e)))
        .sum();
    if used <= max_tokens {
        return entries.to_vec();
    }

    // Removal order: non-root first (deepest, then shortest description),
    // root-level entries after them.
    let depth = |i: usize| depth_of(&entries[i].path);
    let is_root = |i: usize| depth(i) == 0;
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|&a, &b| {
        (
            is_root(a) as u8,
            std::cmp::Reverse(depth(a)),
            entries[a].description.len(),
        )
            .cmp(&(
                is_root(b) as u8,
                std::cmp::Reverse(depth(b)),
                entries[b].description.len(),
            ))
    });

    // Keep at least one root-level entry (always a top-level view).
    let protected = entries.iter().position(|e| depth_of(&e.path) == 0);
    let mut removed = vec![false; entries.len()];
    let mut budget = used;
    for i in order {
        if budget <= max_tokens {
            break;
        }
        if protected == Some(i) {
            continue;
        }
        removed[i] = true;
        budget -= estimate_tokens(&render_line(&entries[i]));
    }

    let mut out: Vec<CodeMapEntry> = entries
        .iter()
        .zip(&removed)
        .filter(|(_, &r)| !r)
        .map(|(e, _)| e.clone())
        .collect();
    let n = removed.iter().filter(|&&r| r).count();
    if n > 0 {
        out.push(CodeMapEntry {
            path: String::new(),
            description: format!("… ({n} more entries omitted)"),
        });
    }
    out
}

/// Render the entries as the prompt section (hierarchical, 2 spaces per
/// level): `Project code map:` + one `- path/ → description` line each.
pub fn render_to_prompt(entries: &[CodeMapEntry]) -> String {
    let mut s = String::from(SECTION_HEADING);
    s.push_str(":\n");
    for entry in entries {
        s.push_str(&render_line(entry));
        s.push('\n');
    }
    s.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("bebok-codemap-gen-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn config() -> CodeMapConfig {
        CodeMapConfig {
            enabled: true,
            ..Default::default()
        }
    }

    fn entry(path: &str, description: &str) -> CodeMapEntry {
        CodeMapEntry {
            path: path.to_string(),
            description: description.to_string(),
        }
    }

    #[test]
    fn reads_cargo_toml_description() {
        let root = temp_root("cargo");
        std::fs::create_dir_all(root.join("crate")).unwrap();
        std::fs::write(
            root.join("crate/Cargo.toml"),
            "[package]\nname = \"x\"\ndescription = \"Rust engine: agent loop\"\n",
        )
        .unwrap();
        std::fs::write(root.join("crate/lib.rs"), "").unwrap();
        let cache = generate_code_map(&root, &config()).unwrap();
        assert_eq!(cache.entries[0].description, "Rust engine: agent loop");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reads_package_json_description() {
        let root = temp_root("package");
        std::fs::create_dir_all(root.join("web")).unwrap();
        std::fs::write(
            root.join("web/package.json"),
            r#"{ "name": "web", "description": "Angular client" }"#,
        )
        .unwrap();
        let cache = generate_code_map(&root, &config()).unwrap();
        assert_eq!(cache.entries[0].description, "Angular client");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reads_readme_first_line() {
        let root = temp_root("readme");
        std::fs::create_dir_all(root.join("docsdir")).unwrap();
        std::fs::write(
            root.join("docsdir/README.md"),
            "# Bebok handbook\n\nLong text below.\n",
        )
        .unwrap();
        std::fs::write(root.join("docsdir/page.md"), "x").unwrap();
        let cache = generate_code_map(&root, &config()).unwrap();
        assert_eq!(cache.entries[0].description, "Bebok handbook");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reads_rust_module_docs() {
        let root = temp_root("mod");
        std::fs::create_dir_all(root.join("mymod")).unwrap();
        std::fs::write(root.join("mymod/mod.rs"), "//! Session persistence.\n").unwrap();
        let cache = generate_code_map(&root, &config()).unwrap();
        assert_eq!(cache.entries[0].description, "Session persistence.");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn auto_description_from_dir_name() {
        assert_eq!(auto_description("bebok-core"), "Bebok core");
        assert_eq!(auto_description("code_index_tools"), "Code index tools");
        assert_eq!(auto_description("codeMap"), "Code map");
        assert_eq!(auto_description("src"), "Source");
        assert_eq!(auto_description("tests"), "Tests");
        assert_eq!(auto_description("config"), "Configuration");
    }

    #[test]
    fn max_depth_limits_scan() {
        let root = temp_root("depth");
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        std::fs::write(root.join("a/f.txt"), "x").unwrap();
        std::fs::write(root.join("a/b/f.txt"), "x").unwrap();
        std::fs::write(root.join("a/b/c/f.txt"), "x").unwrap();

        let mut cfg = config();
        cfg.max_depth = 2;
        let cache = generate_code_map(&root, &cfg).unwrap();
        let paths: Vec<&str> = cache.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"a/"));
        assert!(paths.contains(&"a/b/"));
        assert!(!paths.contains(&"a/b/c/"), "depth 3 must not be scanned");

        cfg.max_depth = 3;
        let cache = generate_code_map(&root, &cfg).unwrap();
        assert!(
            cache.entries.iter().any(|e| e.path == "a/b/c/"),
            "depth 3 included when max_depth = 3"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ignores_dotfiles_and_build_dirs() {
        let root = temp_root("skip");
        for dir in [
            ".git",
            "node_modules",
            "target",
            "dist",
            ".bebok",
            ".hidden",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("f.txt"), "x").unwrap();
        }
        std::fs::create_dir_all(root.join("keep")).unwrap();
        std::fs::write(root.join("keep/f.txt"), "x").unwrap();

        let cache = generate_code_map(&root, &config()).unwrap();
        assert_eq!(cache.entries.len(), 1, "{:?}", cache.entries);
        assert_eq!(cache.entries[0].path, "keep/");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn generates_atomic_cache_file() {
        let root = temp_root("atomic");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        let cache = generate_code_map(&root, &config()).unwrap();
        let path = cache_path(&root);
        write_cache(&path, &cache).unwrap();
        assert!(path.is_file(), "cache file must exist after generation");
        let read = read_cache(&path).unwrap();
        assert_eq!(read.entries, cache.entries);
        assert!(
            !path.with_extension("json.tmp").exists(),
            "tmp file renamed away"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_cache_when_no_dirs() {
        let root = temp_root("empty");
        let cache = generate_code_map(&root, &config()).unwrap();
        assert!(
            cache.entries.is_empty(),
            "empty root -> empty map, no crash"
        );
        let rendered = render_to_prompt(&cache.entries);
        assert_eq!(rendered, "Project code map:");
        // Missing root -> None (caller reports no map).
        assert!(generate_code_map(&root.join("nope"), &config()).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_directories_are_skipped() {
        let root = temp_root("empty-dirs");
        std::fs::create_dir_all(root.join("ghost")).unwrap();
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::write(root.join("real/f.txt"), "x").unwrap();
        let cache = generate_code_map(&root, &config()).unwrap();
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.entries[0].path, "real/");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn overrides_replace_auto_descriptions() {
        let root = temp_root("overrides");
        std::fs::create_dir_all(root.join("engine")).unwrap();
        std::fs::write(root.join("engine/f.txt"), "x").unwrap();
        let mut cfg = config();
        cfg.overrides
            .insert("engine".to_string(), "Rust engine: agent loop".to_string());
        let cache = generate_code_map(&root, &cfg).unwrap();
        assert_eq!(cache.entries[0].description, "Rust engine: agent loop");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn max_tokens_truncates_deepest_first() {
        let entries = vec![
            entry("engine/", "Rust engine."),
            entry("engine/crates/", "Crates."),
            entry(
                "engine/crates/bebok-core-deep/",
                "Deep entry with a long description here.",
            ),
            entry(
                "engine/crates/bebok-server-deep/",
                "Another deep entry with a long description.",
            ),
            entry("client/", "Angular client."),
        ];
        let full: usize = entries
            .iter()
            .map(|e| estimate_tokens(&render_line(e)))
            .sum();
        let cut = truncate_to_budget(&entries, full - 5);
        // Root-level entries survive, a deep one does not.
        assert!(cut.iter().any(|e| e.path == "engine/"));
        assert!(cut.iter().any(|e| e.path == "client/"));
        assert!(
            cut.iter()
                .any(|e| e.description.contains("more entries omitted"))
        );
        assert!(cut.len() < entries.len());
        // The removed one is a deepest entry, never the top-level view.
        assert!(
            !cut.iter()
                .any(|e| e.path == "engine/crates/bebok-server-deep/"),
            "deepest (then shortest) goes first: {:?}",
            cut
        );

        // Under budget: untouched.
        assert_eq!(truncate_to_budget(&entries, full), entries);

        // Extreme budget still keeps one root-level entry.
        let kept = truncate_to_budget(&entries, 1);
        assert!(
            kept.iter().any(|e| e.path == "engine/"),
            "the first root-level entry is never dropped: {:?}",
            kept
        );
    }

    #[test]
    fn hierarchical_rendering() {
        let entries = vec![
            entry("engine/", "Rust engine."),
            entry("engine/crates/", "Crates."),
            entry("engine/crates/core/", "Core crate."),
            entry("client/", "Angular client."),
        ];
        let text = render_to_prompt(&entries);
        assert!(text.starts_with("Project code map:"));
        assert!(text.contains("\n- engine/ → Rust engine."));
        assert!(text.contains("\n  - engine/crates/ → Crates."));
        assert!(text.contains("\n    - engine/crates/core/ → Core crate."));
        assert!(text.contains("\n- client/ → Angular client."));
    }

    #[test]
    fn stale_cache_detected_after_24h() {
        let mut cache = CodeMapCache {
            version: CACHE_VERSION,
            generated_at: "2020-01-01T00:00:00Z".to_string(),
            root: "/x".to_string(),
            entries: Vec::new(),
        };
        assert!(is_stale(&cache));
        cache.generated_at = chrono::Utc::now().to_rfc3339();
        assert!(!is_stale(&cache));
        cache.generated_at = "not a date".to_string();
        assert!(is_stale(&cache));
    }
}
