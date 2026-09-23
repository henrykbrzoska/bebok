//! "Project code map" system-prompt section.
//!
//! Answers "what is where" for the model up front: instead of a
//! `list_dir` / `tree` / `glob` walk at the start of every conversation, the
//! engine injects a pre-computed, hierarchical directory -> one-sentence
//! description map (max `code_map.max_depth` levels deep) trimmed to
//! `code_map.max_tokens` tokens.
//!
//! Off by default (`code_map.enabled = false` returns `None` on the first
//! line — zero I/O, zero prompt tokens). When on, the section reads the
//! `<project>/.bebok/code-map.json` cache; a missing or stale (> 24 h) cache
//! is regenerated on the fly and written back (lazy refresh — option A of
//! the plan; an `after.file_write` rescan plugin is a possible follow-up).
//!
//! Kept in its own file so the prompt assemblers (`services/turn.rs`,
//! `agent/task_tool.rs`, `agent/fleet_tool.rs`) each add exactly one line
//! and concurrent work on those assemblers stays conflict-free — the same
//! shape as `verify_prompt.rs` / `build_test_prompt.rs`.

use std::path::Path;

use crate::config::ResolvedConfig;

use super::code_map_gen::{
    self, CodeMapCache, apply_overrides, render_to_prompt, truncate_to_budget,
};

/// The heading every renderer starts with (tests and readers grep for it);
/// defined once in [`code_map_gen`] where the renderer lives.
pub use super::code_map_gen::SECTION_HEADING;

/// The section for the resolved config and project root, or `None` when the
/// feature is off, the root is not a directory, or generation failed.
pub fn code_map_section(root: &Path, cfg: &ResolvedConfig) -> Option<String> {
    // 1. Toggle: disabled -> None, no work at all (no I/O, no cache write).
    if !cfg.code_map.enabled {
        return None;
    }

    let cache_file = code_map_gen::cache_path(root);

    // 2. Cached map, else 3. generate on the fly (lazy: first conversation).
    let cache = match code_map_gen::read_cache(&cache_file) {
        Some(c) if !code_map_gen::is_stale(&c) => c,
        _ => {
            let cache = code_map_gen::generate_code_map(root, &cfg.code_map)?;
            if let Err(e) = code_map_gen::write_cache(&cache_file, &cache) {
                // A read-only project still gets a (transient) section.
                tracing::debug!("code map cache not written: {e}");
            }
            cache
        }
    };

    // Config overrides win over the cached descriptions too, so a config
    // edit lands without waiting for the 24 h staleness window.
    Some(render_section(
        &cache,
        &cfg.code_map.overrides,
        cfg.code_map.max_tokens,
        root,
    ))
}

