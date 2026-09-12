//! Directory picker backend (F5-2): `GET /fs/browse`.
//!
//! # Threat model (read before changing this file)
//!
//! Unlike `/fs/tree` and `/fs/file` — which are rooted inside one project's
//! instance directory and validate every path against that root — `/fs/browse`
//! is **intentionally host-wide**. That is its entire purpose: the user must be
//! able to navigate to a directory *before* a project exists at that path, so
//! there is no root to be confined to yet.
//!
//! This is safe only under the current trust model:
//! - the whole HTTP API sits behind the capability-token middleware
//!   (`crate::auth::require_token`, applied around the router in
//!   `routes::build_api_router`), and
//! - the engine is meant to run loopback-only or over a trusted LAN — the same
//!   boundary every other endpoint already relies on.
//!
//! What it discloses to a token holder: directory **names and paths**, never
//! file contents, never file names, never recursion. If Bebok ever grows
//! official untrusted-network remote access, this endpoint is the first thing
//! to revisit (e.g. confine it to a configured set of allowed roots).

use axum::Json;
use axum::extract::Query;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

/// `GET /fs/browse?path=&show_hidden=` query.
#[derive(Deserialize)]
pub struct BrowseQuery {
    /// Absolute directory to list. Empty/absent lists the host's roots.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub show_hidden: Option<bool>,
}

/// One row of the picker: always a directory.
#[derive(Debug, Clone, Serialize)]
pub struct BrowseEntry {
    /// Display name (a folder name, or a friendly label such as `C:\` / `Home`).
    pub name: String,
    /// Absolute, normalised filesystem path.
    pub path: String,
    pub hidden: bool,
    /// Whether the engine could list this directory's own children (lets the
    /// client grey the row out instead of failing after the click).
    pub readable: bool,
}

/// `GET /fs/browse` -> immediate **subdirectories** of `path`, or the host's
/// roots when `path` is empty. Never returns files or file contents.
pub async fn fs_browse(
    Query(q): Query<BrowseQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let requested = q.path.as_deref().map(str::trim).unwrap_or("");
    let show_hidden = q.show_hidden.unwrap_or(false);

    if requested.is_empty() {
        return Ok(Json(
            serde_json::json!({ "path": serde_json::Value::Null, "entries": roots() }),
        ));
    }

    let path = std::path::Path::new(requested);
    let meta = std::fs::metadata(path)
        .map_err(|e| ApiError::bad_request(format!("cannot read path: {e}")).into_response())?;
    if !meta.is_dir() {
        return Err(ApiError::bad_request("not a directory").into_response());
    }
    let read = std::fs::read_dir(path).map_err(|e| {
        // The directory itself is unreadable (typically permission denied);
        // individual unreadable children are simply omitted below.
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            ApiError::forbidden(format!("cannot list directory: {e}")).into_response()
        } else {
            ApiError::bad_request(format!("cannot list directory: {e}")).into_response()
        }
    })?;

    let mut entries: Vec<BrowseEntry> = Vec::new();
    for item in read.flatten() {
        // Children we cannot stat are omitted, not an error for the request.
        let Ok(meta) = item.metadata() else { continue };
        if !meta.is_dir() {
            continue;
        }
        let name = item.file_name().to_string_lossy().to_string();
        let hidden = is_hidden(&name, &meta);
        if hidden && !show_hidden {
            continue;
        }
        let child = item.path();
        entries.push(BrowseEntry {
            name,
            path: bebok_core::util::normalize_path(&child),
            hidden,
            readable: std::fs::read_dir(&child).is_ok(),
        });
    }
    entries.sort_by_key(|e| e.name.to_lowercase());

    Ok(Json(serde_json::json!({
        "path": bebok_core::util::normalize_path(path),
        "entries": entries,
    })))
}

/// Hidden-file convention, per OS: the `FILE_ATTRIBUTE_HIDDEN` bit on Windows,
/// a leading dot on Unix. Both branches are cfg-gated; neither is "the"
/// behaviour (Bebok ships on Windows and Linux).
#[allow(unused_variables)]
fn is_hidden(name: &str, meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0 || name.starts_with('.')
    }
    #[cfg(not(windows))]
    {
        name.starts_with('.')
    }
}

