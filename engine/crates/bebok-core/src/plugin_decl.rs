//! On-disk plugin declaration contract (TOR B — silnik).
//!
//! A *declared* plugin is a JSON file under `<project>/.bebok/plugins/`:
//!
//! ```json
//! {
//!   "name": "bebok-index",
//!   "repo": "henrykbrzoska/bebok-index",
//!   "url": "https://github.com/henrykbrzoska/bebok-index",
//!   "enabled": true
//! }
//! ```
//!
//! This module only parses, lists and writes those declaration files. It
//! never touches the network and never loads code: the in-process plugin
//! host lives in [`super::plugin`](crate::plugin). The `enabled` flag is a
//! local on/off switch; `installed` (directory/file exists) is derived at
//! read time, never persisted.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// Directory below `.bebok/` holding plugin declarations.
pub const PLUGINS_SUBDIR: [&str; 2] = [".bebok", "plugins"];

/// The only plugin the engine knows how to install: `name` +
/// `owner/repo` pinned here so the server cannot be pointed at an arbitrary
/// URL (no generic network fetch/clone).
pub const KNOWN_PLUGIN_NAME: &str = "bebok-index";
/// `owner/repo` of the known plugin.
pub const KNOWN_PLUGIN_REPO: &str = "henrykbrzoska/bebok-index";
/// Clone URL of the known plugin.
pub const KNOWN_PLUGIN_URL: &str = "https://github.com/henrykbrzoska/bebok-index";

/// On-disk declaration of one plugin (`<project>/.bebok/plugins/<name>.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDecl {
    /// File stem and plugin id (e.g. `"bebok-index"`).
    pub name: String,
    /// `owner/repo` shorthand (e.g. `"henrykbrzoska/bebok-index"`).
    #[serde(default)]
    pub repo: String,
    /// Clone URL (e.g. `"https://github.com/henrykbrzoska/bebok-index"`).
    #[serde(default)]
    pub url: String,
    /// Local on/off switch (default: enabled when absent).
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Optional pre-built archive URL (`https://…`, .zip or .tar.gz).
    #[serde(default)]
    pub asset_url: Option<String>,
    /// Expected SHA-256 hex digest of the archive (64 hex chars).
    #[serde(default)]
    pub asset_sha256: Option<String>,
}

fn default_enabled() -> bool {
    true
}

impl PluginDecl {
    /// Declaration for the known `bebok-index` plugin.
    pub fn bebok_index(enabled: bool) -> Self {
        Self {
            name: KNOWN_PLUGIN_NAME.to_string(),
            repo: KNOWN_PLUGIN_REPO.to_string(),
            url: KNOWN_PLUGIN_URL.to_string(),
            enabled,
            asset_url: None,
            asset_sha256: None,
        }
    }

    /// Validate field values (non-empty name, `owner/repo` repo shape when
    /// present, `http(s)://` URL when present). Called on read and write.
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(CoreError::BadRequest("plugin name is empty".to_string()));
        }
        if self.name.contains(['/', '\\', '.'])
            || self.name.contains("..")
            || self.name.trim() != self.name
        {
            return Err(CoreError::BadRequest(format!(
                "invalid plugin name '{}'",
                self.name
            )));
        }
        if !self.repo.is_empty() {
            let mut parts = self.repo.split('/');
            match (parts.next(), parts.next(), parts.next()) {
                (Some(owner), Some(repo), None) if !owner.is_empty() && !repo.is_empty() => {}
                _ => {
                    return Err(CoreError::BadRequest(format!(
                        "invalid plugin repo '{}' (expected owner/repo)",
                        self.repo
                    )));
                }
            }
        }
        if !self.url.is_empty()
            && !self.url.starts_with("http://")
            && !self.url.starts_with("https://")
        {
            return Err(CoreError::BadRequest(format!(
                "invalid plugin url '{}' (expected http(s)://)",
                self.url
            )));
        }
        Ok(())
    }
}

/// Declared plugin with its derived install state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredPlugin {
    pub name: String,
    pub repo: String,
    pub url: String,
    pub enabled: bool,
    /// True when `<root>/.bebok/plugins/<name>/` exists on disk.
    pub installed: bool,
    /// Version from the slot manifest (`""` when the slot has no manifest).
    /// Plan B: lets `GET /plugins` render `v{version}` and lets the client
    /// decide whether an Update is worthwhile. Old engines never emit it.
    #[serde(default)]
    pub version: String,
    /// Plan B platform-binary state: `"present"` when the slot binary
    /// resolves for this OS, `"missing"` otherwise (missing slot counts as
    /// missing). Old engines never emit it; the client treats absence as
    /// "unknown" (secondary Update styling).
    #[serde(default)]
    pub binary: String,
}

impl DeclaredPlugin {
    /// Build from a declaration + install flag, enriching with the slot
    /// manifest version and the platform-binary presence. Never fails:
    /// unreadable slots degrade to `("", "missing")`.
    pub fn with_state(root: &Path, decl: PluginDecl, installed: bool) -> Self {
        let (version, binary) = slot_state(root, &decl.name, installed);
        Self {
            name: decl.name,
            repo: decl.repo,
            url: decl.url,
            enabled: decl.enabled,
            installed,
            version,
            binary,
        }
    }
}