/// Render one cache with the *current* config applied (overrides win over
/// the cached descriptions too, so a config edit lands without waiting for
/// the 24 h staleness window) and `<project-root>` substituted.
fn render_section(
    cache: &CodeMapCache,
    overrides: &std::collections::HashMap<String, String>,
    max_tokens: usize,
    root: &Path,
) -> String {
    let mut entries = cache.entries.clone();
    apply_overrides(&mut entries, overrides);
    let entries = truncate_to_budget(&entries, max_tokens);
    let text = render_to_prompt(&entries);
    let root_display = root.to_string_lossy();
    text.replace("<project-root>", &root_display)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::code_map_gen::{CACHE_VERSION, CodeMapEntry};
    use crate::config::ResolvedConfig;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let base =
            std::env::temp_dir().join(format!("bebok-code-map-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn enabled_cfg() -> ResolvedConfig {
        let mut cfg = ResolvedConfig::default();
        cfg.code_map.enabled = true;
        cfg
    }

    fn write_file(path: &std::path::Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn disabled_returns_none() {
        let root = temp_root("disabled");
        write_file(&root.join("src/main.rs"), "fn main() {}");
        // Default config (enabled = false).
        assert!(code_map_section(&root, &ResolvedConfig::default()).is_none());
        let mut cfg = enabled_cfg();
        cfg.code_map.enabled = false;
        assert!(code_map_section(&root, &cfg).is_none());
        // Zero I/O: no cache file was created while disabled.
        assert!(!code_map_gen::cache_path(&root).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn enabled_without_cache_generates_on_the_fly() {
        let root = temp_root("lazy");
        write_file(&root.join("engine/lib.rs"), "//! Rust engine.");
        write_file(
            &root.join("client/package.json"),
            r#"{ "description": "Angular client" }"#,
        );

        let section = code_map_section(&root, &enabled_cfg()).unwrap();
        assert!(section.starts_with("Project code map:"), "{section}");
        assert!(section.contains("\n- engine/ → Rust engine."), "{section}");
        assert!(
            section.contains("\n- client/ → Angular client"),
            "{section}"
        );
        // The cache was written back for the next conversation.
        assert!(code_map_gen::cache_path(&root).is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn enabled_with_cache_reads_cache() {
        let root = temp_root("cache");
        let cache = CodeMapCache {
            version: CACHE_VERSION,
            generated_at: chrono::Utc::now().to_rfc3339(),
            root: root.to_string_lossy().into_owned(),
            entries: vec![CodeMapEntry {
                path: "cached/".into(),
                description: "From the cache.".into(),
            }],
        };
        code_map_gen::write_cache(&code_map_gen::cache_path(&root), &cache).unwrap();

        let section = code_map_section(&root, &enabled_cfg()).unwrap();
        assert!(section.contains("- cached/ → From the cache."), "{section}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn max_tokens_truncates_entries() {
        let root = temp_root("budget");
        for i in 0..40 {
            write_file(&root.join(format!("a/b/dir{i:02}/file.txt")), "x");
        }
        let mut cfg = enabled_cfg();
        cfg.code_map.max_tokens = 100;
        let section = code_map_section(&root, &cfg).unwrap();
        assert!(section.contains("more entries omitted"), "{section}");
        // Root-level entry always survives.
        assert!(section.contains("\n- a/ → "), "{section}");
        let full_chars = section.chars().count();
        assert!(
            full_chars < 4000,
            "map must respect the budget: {full_chars}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn overrides_replace_auto_descriptions() {
        let root = temp_root("overrides");
        write_file(&root.join("engine/f.txt"), "x");
        let mut cfg = enabled_cfg();
        cfg.code_map
            .overrides
            .insert("engine".into(), "Core engine: config, sessions".into());
        let section = code_map_section(&root, &cfg).unwrap();
        assert!(
            section.contains("\n- engine/ → Core engine: config, sessions"),
            "{section}"
        );
        assert!(
            !section.contains("Engine"),
            "auto description replaced: {section}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn project_root_substitution() {
        let root = temp_root("substitution");
        let cache = CodeMapCache {
            version: CACHE_VERSION,
            generated_at: chrono::Utc::now().to_rfc3339(),
            root: root.to_string_lossy().into_owned(),
            entries: vec![CodeMapEntry {
                path: "x/".into(),
                description: "Lives under <project-root>/x".into(),
            }],
        };
        code_map_gen::write_cache(&code_map_gen::cache_path(&root), &cache).unwrap();

        let section = code_map_section(&root, &enabled_cfg()).unwrap();
        assert!(
            !section.contains("<project-root>"),
            "placeholder resolved: {section}"
        );
        assert!(section.contains(&root.to_string_lossy().to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn section_heading_present() {
        let root = temp_root("heading");
        write_file(&root.join("src/f.txt"), "x");
        let section = code_map_section(&root, &enabled_cfg()).unwrap();
        assert!(section.starts_with("Project code map:"), "{section}");
        assert_eq!(
            SECTION_HEADING,
            crate::agent::code_map_gen::SECTION_HEADING,
            "one heading constant, re-exported"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_project_returns_empty_map() {
        let root = temp_root("empty");
        // Minimal map: heading only, no crash, no bogus entries.
        let section = code_map_section(&root, &enabled_cfg()).unwrap();
        assert_eq!(section, "Project code map:");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ignores_git_node_modules_target() {
        let root = temp_root("skip");
        for dir in [".git", "node_modules", "target"] {
            write_file(&root.join(dir).join("junk.txt"), "x");
        }
        write_file(&root.join("src/f.txt"), "x");

        let section = code_map_section(&root, &enabled_cfg()).unwrap();
        assert!(!section.contains(".git/"), "{section}");
        assert!(!section.contains("node_modules/"), "{section}");
        assert!(!section.contains("target/"), "{section}");
        assert!(section.contains("- src/ → "), "{section}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hierarchical_rendering() {
        let root = temp_root("hierarchy");
        write_file(&root.join("engine/crates/core/lib.rs"), "//! Core.");
        write_file(&root.join("engine/crates/server/main.rs"), "fn main() {}");
        write_file(&root.join("client/app.ts"), "export {};");

        let section = code_map_section(&root, &enabled_cfg()).unwrap();
        let engine_line = section
            .lines()
            .find(|l| l.ends_with("- engine/ → Engine"))
            .unwrap_or_else(|| panic!("{section}"));
        let core_line = section
            .lines()
            .find(|l| l.contains("engine/crates/core/"))
            .unwrap_or_else(|| panic!("{section}"));
        let server_line = section
            .lines()
            .find(|l| l.contains("engine/crates/server/"))
            .unwrap_or_else(|| panic!("{section}"));
        let client_line = section
            .lines()
            .find(|l| l.ends_with("client/ → Client"))
            .unwrap_or_else(|| panic!("{section}"));
        // Children are indented relative to their parent, siblings share it.
        assert!(engine_line.starts_with("- "), "{engine_line}");
        assert!(core_line.starts_with("    - "), "{core_line}");
        assert!(server_line.starts_with("    - "), "{server_line}");
        assert!(client_line.starts_with("- "), "{client_line}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_root_returns_none() {
        let root = temp_root("gone");
        let missing = root.join("does-not-exist");
        assert!(code_map_section(&missing, &enabled_cfg()).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
