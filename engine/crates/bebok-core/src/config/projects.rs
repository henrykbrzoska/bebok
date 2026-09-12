//! Projects registry (F5-1): the `"projects"` top-level key of the global
//! config file (`~/.config/bebok/config.json`).
//!
//! This is **not** part of `ResolvedConfig` and does not take part in the
//! defaults -> global -> project merge: it is sibling infrastructure that
//! happens to live in the same file. Reads go through
//! [`loader::read_layer_json`] and writes through [`writer::write_delta_to`],
//! so unrelated top-level keys (and their JSONC comments) survive untouched -
//! `write_full_*` would delete them and must not be used here.
//!
//! Every stored `path` has been through [`crate::util::normalize_path`], which
//! canonicalises and strips the Windows extended-length `\\?\` prefix, so the
//! registry is the canonical source for the path handed to `/session`,
//! `/fs/tree` and the tool layer afterwards.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::loader::{global_config_path, read_layer_json};
use super::writer::write_delta_to;
use crate::util::{normalize_path, now_ms};

/// One registered project directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub id: String,
    pub name: String,
    /// Absolute, normalised filesystem path (see [`crate::util::normalize_path`]).
    pub path: String,
    pub added_at: i64,
    #[serde(default)]
    pub last_opened_at: Option<i64>,
    #[serde(default)]
    pub pinned: bool,
}

/// Registry failure, mapped to HTTP status codes by the route layer.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectsError {
    /// Bad input (missing/blank path, path does not exist or is not a
    /// directory) -> 400.
    Invalid(String),
    /// Unknown project id -> 404.
    NotFound,
    /// Could not persist the registry -> 500.
    Io(String),
}

impl std::fmt::Display for ProjectsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) => write!(f, "{m}"),
            Self::NotFound => write!(f, "unknown project"),
            Self::Io(m) => write!(f, "{m}"),
        }
    }
}

/// Partial update for [`patch_in`] (both fields optional).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProjectPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub pinned: Option<bool>,
}

// ---------------------------------------------------------------------------
// load / save
// ---------------------------------------------------------------------------

/// Read the registry from an explicit config file. A missing file, an
/// unparsable file or a missing/!array `"projects"` key all yield an empty
/// list - the registry never fails a read, because an unreadable global config
/// must not make the picker unusable.
pub fn load_from(config_path: &Path) -> Vec<ProjectEntry> {
    let Some(value) = read_layer_json(config_path) else {
        return Vec::new();
    };
    let Some(items) = value.get("projects").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| serde_json::from_value::<ProjectEntry>(item.clone()).ok())
        .collect()
}

/// Persist the registry to an explicit config file (delta write: only the
/// `"projects"` key is replaced).
pub fn save_to(config_path: &Path, projects: &[ProjectEntry]) -> Result<(), ProjectsError> {
    let array = serde_json::to_value(projects).map_err(|e| ProjectsError::Io(e.to_string()))?;
    write_delta_to(config_path, &json!({ "projects": array })).map_err(ProjectsError::Io)
}

/// Read the registry from the global config file.
pub fn load() -> Vec<ProjectEntry> {
    load_from(&global_config_path())
}

/// Persist the registry to the global config file.
pub fn save(projects: &[ProjectEntry]) -> Result<(), ProjectsError> {
    save_to(&global_config_path(), projects)
}

// ---------------------------------------------------------------------------
// queries
// ---------------------------------------------------------------------------

/// Registry display order: pinned first, then most recently opened (entries
/// that were never opened come last), then by name (case-insensitive).
pub fn sort_entries(projects: &mut [ProjectEntry]) {
    projects.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.last_opened_at.cmp(&a.last_opened_at))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// The registry in display order.
pub fn list_from(config_path: &Path) -> Vec<ProjectEntry> {
    let mut projects = load_from(config_path);
    sort_entries(&mut projects);
    projects
}

// ---------------------------------------------------------------------------
// mutations
// ---------------------------------------------------------------------------