/// Slot-derived `(version, binary)` for [`DeclaredPlugin::with_state`].
/// `installed == false` short-circuits to `("", "missing")`; any I/O or
/// parse failure degrades the same way (never fatal for a listing).
pub fn slot_state(root: &Path, name: &str, installed: bool) -> (String, String) {
    if !installed {
        return (String::new(), "missing".to_string());
    }
    let slot = install_dir(root, name);
    if !slot.is_dir() {
        return (String::new(), "missing".to_string());
    }
    let version = crate::plugin_registry::read_manifest(&slot)
        .map(|m| m.version)
        .unwrap_or_default();
    let binary = if slot_has_binary(&slot, name) {
        "present"
    } else {
        "missing"
    }
    .to_string();
    (version, binary)
}

/// True when the slot contains an executable for this platform: the
/// manifest's platform entrypoint (`entrypoint_windows` on Windows,
/// `entrypoint_unix` elsewhere, else generic `entrypoint`, else `<name>`),
/// first word only, resolved against the slot dir (plus `.exe` on Windows).
fn slot_has_binary(slot: &Path, name: &str) -> bool {
    let cmd: String = match crate::plugin_registry::read_manifest(slot) {
        Ok(m) => {
            #[cfg(windows)]
            let ep = m.entrypoint_windows.as_deref().or(m.entrypoint.as_deref());
            #[cfg(not(windows))]
            let ep = m.entrypoint_unix.as_deref().or(m.entrypoint.as_deref());
            ep.unwrap_or(name)
                .split_whitespace()
                .next()
                .unwrap_or(name)
                .to_string()
        }
        Err(_) => name.to_string(),
    };
    if Path::new(&cmd).is_absolute() {
        return Path::new(&cmd).exists();
    }
    if slot.join(&cmd).exists() {
        return true;
    }
    #[cfg(windows)]
    if slot.join(format!("{cmd}.exe")).exists() {
        return true;
    }
    false
}

impl From<(PluginDecl, bool)> for DeclaredPlugin {
    fn from((decl, installed): (PluginDecl, bool)) -> Self {
        Self {
            name: decl.name,
            repo: decl.repo,
            url: decl.url,
            enabled: decl.enabled,
            installed,
            version: String::new(),
            binary: "present".to_string(),
        }
    }
}

/// `<root>/.bebok/plugins` (root = project directory).
pub fn plugins_dir(root: &Path) -> PathBuf {
    root.join(PLUGINS_SUBDIR[0]).join(PLUGINS_SUBDIR[1])
}

/// Declaration file path: `<root>/.bebok/plugins/<name>.json`.
pub fn decl_path(root: &Path, name: &str) -> PathBuf {
    plugins_dir(root).join(format!("{name}.json"))
}

/// Install marker: `<root>/.bebok/plugins/<name>/` (a checkout dir).
pub fn install_dir(root: &Path, name: &str) -> PathBuf {
    plugins_dir(root).join(name)
}

/// Parse one declaration file (validates field values).
pub fn read_decl(path: &Path) -> Result<PluginDecl> {
    let text = std::fs::read_to_string(path).map_err(CoreError::Io)?;
    let decl: PluginDecl = serde_json::from_str(&text).map_err(CoreError::Json)?;
    decl.validate()?;
    Ok(decl)
}

/// Write one declaration file (creates `.bebok/plugins/`; validates first).
pub fn write_decl(root: &Path, decl: &PluginDecl) -> Result<PathBuf> {
    decl.validate()?;
    let dir = plugins_dir(root);
    std::fs::create_dir_all(&dir).map_err(CoreError::Io)?;
    let path = decl_path(root, &decl.name);
    let text = serde_json::to_string_pretty(decl).map_err(CoreError::Json)?;
    std::fs::write(&path, format!("{text}\n")).map_err(CoreError::Io)?;
    Ok(path)
}

