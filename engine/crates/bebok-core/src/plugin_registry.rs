//! Central plugin registry (remote catalogue of installable plugins).
//!
//! The registry is a single JSON file (`plugins.json`) hosted in the public
//! repo [`REGISTRY_REPO`], fetched over HTTPS and cached under the engine
//! data dir with a TTL ([`REGISTRY_TTL`]). It is the **only** source of
//! installable plugins: [`InstanceStore::install_plugin`] accepts a name
//! only when the registry lists it (no arbitrary URLs, only public repos).
//!
//! Registry file shape:
//!
//! ```json
//! { "plugins": [{ "name": "...", "repo": "owner/repo",
//!                 "url": "https://github.com/owner/repo",
//!                 "description": "..." }] }
//! ```
//!
//! Every plugin repo must carry a `bebok-plugin.json` manifest in its root
//! (`name`, `version`, `min_engine_version`, `description`); the installer
//! validates it after cloning. Install pins the **latest tag**
//! (`git describe --tags`, newest first) — never a floating branch.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// `owner/repo` of the central registry repo.
pub const REGISTRY_REPO: &str = "henrykbrzoska/bebok-plugins";
/// Raw URL of the registry file (`plugins.json` on `main`).
pub const REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/henrykbrzoska/bebok-plugins/main/plugins.json";
/// Cache file name under the engine data dir.
pub const REGISTRY_CACHE_FILE: &str = "plugin-registry.json";
/// Cache freshness: refetch when older than this.
pub const REGISTRY_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
/// Network timeout for the registry fetch.
pub const REGISTRY_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// `bebok-plugin.json` manifest file name (repo root of every plugin).
pub const PLUGIN_MANIFEST_FILE: &str = "bebok-plugin.json";

/// One registry entry (an installable plugin).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryPlugin {
    /// Plugin id (also the slot dir name, e.g. `"bebok-index"`).
    pub name: String,
    /// `owner/repo` shorthand.
    #[serde(default)]
    pub repo: String,
    /// Clone URL (`https://github.com/owner/repo`).
    #[serde(default)]
    pub url: String,
    /// Human-readable blurb for the Settings list.
    #[serde(default)]
    pub description: String,
}

/// The registry file (`plugins.json`): `{ "plugins": [...] }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRegistryFile {
    #[serde(default)]
    pub plugins: Vec<RegistryPlugin>,
}

impl PluginRegistryFile {
    /// Find an entry by plugin name.
    pub fn find(&self, name: &str) -> Option<&RegistryPlugin> {
        self.plugins.iter().find(|p| p.name == name)
    }

    /// Validate entries: non-empty names, `owner/repo` shape, `https://`
    /// URLs. Invalid entries are dropped by [`PluginRegistryFile::sane`];
    /// this strict variant errors instead (used in tests).
    pub fn validate(&self) -> Result<()> {
        for p in &self.plugins {
            if p.name.trim().is_empty()
                || p.name.contains(['/', '\\', '.'])
                || p.name.contains("..")
                || p.name.trim() != p.name
            {
                return Err(CoreError::BadRequest(format!(
                    "invalid registry plugin name '{}'",
                    p.name
                )));
            }
            if !p.url.starts_with("https://") {
                return Err(CoreError::BadRequest(format!(
                    "invalid registry plugin url '{}' (expected https://)",
                    p.url
                )));
            }
        }
        Ok(())
    }

    /// Lenient variant: keep only entries that pass [`Self::validate`]
    /// field checks (single-entry granularity). One bad entry must not hide
    /// the rest of the catalogue.
    pub fn sane(mut self) -> Self {
        self.plugins.retain(|p| {
            let single = Self {
                plugins: vec![p.clone()],
            };
            single.validate().is_ok()
        });
        self
    }
}

/// The plugin's own manifest (`bebok-plugin.json` in the plugin repo root).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Must match the registry entry name and the slot dir.
    pub name: String,
    /// Release version (matches the installed tag, e.g. `"1.6.0"`).
    #[serde(default)]
    pub version: String,
    /// Minimum engine version that can host this plugin (semver-ish).
    #[serde(default)]
    pub min_engine_version: String,
    /// Human-readable blurb.
    #[serde(default)]
    pub description: String,
}