/// Add a project. The path must exist and be a directory; it is normalised
/// before comparison, so two spellings of the same location (relative vs
/// absolute, `\\?\` prefixed vs not, differing case on Windows) collapse into
/// one entry.
///
/// Returns `(entry, created)` - `created == false` means an entry for the same
/// canonical path already existed and is returned unchanged (the caller
/// answers 200 instead of 201).
pub fn add_in(
    config_path: &Path,
    raw_path: &str,
    name: Option<String>,
) -> Result<(ProjectEntry, bool), ProjectsError> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Err(ProjectsError::Invalid("path is required".to_string()));
    }
    let candidate = Path::new(trimmed);
    if !std::fs::metadata(candidate).is_ok_and(|m| m.is_dir()) {
        return Err(ProjectsError::Invalid(format!(
            "not a directory: {trimmed}"
        )));
    }
    let normalized = normalize_path(candidate);

    let mut projects = load_from(config_path);
    if let Some(existing) = projects.iter().find(|p| same_path(&p.path, &normalized)) {
        return Ok((existing.clone(), false));
    }

    let entry = ProjectEntry {
        id: uuid::Uuid::new_v4().to_string(),
        name: name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| default_name(&normalized)),
        path: normalized,
        added_at: now_ms(),
        last_opened_at: None,
        pinned: false,
    };
    projects.push(entry.clone());
    save_to(config_path, &projects)?;
    Ok((entry, true))
}

/// Partially update an entry (name and/or pinned). 404 when the id is unknown.
pub fn patch_in(
    config_path: &Path,
    id: &str,
    patch: &ProjectPatch,
) -> Result<ProjectEntry, ProjectsError> {
    let mut projects = load_from(config_path);
    let Some(index) = projects.iter().position(|p| p.id == id) else {
        return Err(ProjectsError::NotFound);
    };
    if let Some(name) = patch.name.as_ref() {
        let name = name.trim();
        if name.is_empty() {
            return Err(ProjectsError::Invalid("name must not be empty".to_string()));
        }
        projects[index].name = name.to_string();
    }
    if let Some(pinned) = patch.pinned {
        projects[index].pinned = pinned;
    }
    let updated = projects[index].clone();
    save_to(config_path, &projects)?;
    Ok(updated)
}

/// Forget a project. This only removes the registry entry - it never touches
/// anything on disk.
pub fn remove_in(config_path: &Path, id: &str) -> Result<(), ProjectsError> {
    let mut projects = load_from(config_path);
    let before = projects.len();
    projects.retain(|p| p.id != id);
    if projects.len() == before {
        return Err(ProjectsError::NotFound);
    }
    save_to(config_path, &projects)
}

/// Mark a project as opened now and return it (the caller switches to the
/// returned, normalised `path`).
pub fn mark_opened_in(config_path: &Path, id: &str) -> Result<ProjectEntry, ProjectsError> {
    let mut projects = load_from(config_path);
    let Some(index) = projects.iter().position(|p| p.id == id) else {
        return Err(ProjectsError::NotFound);
    };
    projects[index].last_opened_at = Some(now_ms());
    let updated = projects[index].clone();
    save_to(config_path, &projects)?;
    Ok(updated)
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Last path component, falling back to the whole path (a drive root such as
/// `C:\` or `/` has no file name).
fn default_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| path.to_string())
}

