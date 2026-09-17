//! "Code index first" system-prompt section.
//!
//! When the `bebok-index` plugin is available (slot directory exists or
//! declaration `enabled != false`), every assembled prompt gets a section
//! telling the model to query the local code index before falling back to
//! `grep` / `glob`. This is the single enforcement point — user sessions
//! and every delegated sub-agent (task / fleet) flow through
//! [`super::request::apply_request_hook`] which calls [`index_section`].
//!
//! The section text can be customised per-project by placing an
//! `AGENT_INDEX.md` file (or the file named in `bebok-plugin.json`'s
//! `prompt_file` field) inside the plugin slot directory. When the file
//! is absent or unreadable, a built-in fallback with identical semantics
//! is injected.

use std::path::Path;

use crate::plugin_decl;

/// Built-in section text used when the plugin slot exists but no readable
/// `AGENT_INDEX.md` (or custom `prompt_file`) is found.
pub const FALLBACK_SECTION: &str = "\
Code index first — MANDATORY: whenever you need to find code (where is X defined, \
who calls Y, list files matching Z), FIRST use the native tools: \
`code_index_status` (no args needed — uses the session root) to check the index is ready, \
then `code_index_search` (args: `query` and optional `limit`) to search. \
Only when the native tools are unavailable or the status reports not ready, \
fall back to `fetch`: GET /plugins/bebok-index/status?directory=<project-root> \
and POST /plugins/bebok-index/search?directory=<project-root> with JSON body \
{\"query\": \"...\", \"limit\": 5} and header Content-Type: application/json. \
If the fetch fallback replies {\"ok\": false, \"error\": \"query is required and must not be empty\"} \
(the client dropped the JSON body), retry once with the same query as a raw JSON \
string in the `body` parameter with header Content-Type: application/json, \
e.g. body='{\"query\": \"...\", \"limit\": 5}', and reach for \
`read_file` / `grep` / `glob` only when that retry also returns ok:false. \
If the index is available but returns no results for a well-formed query, \
fall back to `read_file` / `grep` / `glob`. \
Never start with grep/glob when the index is available.";

/// Heading grep-able in tests and other modules.
pub const SECTION_HEADING: &str = "Code index first";

/// Return the code-index prompt section, or `None` when the plugin is not
/// available.
///
/// Availability is determined by (in order):
/// 1. The slot directory `<root>/.bebok/plugins/bebok-index/` exists on disk.
/// 2. A declaration file `<root>/.bebok/plugins/bebok-index.json` exists and
///    has `"enabled": true` (an explicit declaration wins over the slot:
///    `enabled: false` means off even when the slot directory exists).
///
/// When available, the function tries to read the prompt file named in the
/// slot manifest's `prompt_file` field (`<slot>/bebok-plugin.json`,
/// falling back to `AGENT_INDEX.md`). If the file is absent or unreadable,
/// the built-in [`FALLBACK_SECTION`] is returned.
pub fn index_section(root: &Path) -> Option<String> {
    let name = plugin_decl::KNOWN_PLUGIN_NAME;
    let slot = plugin_decl::install_dir(root, name);
    let decl_path = plugin_decl::decl_path(root, name);

    let available = if decl_path.is_file() {
        // An explicit declaration wins over the slot. Fail closed on an
        // unreadable/invalid declaration (mirrors `is_disabled`): a corrupt
        // file hides the index instead of silently re-enabling it.
        match plugin_decl::read_decl(&decl_path) {
            Ok(d) if d.enabled => true,
            Ok(_) => return None,
            Err(e) => {
                tracing::warn!(
                    "hiding code index: unreadable declaration {}: {e}",
                    decl_path.display()
                );
                return None;
            }
        }
    } else {
        slot.is_dir()
    };

    if !available {
        return None;
    }

    // Resolve prompt file name from the slot manifest (fall back to AGENT_INDEX.md).
    // The manifest's `prompt_file` is sanitized (plain file name, no
    // traversal): anything suspicious falls back to the default.
    let prompt_name = read_prompt_file_name(&slot.join("bebok-plugin.json"))
        .unwrap_or_else(|| "AGENT_INDEX.md".to_string());

    let prompt_path = slot.join(&prompt_name);
    let text = std::fs::read_to_string(&prompt_path)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| FALLBACK_SECTION.to_string());

    let root_display = root.to_string_lossy();
    let text = text.replace("<project-root>", &root_display);

    Some(text)
}