impl PluginManifest {
    /// Validate: non-empty name + a parseable `min_engine_version` when
    /// present (`major.minor.patch`, numbers only).
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(CoreError::BadRequest(
                "plugin manifest has an empty name".to_string(),
            ));
        }
        if !self.min_engine_version.is_empty() {
            let parts: Vec<&str> = self.min_engine_version.split('.').collect();
            if parts.len() != 3
                || parts
                    .iter()
                    .any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()))
            {
                return Err(CoreError::BadRequest(format!(
                    "invalid min_engine_version '{}' (expected major.minor.patch)",
                    self.min_engine_version
                )));
            }
        }
        Ok(())
    }

    /// True when `engine_version` (same `major.minor.patch` shape) satisfies
    /// `min_engine_version`. Unparseable engine versions fail closed.
    pub fn engine_compatible(&self, engine_version: &str) -> bool {
        if self.min_engine_version.is_empty() {
            return true;
        }
        let parse = |v: &str| -> Option<(u64, u64, u64)> {
            let mut it = v.split('.');
            let maj = it.next()?.parse().ok()?;
            let min = it.next()?.parse().ok()?;
            let pat = it.next()?.parse().ok()?;
            if it.next().is_some() {
                return None;
            }
            Some((maj, min, pat))
        };
        match (parse(engine_version), parse(&self.min_engine_version)) {
            (Some(have), Some(need)) => have >= need,
            _ => false,
        }
    }
}

/// Read + validate the manifest from a plugin checkout dir.
pub fn read_manifest(dir: &Path) -> Result<PluginManifest> {
    let path = dir.join(PLUGIN_MANIFEST_FILE);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        CoreError::BadRequest(format!("plugin manifest missing ({}): {e}", path.display()))
    })?;
    let manifest: PluginManifest = serde_json::from_str(&text)
        .map_err(|e| CoreError::BadRequest(format!("invalid plugin manifest: {e}")))?;
    manifest.validate()?;
    Ok(manifest)
}

/// Cache file path for the registry (`<data_dir>/plugin-registry.json`).
pub fn registry_cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join(REGISTRY_CACHE_FILE)
}

/// Load the cached registry file. `None` when missing/unparseable (a corrupt
/// cache must not hide the catalogue — the caller refetches or falls back).
pub fn read_cached_registry(data_dir: &Path) -> Option<PluginRegistryFile> {
    let text = std::fs::read_to_string(registry_cache_path(data_dir)).ok()?;
    let file: PluginRegistryFile = serde_json::from_str(&text).ok()?;
    Some(file.sane())
}

/// True when the cache file exists and is younger than [`REGISTRY_TTL`].
pub fn cache_is_fresh(data_dir: &Path) -> bool {
    std::fs::metadata(registry_cache_path(data_dir))
        .and_then(|m| m.modified())
        .map(|t| t.elapsed().map(|age| age < REGISTRY_TTL).unwrap_or(false))
        .unwrap_or(false)
}

/// Persist the registry file to the cache (atomic tmp+rename).
pub fn write_cached_registry(data_dir: &Path, registry: &PluginRegistryFile) -> Result<()> {
    std::fs::create_dir_all(data_dir).map_err(CoreError::Io)?;
    let path = registry_cache_path(data_dir);
    let tmp = path.with_extension("tmp");
    let text = serde_json::to_string_pretty(registry).map_err(CoreError::Json)?;
    std::fs::write(&tmp, format!("{text}\n")).map_err(CoreError::Io)?;
    std::fs::rename(&tmp, &path).map_err(CoreError::Io)?;
    Ok(())
}