/// Path equality for dedupe. Both sides are already normalised; Windows paths
/// compare case-insensitively, Unix paths byte-exactly.
fn same_path(a: &str, b: &str) -> bool {
    #[cfg(windows)]
    {
        a.eq_ignore_ascii_case(b)
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway global-config path + project directories, mirroring the
    /// `std::env::temp_dir().join(...)` pattern `config/loader.rs`'s own tests
    /// use (no drive letters or home directory assumed).
    struct Fixture {
        base: std::path::PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let base =
                std::env::temp_dir().join(format!("bebok-projects-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&base).unwrap();
            Self { base }
        }

        fn config(&self) -> std::path::PathBuf {
            self.base.join("config.json")
        }

        fn dir(&self, name: &str) -> std::path::PathBuf {
            let path = self.base.join(name);
            std::fs::create_dir_all(&path).unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn missing_file_loads_an_empty_registry() {
        let fx = Fixture::new();
        assert!(load_from(&fx.config()).is_empty());
    }

    #[test]
    fn add_persists_and_defaults_the_name_to_the_last_component() {
        let fx = Fixture::new();
        let dir = fx.dir("alpha");
        let (entry, created) = add_in(&fx.config(), &dir.to_string_lossy(), None).unwrap();
        assert!(created);
        assert_eq!(entry.name, "alpha");
        assert!(!entry.path.starts_with(r"\\?\"), "{}", entry.path);
        assert_eq!(entry.last_opened_at, None);
        assert!(!entry.pinned);

        let loaded = load_from(&fx.config());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], entry);
    }

    #[test]
    fn add_keeps_unrelated_top_level_config_keys() {
        let fx = Fixture::new();
        std::fs::write(fx.config(), "{\n  // keep me\n  \"model\": \"gpt-4\"\n}\n").unwrap();
        let dir = fx.dir("beta");
        add_in(&fx.config(), &dir.to_string_lossy(), None).unwrap();
        let text = std::fs::read_to_string(fx.config()).unwrap();
        assert!(text.contains("\"model\""), "{text}");
        assert!(text.contains("keep me"), "{text}");
        assert!(text.contains("\"projects\""), "{text}");
    }

    #[test]
    fn add_dedupes_by_canonical_path() {
        let fx = Fixture::new();
        let dir = fx.dir("gamma");
        let (first, created_first) = add_in(&fx.config(), &dir.to_string_lossy(), None).unwrap();
        // Same location, spelled with a redundant "." segment.
        let noisy = dir.join(".");
        let (second, created_second) =
            add_in(&fx.config(), &noisy.to_string_lossy(), None).unwrap();
        assert!(created_first);
        assert!(!created_second, "second add must reuse the existing entry");
        assert_eq!(first.id, second.id);
        assert_eq!(load_from(&fx.config()).len(), 1);
    }

    #[test]
    fn add_rejects_a_missing_path_and_a_file() {
        let fx = Fixture::new();
        let missing = fx.base.join("nope");
        assert!(matches!(
            add_in(&fx.config(), &missing.to_string_lossy(), None),
            Err(ProjectsError::Invalid(_))
        ));

        let file = fx.base.join("a-file.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(matches!(
            add_in(&fx.config(), &file.to_string_lossy(), None),
            Err(ProjectsError::Invalid(_))
        ));
        assert!(matches!(
            add_in(&fx.config(), "   ", None),
            Err(ProjectsError::Invalid(_))
        ));
        // Nothing was written before the rejection.
        assert!(load_from(&fx.config()).is_empty());
    }

    #[test]
    fn patch_updates_name_and_pinned_and_404s_on_unknown_id() {
        let fx = Fixture::new();
        let dir = fx.dir("delta");
        let (entry, _) = add_in(&fx.config(), &dir.to_string_lossy(), None).unwrap();

        let patched = patch_in(
            &fx.config(),
            &entry.id,
            &ProjectPatch {
                name: Some("Delta ".to_string()),
                pinned: Some(true),
            },
        )
        .unwrap();
        assert_eq!(patched.name, "Delta");
        assert!(patched.pinned);
        assert_eq!(load_from(&fx.config())[0].name, "Delta");

        assert_eq!(
            patch_in(&fx.config(), "no-such-id", &ProjectPatch::default()),
            Err(ProjectsError::NotFound)
        );
    }

    #[test]
    fn remove_forgets_the_entry_but_leaves_the_directory_alone() {
        let fx = Fixture::new();
        let dir = fx.dir("epsilon");
        let (entry, _) = add_in(&fx.config(), &dir.to_string_lossy(), None).unwrap();
        remove_in(&fx.config(), &entry.id).unwrap();
        assert!(load_from(&fx.config()).is_empty());
        assert!(dir.is_dir(), "removing a project must never touch disk");
        assert_eq!(
            remove_in(&fx.config(), &entry.id),
            Err(ProjectsError::NotFound)
        );
    }

    #[test]
    fn mark_opened_sets_the_timestamp() {
        let fx = Fixture::new();
        let dir = fx.dir("zeta");
        let (entry, _) = add_in(&fx.config(), &dir.to_string_lossy(), None).unwrap();
        assert_eq!(entry.last_opened_at, None);

        let opened = mark_opened_in(&fx.config(), &entry.id).unwrap();
        assert!(opened.last_opened_at.is_some());
        assert_eq!(opened.path, entry.path);
        assert_eq!(
            load_from(&fx.config())[0].last_opened_at,
            opened.last_opened_at
        );
        assert_eq!(
            mark_opened_in(&fx.config(), "no-such-id"),
            Err(ProjectsError::NotFound)
        );
    }

    #[test]
    fn list_sorts_pinned_then_recent_then_name() {
        let mut projects = vec![
            ProjectEntry {
                id: "1".into(),
                name: "never".into(),
                path: "/never".into(),
                added_at: 1,
                last_opened_at: None,
                pinned: false,
            },
            ProjectEntry {
                id: "2".into(),
                name: "older".into(),
                path: "/older".into(),
                added_at: 1,
                last_opened_at: Some(10),
                pinned: false,
            },
            ProjectEntry {
                id: "3".into(),
                name: "newer".into(),
                path: "/newer".into(),
                added_at: 1,
                last_opened_at: Some(20),
                pinned: false,
            },
            ProjectEntry {
                id: "4".into(),
                name: "pinned".into(),
                path: "/pinned".into(),
                added_at: 1,
                last_opened_at: None,
                pinned: true,
            },
        ];
        sort_entries(&mut projects);
        let order: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(order, vec!["pinned", "newer", "older", "never"]);
    }
}