/// Read the `prompt_file` field from the slot's `bebok-plugin.json` (if present),
/// sanitized via [`sanitize_prompt_file`]. Anything unreadable, missing or
/// suspicious returns `None` (the caller falls back to `AGENT_INDEX.md`).
fn read_prompt_file_name(manifest_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest_path).ok()?;
    // The manifest is JSON; try to parse the extra field from the raw value.
    // (The typed `PluginManifest` also carries `prompt_file`, but the raw
    // read keeps working with hand-written manifests that fail validation.)
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let raw = value.get("prompt_file")?.as_str()?;
    sanitize_prompt_file(raw)
}

/// Accept only a plain file name for the plugin's prompt file: reject empty
/// strings, absolute paths, `..` segments and any path separator (both `/`
/// and `\`, so Windows-style traversal fails everywhere). Returns the file
/// name on success, `None` when the value must not be trusted.
pub fn sanitize_prompt_file(raw: &str) -> Option<String> {
    let name = raw.trim();
    if name.is_empty() {
        return None;
    }
    let path = std::path::Path::new(name);
    if path.is_absolute()
        || name.contains("..")
        || name.contains('/')
        || name.contains('\\')
        || path.file_name().is_none_or(|f| f.to_string_lossy() != name)
    {
        tracing::warn!("rejecting suspicious plugin prompt_file '{raw}'");
        return None;
    }
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::preset::{ASK_PROMPT, PLAN_PROMPT};
    use std::fs;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let base =
            std::env::temp_dir().join(format!("bebok-index-prompt-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn plugins_dir(root: &Path) -> std::path::PathBuf {
        root.join(".bebok").join("plugins")
    }

    fn slot_dir(root: &Path) -> std::path::PathBuf {
        plugin_decl::install_dir(root, plugin_decl::KNOWN_PLUGIN_NAME)
    }

    fn decl_path(root: &Path) -> std::path::PathBuf {
        plugin_decl::decl_path(root, plugin_decl::KNOWN_PLUGIN_NAME)
    }

    fn write_decl(root: &Path, content: &str) {
        let dir = plugins_dir(root);
        fs::create_dir_all(&dir).unwrap();
        fs::write(decl_path(root), content).unwrap();
    }

    fn write_slot_file(root: &Path, name: &str, content: &str) {
        let dir = slot_dir(root);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), content).unwrap();
    }

    // --- Tests ---

    #[test]
    fn no_plugin_returns_none() {
        let root = temp_root("no-plugin");
        assert!(index_section(&root).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn slot_dir_without_prompt_file_returns_fallback() {
        let root = temp_root("slot-no-md");
        // Create slot dir (empty) + a declaration with enabled=true.
        write_decl(
            &root,
            r#"{"name":"bebok-index","repo":"a/b","enabled":true}"#,
        );
        // Create slot dir explicitly (even though write_decl didn't).
        fs::create_dir_all(slot_dir(&root)).unwrap();

        let section = index_section(&root).unwrap();
        // The returned section has <project-root> replaced with the actual root path.
        let expected = FALLBACK_SECTION.replace("<project-root>", &root.to_string_lossy());
        assert_eq!(section, expected);
        assert!(
            !section.contains("<project-root>"),
            "output must not contain unresolved <project-root>"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn slot_dir_with_custom_prompt_file() {
        let root = temp_root("custom-prompt");
        write_decl(
            &root,
            r#"{"name":"bebok-index","repo":"a/b","enabled":true}"#,
        );
        // prompt_file lives in the slot manifest, not the declaration.
        write_slot_file(
            &root,
            "bebok-plugin.json",
            r#"{"name":"bebok-index","prompt_file":"CUSTOM.md"}"#,
        );
        write_slot_file(&root, "CUSTOM.md", "Custom index instructions here.");

        let section = index_section(&root).unwrap();
        assert_eq!(section, "Custom index instructions here.");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn slot_dir_with_default_agent_index_md() {
        let root = temp_root("default-md");
        write_decl(
            &root,
            r#"{"name":"bebok-index","repo":"a/b","enabled":true}"#,
        );
        write_slot_file(&root, "AGENT_INDEX.md", "My AGENT_INDEX content.");

        let section = index_section(&root).unwrap();
        assert_eq!(section, "My AGENT_INDEX content.");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn manifest_without_prompt_file_defaults_to_agent_index_md() {
        let root = temp_root("no-prompt-file");
        write_decl(
            &root,
            r#"{"name":"bebok-index","repo":"a/b","enabled":true}"#,
        );
        write_slot_file(&root, "AGENT_INDEX.md", "Default fallback content.");

        let section = index_section(&root).unwrap();
        assert_eq!(section, "Default fallback content.");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn disabled_declaration_returns_none() {
        let root = temp_root("disabled");
        write_decl(
            &root,
            r#"{"name":"bebok-index","repo":"a/b","enabled":false}"#,
        );
        // Even with a slot dir, disabled declaration means not available.
        fs::create_dir_all(slot_dir(&root)).unwrap();

        assert!(index_section(&root).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn slot_dir_exists_without_declaration_returns_available() {
        let root = temp_root("slot-only");
        // No declaration file, but slot dir exists.
        fs::create_dir_all(slot_dir(&root)).unwrap();
        write_slot_file(&root, "AGENT_INDEX.md", "Slot-only content.");

        let section = index_section(&root).unwrap();
        assert_eq!(section, "Slot-only content.");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_prompt_file_falls_back_to_builtin() {
        let root = temp_root("empty-md");
        write_decl(
            &root,
            r#"{"name":"bebok-index","repo":"a/b","enabled":true}"#,
        );
        write_slot_file(&root, "AGENT_INDEX.md", "   \n  \n  ");

        let section = index_section(&root).unwrap();
        let expected = FALLBACK_SECTION.replace("<project-root>", &root.to_string_lossy());
        assert_eq!(section, expected);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fallback_section_contains_mandatory_keyword() {
        assert!(FALLBACK_SECTION.contains("MANDATORY"));
        assert!(FALLBACK_SECTION.contains(SECTION_HEADING));
        // Native tools are mentioned first.
        assert!(FALLBACK_SECTION.contains("code_index_status"));
        assert!(FALLBACK_SECTION.contains("code_index_search"));
        // Fetch fallback URLs still present.
        assert!(FALLBACK_SECTION.contains("/plugins/bebok-index/status"));
        assert!(FALLBACK_SECTION.contains("/plugins/bebok-index/search"));
    }

    #[test]
    fn fallback_section_documents_raw_body_retry() {
        // The exact error the plugin returns when a harness drops the JSON body.
        assert!(FALLBACK_SECTION.contains("query is required and must not be empty"));
        // The retry must re-send the query as a raw JSON string in `body`.
        assert!(FALLBACK_SECTION.contains("retry once"));
        assert!(FALLBACK_SECTION.contains("raw JSON string in the `body` parameter"));
        assert!(FALLBACK_SECTION.contains("Content-Type: application/json"));
        // grep/glob fallback is allowed only after the retry also fails.
        assert!(FALLBACK_SECTION.contains("only when that retry also returns ok:false"));
    }

    #[test]
    fn ask_and_plan_prompts_document_raw_body_retry() {
        for (name, prompt) in [("ask", &ASK_PROMPT), ("plan", &PLAN_PROMPT)] {
            assert!(
                prompt.contains("query is required and must not be empty"),
                "{name} preset must document the dropped-body error"
            );
            assert!(
                prompt.contains("retry once with the query as a raw JSON string"),
                "{name} preset must document the raw-body retry"
            );
            assert!(
                prompt.contains("only when that retry also returns ok:false"),
                "{name} preset must gate grep/glob on the retry failing"
            );
        }
    }

    #[test]
    fn custom_prompt_file_not_in_slot_falls_back() {
        let root = temp_root("missing-custom");
        write_decl(
            &root,
            r#"{"name":"bebok-index","repo":"a/b","enabled":true}"#,
        );
        // Slot manifest points at a custom file that doesn't exist.
        write_slot_file(
            &root,
            "bebok-plugin.json",
            r#"{"name":"bebok-index","prompt_file":"MISSING.md"}"#,
        );
        // Slot exists but the custom file doesn't.
        fs::create_dir_all(slot_dir(&root)).unwrap();

        let section = index_section(&root).unwrap();
        let expected = FALLBACK_SECTION.replace("<project-root>", &root.to_string_lossy());
        assert_eq!(section, expected);
        let _ = fs::remove_dir_all(&root);
    }

    // --- New tests for <project-root> substitution ---

    #[test]
    fn index_section_substitutes_project_root_in_fallback() {
        let root = temp_root("substitution");
        write_decl(
            &root,
            r#"{"name":"bebok-index","repo":"a/b","enabled":true}"#,
        );
        fs::create_dir_all(slot_dir(&root)).unwrap();

        let section = index_section(&root).unwrap();
        // No unresolved placeholder remains.
        assert!(
            !section.contains("<project-root>"),
            "output must not contain unresolved <project-root>"
        );
        // The actual root path must appear in the fetch fallback URLs.
        let root_str = root.to_string_lossy();
        assert!(
            section.contains(&*root_str),
            "output must contain the concrete root path"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn index_section_substitutes_project_root_in_custom_file() {
        let root = temp_root("sub-custom");
        write_decl(
            &root,
            r#"{"name":"bebok-index","repo":"a/b","enabled":true}"#,
        );
        write_slot_file(&root, "AGENT_INDEX.md", "Search in <project-root> please.");

        let section = index_section(&root).unwrap();
        let root_str = root.to_string_lossy();
        assert_eq!(section, format!("Search in {root_str} please."));
        assert!(!section.contains("<project-root>"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fallback_section_mentions_native_tools() {
        assert!(
            FALLBACK_SECTION.contains("code_index_status"),
            "FALLBACK_SECTION must mention native code_index_status tool"
        );
        assert!(
            FALLBACK_SECTION.contains("code_index_search"),
            "FALLBACK_SECTION must mention native code_index_search tool"
        );
        assert!(
            FALLBACK_SECTION.contains("no args needed"),
            "FALLBACK_SECTION must note native tools need no directory arg"
        );
    }

    #[test]
    fn sanitize_prompt_file_accepts_plain_names() {
        assert_eq!(
            sanitize_prompt_file("AGENT_INDEX.md").as_deref(),
            Some("AGENT_INDEX.md")
        );
        assert_eq!(
            sanitize_prompt_file("  CUSTOM.md  ").as_deref(),
            Some("CUSTOM.md")
        );
    }

    #[test]
    fn sanitize_prompt_file_rejects_traversal() {
        for evil in [
            "",
            "   ",
            "../evil.md",
            "sub/dir.md",
            "sub\\dir.md",
            "..\\evil.md",
            "/etc/passwd",
            "C:\\Windows\\x.md",
            "....//x",
        ] {
            assert_eq!(sanitize_prompt_file(evil), None, "must reject '{evil}'");
        }
    }

    #[test]
    fn malicious_prompt_file_falls_back_to_default() {
        let root = temp_root("traversal");
        write_decl(
            &root,
            r#"{"name":"bebok-index","repo":"a/b","enabled":true}"#,
        );
        // Traversal attempt in the slot manifest must be ignored…
        write_slot_file(
            &root,
            "bebok-plugin.json",
            r#"{"name":"bebok-index","prompt_file":"../../evil.md"}"#,
        );
        // …so the default file is used instead.
        write_slot_file(&root, "AGENT_INDEX.md", "Default wins.");

        let section = index_section(&root).unwrap();
        assert_eq!(section, "Default wins.");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupt_declaration_hides_index() {
        let root = temp_root("corrupt-decl");
        write_decl(&root, "{not valid json");
        fs::create_dir_all(slot_dir(&root)).unwrap();

        // Fail closed (mirrors is_disabled): a corrupt file hides the index.
        assert!(index_section(&root).is_none());
        let _ = fs::remove_dir_all(&root);
    }
}