/// Fetch the registry file over HTTPS (timeout, no auth — public repos
/// only). Returns the sanitized catalogue.
pub async fn fetch_registry(url: &str) -> Result<PluginRegistryFile> {
    let client = reqwest::Client::builder()
        .timeout(REGISTRY_FETCH_TIMEOUT)
        .build()
        .map_err(|e| CoreError::BadRequest(format!("registry client: {e}")))?;
    let resp = client
        .get(url)
        .header("Accept", "application/json")
        .header("User-Agent", "bebok-engine")
        .send()
        .await
        .map_err(|e| CoreError::BadRequest(format!("registry fetch failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(CoreError::BadRequest(format!(
            "registry fetch failed: HTTP {}",
            resp.status()
        )));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| CoreError::BadRequest(format!("registry read failed: {e}")))?;
    let file: PluginRegistryFile = serde_json::from_str(&text)
        .map_err(|e| CoreError::BadRequest(format!("invalid registry file: {e}")))?;
    Ok(file.sane())
}

/// Load the catalogue: fresh cache wins; otherwise fetch, persist the cache
/// (best-effort — a read-only data dir still yields the fetched list), and
/// fall back to a stale cache when the network fails. `Err` only when there
/// is neither cache nor network (the caller may fall back to the bundled
/// `bebok-index` entry).
pub async fn load_registry(data_dir: &Path, url: &str) -> Result<PluginRegistryFile> {
    if cache_is_fresh(data_dir)
        && let Some(cached) = read_cached_registry(data_dir)
    {
        return Ok(cached);
    }
    match fetch_registry(url).await {
        Ok(fresh) => {
            let _ = write_cached_registry(data_dir, &fresh);
            Ok(fresh)
        }
        Err(fetch_err) => match read_cached_registry(data_dir) {
            Some(stale) => {
                tracing::warn!("registry fetch failed, using stale cache: {fetch_err}");
                Ok(stale)
            }
            None => Err(fetch_err),
        },
    }
}

/// Bundled fallback: the catalogue contains just `bebok-index` (used when
/// offline with an empty cache, so the first plugin keeps working).
pub fn fallback_registry() -> PluginRegistryFile {
    PluginRegistryFile {
        plugins: vec![RegistryPlugin {
            name: crate::plugin_decl::KNOWN_PLUGIN_NAME.to_string(),
            repo: crate::plugin_decl::KNOWN_PLUGIN_REPO.to_string(),
            url: crate::plugin_decl::KNOWN_PLUGIN_URL.to_string(),
            description: "Per-instance code index for Bebok.".to_string(),
        }],
    }
}

/// Catalogue with availability: `load_registry`, falling back to
/// [`fallback_registry`] when offline with an empty cache. Never fails for
/// network reasons — only for truly unusable inputs.
pub async fn load_registry_or_fallback(data_dir: &Path, url: &str) -> PluginRegistryFile {
    match load_registry(data_dir, url).await {
        Ok(reg) => reg,
        Err(e) => {
            tracing::warn!("plugin registry unavailable ({e}); using bundled fallback");
            fallback_registry()
        }
    }
}