/// Top-level roots for the picker, per OS: existing drive letters plus the home
/// directory on Windows; `/` plus the home directory on Unix.
fn roots() -> Vec<BrowseEntry> {
    let mut entries: Vec<BrowseEntry> = Vec::new();

    #[cfg(windows)]
    {
        for letter in b'A'..=b'Z' {
            let root = format!("{}:\\", letter as char);
            let path = std::path::Path::new(&root);
            if std::fs::metadata(path).is_ok() {
                entries.push(BrowseEntry {
                    name: root.clone(),
                    path: bebok_core::util::normalize_path(path),
                    hidden: false,
                    readable: std::fs::read_dir(path).is_ok(),
                });
            }
        }
    }

    #[cfg(not(windows))]
    {
        let root = std::path::Path::new("/");
        entries.push(BrowseEntry {
            name: "/".to_string(),
            path: "/".to_string(),
            hidden: false,
            readable: std::fs::read_dir(root).is_ok(),
        });
    }

    if let Some(home) = dirs::home_dir()
        && home.is_dir()
    {
        entries.push(BrowseEntry {
            name: "Home".to_string(),
            path: bebok_core::util::normalize_path(&home),
            hidden: false,
            readable: std::fs::read_dir(&home).is_ok(),
        });
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let base =
            std::env::temp_dir().join(format!("bebok-browse-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    async fn browse(path: Option<&str>, show_hidden: bool) -> Result<serde_json::Value, u16> {
        let query = BrowseQuery {
            path: path.map(str::to_string),
            show_hidden: Some(show_hidden),
        };
        match fs_browse(Query(query)).await {
            Ok(Json(value)) => Ok(value),
            Err(response) => Err(response.status().as_u16()),
        }
    }

    #[tokio::test]
    async fn empty_path_returns_host_roots() {
        let value = browse(None, false).await.unwrap();
        assert!(value["path"].is_null());
        let entries = value["entries"].as_array().unwrap();
        assert!(!entries.is_empty(), "every host has at least one root");
        #[cfg(not(windows))]
        assert!(entries.iter().any(|e| e["path"] == "/"));
        #[cfg(windows)]
        assert!(
            entries
                .iter()
                .any(|e| e["name"].as_str().unwrap_or("").ends_with(":\\")),
            "{entries:?}"
        );
    }

    #[tokio::test]
    async fn lists_subdirectories_only_sorted_case_insensitively() {
        let base = temp_dir("list");
        std::fs::create_dir_all(base.join("Beta")).unwrap();
        std::fs::create_dir_all(base.join("alpha")).unwrap();
        std::fs::write(base.join("a-file.txt"), "x").unwrap();

        let value = browse(Some(&base.to_string_lossy()), false).await.unwrap();
        let names: Vec<String> = value["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["alpha", "Beta"]);
        // Normalised paths: no Windows verbatim prefix ever reaches the client.
        for entry in value["entries"].as_array().unwrap() {
            assert!(!entry["path"].as_str().unwrap().starts_with(r"\\?\"));
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn a_file_path_is_rejected_with_400() {
        let base = temp_dir("file");
        let file = base.join("plain.txt");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(browse(Some(&file.to_string_lossy()), false).await, Err(400));
        assert_eq!(
            browse(Some(&base.join("missing").to_string_lossy()), false).await,
            Err(400)
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn show_hidden_toggles_the_result_set() {
        let base = temp_dir("hidden");
        std::fs::create_dir_all(base.join("visible")).unwrap();
        let dotted = base.join(".secret");
        std::fs::create_dir_all(&dotted).unwrap();

        let shown = browse(Some(&base.to_string_lossy()), true).await.unwrap();
        let hidden_off = browse(Some(&base.to_string_lossy()), false).await.unwrap();
        let names = |v: &serde_json::Value| -> Vec<String> {
            v["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert!(names(&shown).contains(&".secret".to_string()));
        assert!(!names(&hidden_off).contains(&".secret".to_string()));
        assert!(names(&hidden_off).contains(&"visible".to_string()));
        let _ = std::fs::remove_dir_all(base);
    }

    /// Unix convention: a leading dot marks the entry hidden, and nothing else
    /// does. Cfg-gated so the Windows CI leg does not assert Unix behaviour.
    #[cfg(unix)]
    #[test]
    fn unix_hidden_detection_is_the_dotfile_convention() {
        let base = temp_dir("unix-hidden");
        std::fs::create_dir_all(base.join(".dotted")).unwrap();
        std::fs::create_dir_all(base.join("plain")).unwrap();
        let dotted = std::fs::metadata(base.join(".dotted")).unwrap();
        let plain = std::fs::metadata(base.join("plain")).unwrap();
        assert!(is_hidden(".dotted", &dotted));
        assert!(!is_hidden("plain", &plain));
        let _ = std::fs::remove_dir_all(base);
    }

    /// Windows convention: the `FILE_ATTRIBUTE_HIDDEN` bit, independent of the
    /// name (dotfiles are additionally honoured because cross-platform repos
    /// are full of them).
    #[cfg(windows)]
    #[test]
    fn windows_hidden_detection_uses_the_attribute_bit() {
        let base = temp_dir("win-hidden");
        let plain = base.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let meta = std::fs::metadata(&plain).unwrap();
        assert!(!is_hidden("plain", &meta), "no hidden bit, no dot");
        assert!(is_hidden(".dotted", &meta), "dotfiles count as hidden too");

        // Set the real attribute bit via `attrib` and re-stat.
        let hidden = base.join("attr");
        std::fs::create_dir_all(&hidden).unwrap();
        let ok = std::process::Command::new("attrib")
            .arg("+h")
            .arg(&hidden)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            let meta = std::fs::metadata(&hidden).unwrap();
            assert!(is_hidden("attr", &meta), "FILE_ATTRIBUTE_HIDDEN must count");
        }
        let _ = std::fs::remove_dir_all(base);
    }
}