/// List every declared plugin of a project (sorted by name), each with its
/// derived `installed` flag. Unreadable/invalid files are skipped (logged),
/// never fatal — one broken declaration must not hide the rest.
pub fn list_declared(root: &Path) -> Vec<DeclaredPlugin> {
    let dir = plugins_dir(root);
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&dir).map(|r| {
        r.filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                (path.extension().and_then(|x| x.to_str()) == Some("json")).then_some(path)
            })
            .collect::<Vec<_>>()
    });
    let mut entries = match entries {
        Ok(entries) => entries,
        Err(_) => return out,
    };
    entries.sort();
    for path in entries {
        match read_decl(&path) {
            Ok(decl) => {
                let installed = install_dir(root, &decl.name).exists();
                out.push(DeclaredPlugin::with_state(root, decl, installed));
            }
            Err(e) => {
                tracing::warn!(
                    "skipping invalid plugin declaration {}: {e}",
                    path.display()
                );
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Flip the `enabled` switch of a declared plugin (read-modify-write).
/// Errors with `BadRequest` when the declaration does not exist.
pub fn set_enabled(root: &Path, name: &str, enabled: bool) -> Result<DeclaredPlugin> {
    let path = decl_path(root, name);
    if !path.is_file() {
        return Err(CoreError::BadRequest(format!(
            "unknown plugin '{name}' (no declaration file)"
        )));
    }
    let mut decl = read_decl(&path)?;
    decl.enabled = enabled;
    write_decl(root, &decl)?;
    let installed = install_dir(root, &decl.name).exists();
    Ok(DeclaredPlugin::with_state(root, decl, installed))
}

/// Returns `true` **only** when the declaration file exists, is parseable,
/// and has `enabled == false`.  In every other case (missing file, I/O error,
/// parse error, `enabled == true`) this returns `false` — preserving the
/// existing "assume enabled" behaviour.
pub fn is_disabled(root: &Path, name: &str) -> bool {
    let path = decl_path(root, name);
    match read_decl(&path) {
        Ok(decl) => !decl.enabled,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("bebok-plugin-decl-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn bebok_index_decl_round_trips() {
        let root = temp_root("roundtrip");
        let decl = PluginDecl::bebok_index(true);
        decl.validate().unwrap();
        let path = write_decl(&root, &decl).unwrap();
        assert_eq!(path, decl_path(&root, "bebok-index"));
        let back = read_decl(&path).unwrap();
        assert_eq!(back, decl);

        // The exact contract shape from the brief.
        let text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["name"], "bebok-index");
        assert_eq!(value["repo"], "henrykbrzoska/bebok-index");
        assert_eq!(value["url"], "https://github.com/henrykbrzoska/bebok-index");
        assert_eq!(value["enabled"], true);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn enabled_defaults_to_true_when_absent() {
        let root = temp_root("default");
        std::fs::create_dir_all(plugins_dir(&root)).unwrap();
        std::fs::write(
            decl_path(&root, "bebok-index"),
            r#"{"name":"bebok-index","repo":"henrykbrzoska/bebok-index"}"#,
        )
        .unwrap();
        let decl = read_decl(&decl_path(&root, "bebok-index")).unwrap();
        assert!(decl.enabled);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_names_repos_urls_are_rejected() {
        for bad in ["", "a/b", "..", "x.json", " leading", "trailing "] {
            let decl = PluginDecl {
                name: bad.to_string(),
                repo: String::new(),
                url: String::new(),
                enabled: true,
                asset_url: None,
                asset_sha256: None,
            };
            assert!(decl.validate().is_err(), "{bad:?}");
        }
        let bad_repo = PluginDecl {
            name: "ok".to_string(),
            repo: "no-slash".to_string(),
            url: String::new(),
            enabled: true,
            asset_url: None,
            asset_sha256: None,
        };
        assert!(bad_repo.validate().is_err());
        let bad_url = PluginDecl {
            name: "ok".to_string(),
            repo: String::new(),
            url: "git@github.com:x/y.git".to_string(),
            enabled: true,
            asset_url: None,
            asset_sha256: None,
        };
        assert!(bad_url.validate().is_err());
    }

    #[test]
    fn list_skips_broken_files_and_reports_installed() {
        let root = temp_root("list");
        write_decl(&root, &PluginDecl::bebok_index(true)).unwrap();
        // Broken JSON + wrong shape: skipped, not fatal.
        std::fs::write(decl_path(&root, "broken"), "{not json").unwrap();
        std::fs::write(decl_path(&root, "empty"), "{}").unwrap();

        let listed = list_declared(&root);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "bebok-index");
        assert!(!listed[0].installed);

        // A checkout dir flips `installed` without touching the file.
        std::fs::create_dir_all(install_dir(&root, "bebok-index")).unwrap();
        let listed = list_declared(&root);
        assert!(listed[0].installed);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn set_enabled_flips_the_switch_and_rejects_unknown() {
        let root = temp_root("toggle");
        write_decl(&root, &PluginDecl::bebok_index(true)).unwrap();
        let off = set_enabled(&root, "bebok-index", false).unwrap();
        assert!(!off.enabled);
        assert_eq!(off.name, "bebok-index");
        assert!(!read_decl(&decl_path(&root, "bebok-index")).unwrap().enabled);
        let on = set_enabled(&root, "bebok-index", true).unwrap();
        assert!(on.enabled);
        assert!(set_enabled(&root, "nope", true).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn list_on_missing_dir_is_empty() {
        let root = temp_root("missing");
        assert!(list_declared(&root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn is_disabled_true_when_enabled_false() {
        let root = temp_root("is-disabled-false");
        write_decl(&root, &PluginDecl::bebok_index(false)).unwrap();
        assert!(is_disabled(&root, KNOWN_PLUGIN_NAME));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn is_disabled_false_when_enabled_true() {
        let root = temp_root("is-disabled-true");
        write_decl(&root, &PluginDecl::bebok_index(true)).unwrap();
        assert!(!is_disabled(&root, KNOWN_PLUGIN_NAME));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn is_disabled_false_when_no_declaration() {
        let root = temp_root("is-disabled-none");
        assert!(!is_disabled(&root, KNOWN_PLUGIN_NAME));
        let _ = std::fs::remove_dir_all(&root);
    }
}