/// Latest tag of a clone (newest version-sorted tag, `v`-prefix tolerated).
/// `None` when the clone has no tags (caller clones the default branch) or
/// git is unavailable.
pub async fn latest_tag(clone_dir: &Path) -> Option<String> {
    let out = crate::git::run(clone_dir, &["tag", "--list", "--sort=-v:refname"]).await?;
    if !out.success {
        return None;
    }
    out.stdout
        .lines()
        .map(str::trim)
        .find(|t| !t.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn git_ok(cwd: &Path, args: &[&str]) {
        let out = crate::git::run(cwd, args).await.expect("git spawns");
        assert!(out.success, "git {args:?} failed: {}", out.stderr);
    }

    async fn init_repo_with_tags(dir: &Path) {
        git_ok(dir, &["init", "-q"]).await;
        git_ok(dir, &["symbolic-ref", "HEAD", "refs/heads/main"]).await;
        git_ok(dir, &["config", "user.email", "bebok@example.com"]).await;
        git_ok(dir, &["config", "user.name", "Bebok Test"]).await;
        git_ok(dir, &["config", "commit.gpgsign", "false"]).await;
        std::fs::write(dir.join("README.md"), "hello\n").unwrap();
        git_ok(dir, &["add", "README.md"]).await;
        git_ok(dir, &["commit", "-q", "-m", "init"]).await;
    }

    /// `latest_tag` (used by the install path to pin the checkout): newest
    /// version-sorted tag wins; no tags -> `None`; non-repo -> `None`.
    #[tokio::test]
    async fn latest_tag_picks_newest_tag_and_none_without_tags() {
        if !crate::git::git_available().await {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let base = std::env::temp_dir().join(format!("bebok-latest-tag-{}", uuid::Uuid::new_v4()));
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_repo_with_tags(&repo).await;
        assert_eq!(latest_tag(&repo).await, None);

        for tag in ["v1.5.0", "v1.6.0", "v1.5.9"] {
            git_ok(&repo, &["tag", tag]).await;
        }
        assert_eq!(latest_tag(&repo).await.as_deref(), Some("v1.6.0"));

        // Non-repo dir -> None, never an error.
        let plain = base.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(latest_tag(&plain).await, None);

        let _ = std::fs::remove_dir_all(&base);
    }

    fn sample_registry() -> PluginRegistryFile {
        PluginRegistryFile {
            plugins: vec![
                RegistryPlugin {
                    name: "bebok-index".to_string(),
                    repo: "henrykbrzoska/bebok-index".to_string(),
                    url: "https://github.com/henrykbrzoska/bebok-index".to_string(),
                    description: "Code index.".to_string(),
                },
                RegistryPlugin {
                    name: "bad name!".to_string(),
                    repo: String::new(),
                    url: "ftp://example.com/x".to_string(),
                    description: String::new(),
                },
            ],
        }
    }

    #[test]
    fn sane_drops_invalid_entries_but_keeps_good_ones() {
        let sane = sample_registry().sane();
        assert_eq!(sane.plugins.len(), 1);
        assert_eq!(sane.plugins[0].name, "bebok-index");
        assert!(sample_registry().validate().is_err());
    }

    #[test]
    fn find_returns_entry_by_name() {
        let reg = sample_registry().sane();
        assert!(reg.find("bebok-index").is_some());
        assert!(reg.find("nope").is_none());
    }

    #[test]
    fn registry_cache_round_trips() {
        let base = std::env::temp_dir().join(format!("bebok-registry-{}", uuid::Uuid::new_v4()));
        let reg = sample_registry().sane();
        write_cached_registry(&base, &reg).unwrap();
        assert!(cache_is_fresh(&base));
        let back = read_cached_registry(&base).unwrap();
        assert_eq!(back, reg);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn corrupt_cache_is_none_not_fatal() {
        let base =
            std::env::temp_dir().join(format!("bebok-registry-bad-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(registry_cache_path(&base), "{not json").unwrap();
        assert!(read_cached_registry(&base).is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn manifest_validation_and_engine_compat() {
        let ok = PluginManifest {
            name: "bebok-index".to_string(),
            version: "1.6.0".to_string(),
            min_engine_version: "1.5.0".to_string(),
            description: "x".to_string(),
        };
        ok.validate().unwrap();
        assert!(ok.engine_compatible("1.6.0"));
        assert!(ok.engine_compatible("1.5.0"));
        assert!(!ok.engine_compatible("1.4.9"));
        assert!(!ok.engine_compatible("garbage"));

        let bad = PluginManifest {
            name: String::new(),
            version: String::new(),
            min_engine_version: String::new(),
            description: String::new(),
        };
        assert!(bad.validate().is_err());

        let bad_ver = PluginManifest {
            name: "x".to_string(),
            version: String::new(),
            min_engine_version: "1.5".to_string(),
            description: String::new(),
        };
        assert!(bad_ver.validate().is_err());

        let no_min = PluginManifest {
            name: "x".to_string(),
            version: String::new(),
            min_engine_version: String::new(),
            description: String::new(),
        };
        assert!(no_min.engine_compatible("0.0.1"));
    }

    #[test]
    fn fallback_registry_contains_bebok_index() {
        let reg = fallback_registry();
        let entry = reg.find("bebok-index").expect("fallback has bebok-index");
        assert_eq!(entry.repo, "henrykbrzoska/bebok-index");
    }
}
