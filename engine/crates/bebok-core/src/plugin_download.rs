//! Plugin download + archive unpacking for asset-based installation.
//!
//! When a registry entry or declaration carries `asset_url`, the installer
//! downloads the pre-built archive, verifies `asset_sha256`, and unpacks it
//! into the plugin slot — instead of cloning the git repo. The archive is
//! saved to `<root>/.bebok/plugins/.downloads/` and removed after a
//! successful unpack. Zip-slip and tar-slip protections reject any path
//! escape (`..`, absolute, symlink outside the slot).

use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};

use crate::error::{CoreError, Result};
use crate::event::EventBus;

/// Maximum download size: 256 MiB.
const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;

/// Request timeout for the download.
const DOWNLOAD_TIMEOUT_SECS: u64 = 60;

// ---------------------------------------------------------------------------
// Kind detection
// ---------------------------------------------------------------------------

/// Detected archive format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    TarGz,
}

/// Detect the archive format from a URL suffix. Case-insensitive.
pub fn kind_from_url(url: &str) -> Option<ArchiveKind> {
    let lower = url.to_ascii_lowercase();
    if lower.ends_with(".zip") {
        Some(ArchiveKind::Zip)
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        Some(ArchiveKind::TarGz)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// SHA-256
// ---------------------------------------------------------------------------

/// Compute SHA-256 hex digest of a file.
pub fn sha256_hex(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).map_err(CoreError::Io)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(CoreError::Io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Verify the SHA-256 digest of a file matches the expected hex string.
pub fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let actual = sha256_hex(path)?;
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(CoreError::BadRequest(format!(
            "SHA-256 mismatch for {}: expected {expected}, got {actual}",
            path.display()
        )))
    }
}

// ---------------------------------------------------------------------------
// Download
// ---------------------------------------------------------------------------

/// Download a file from `url` into `dest`, capped at [`MAX_DOWNLOAD_BYTES`].
/// Publishes `plugin.install.progress` events when a bus is provided.
pub async fn download_archive(
    url: &str,
    dest: &Path,
    bus: Option<&EventBus>,
    directory: &str,
    name: &str,
) -> Result<()> {
    if let Some(bus) = bus {
        bus.publish(crate::event::Event::plugin_install_progress(
            directory, name, "download", url,
        ));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .user_agent(format!("bebok/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| CoreError::Other(format!("failed to build HTTP client: {e}")))?;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| CoreError::BadRequest(format!("download failed for {url}: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(CoreError::BadRequest(format!(
            "download returned HTTP {status} for {url}"
        )));
    }

    if let Some(total) = response.content_length()
        && total > MAX_DOWNLOAD_BYTES
    {
        return Err(CoreError::BadRequest(format!(
            "archive too large: {total} bytes (max {MAX_DOWNLOAD_BYTES})"
        )));
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(CoreError::Io)?;
    }
    let mut file = std::fs::File::create(dest).map_err(CoreError::Io)?;
    let mut downloaded: u64 = 0;
    let mut stream = response.bytes_stream();
    use futures::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| CoreError::Other(format!("download stream error: {e}")))?;
        downloaded += chunk.len() as u64;
        if downloaded > MAX_DOWNLOAD_BYTES {
            let _ = std::fs::remove_file(dest);
            return Err(CoreError::BadRequest(format!(
                "archive too large: exceeded {MAX_DOWNLOAD_BYTES} bytes during download"
            )));
        }
        std::io::Write::write_all(&mut file, &chunk).map_err(CoreError::Io)?;
    }
    file.sync_all().map_err(CoreError::Io)?;

    if let Some(bus) = bus {
        bus.publish(crate::event::Event::plugin_install_progress(
            directory, name, "download", "done",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Unpack helpers — slot validation
// ---------------------------------------------------------------------------

/// Reject paths that escape the target slot directory (zip-slip / tar-slip
/// protection) or use symlinks.
fn validate_entry_path(root: &Path, entry_path: &str) -> Result<PathBuf> {
    use std::path::Component;

    // Reject absolute paths (check entry_path itself, not the joined path,
    // because Windows doesn't recognise `/etc/passwd` as absolute).
    let entry = Path::new(entry_path);
    if entry.is_absolute() {
        return Err(CoreError::BadRequest(format!(
            "archive entry has absolute path: {entry_path}"
        )));
    }
    // Also reject paths starting with `/` or `\` (not considered absolute
    // on Windows but still a security concern in archives).
    if let Some(first) = entry_path.chars().next()
        && (first == '/' || first == '\\')
    {
        return Err(CoreError::BadRequest(format!(
            "archive entry has absolute path: {entry_path}"
        )));
    }

    // Walk the relative path components and reject any `..` that escapes
    // the slot. We maintain a stack of normal path segments.
    let mut stack: Vec<&str> = Vec::new();
    for comp in entry.components() {
        match comp {
            Component::ParentDir => {
                if stack.pop().is_none() {
                    // Trying to go above the root.
                    return Err(CoreError::BadRequest(format!(
                        "archive entry escapes slot directory: {entry_path}"
                    )));
                }
            }
            Component::Normal(seg) => {
                stack.push(seg.to_str().unwrap_or(""));
            }
            Component::RootDir | Component::Prefix(_) | Component::CurDir => {
                return Err(CoreError::BadRequest(format!(
                    "archive entry escapes slot directory: {entry_path}"
                )));
            }
        }
    }

    let resolved = root.join(stack.iter().collect::<PathBuf>());
    Ok(resolved)
}

// ---------------------------------------------------------------------------
// Unpack ZIP
// ---------------------------------------------------------------------------

/// Unpack a `.zip` archive into `slot_dir`, enforcing slot confinement.
pub fn unpack_zip(archive: &Path, slot_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(archive).map_err(CoreError::Io)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| CoreError::BadRequest(format!("invalid zip archive: {e}")))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| CoreError::BadRequest(format!("zip entry error: {e}")))?;
        let entry_name = entry.name().to_string();

        // Skip directory entries.
        if entry_name.ends_with('/') {
            continue;
        }

        let dest = validate_entry_path(slot_dir, &entry_name)?;

        // Reject symlinks.
        if entry.is_symlink() {
            return Err(CoreError::BadRequest(format!(
                "archive contains symlink: {entry_name}"
            )));
        }

        // Ensure parent directory exists.
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(CoreError::Io)?;
        }

        let mut out = std::fs::File::create(&dest).map_err(CoreError::Io)?;
        std::io::copy(&mut entry, &mut out).map_err(CoreError::Io)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Unpack tar.gz
// ---------------------------------------------------------------------------

/// Unpack a `.tar.gz` archive into `slot_dir`, enforcing slot confinement.
pub fn unpack_tar_gz(archive: &Path, slot_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(archive).map_err(CoreError::Io)?;
    let decoder = GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    // Unset permissions to avoid issues; we don't need executable bits from
    // the archive.
    archive.set_unpack_xattrs(false);
    // Don't follow symlinks — we detect and reject them.
    archive.set_overwrite(true);

    // We need to iterate entries manually to enforce path validation before
    // extraction.
    let entries = archive
        .entries()
        .map_err(|e| CoreError::BadRequest(format!("failed to read tar.gz entries: {e}")))?;

    for entry in entries {
        let mut entry =
            entry.map_err(|e| CoreError::BadRequest(format!("tar.gz entry error: {e}")))?;
        let entry_path = entry
            .path()
            .map_err(CoreError::Io)?
            .to_string_lossy()
            .to_string();

        // Skip the top-level directory entry (if any).
        if entry_path.is_empty() || entry_path == "./" {
            continue;
        }
        // Strip a single leading directory component (common in tarballs
        // that wrap everything in `plugin-name-1.0.0/`).
        let stripped = match entry_path.strip_prefix("./") {
            Some(s) => s,
            None => &entry_path,
        };
        // If there's a single top-level dir, skip it.
        let relative = if let Some((_top, rest)) = stripped.split_once('/') {
            if rest.is_empty() {
                // This is the top-level dir entry itself.
                continue;
            }
            rest
        } else {
            stripped
        };

        if relative.is_empty() || relative == "." {
            continue;
        }

        // Reject symlinks.
        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink() {
            return Err(CoreError::BadRequest(format!(
                "archive contains symlink: {relative}"
            )));
        }

        let dest = validate_entry_path(slot_dir, relative)?;

        if entry_type.is_dir() {
            std::fs::create_dir_all(&dest).map_err(CoreError::Io)?;
        } else {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(CoreError::Io)?;
            }
            let mut out = std::fs::File::create(&dest).map_err(CoreError::Io)?;
            std::io::copy(&mut entry, &mut out).map_err(CoreError::Io)?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Unpack (dispatch)
// ---------------------------------------------------------------------------

/// Unpack a downloaded archive into `slot_dir` based on its detected format.
pub fn unpack(archive: &Path, slot_dir: &Path, kind: ArchiveKind) -> Result<()> {
    match kind {
        ArchiveKind::Zip => unpack_zip(archive, slot_dir),
        ArchiveKind::TarGz => unpack_tar_gz(archive, slot_dir),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_from_url_detects_zip() {
        assert_eq!(
            kind_from_url("https://example.com/plugin.zip"),
            Some(ArchiveKind::Zip)
        );
        assert_eq!(
            kind_from_url("https://example.com/PLUGIN.ZIP"),
            Some(ArchiveKind::Zip)
        );
    }

    #[test]
    fn kind_from_url_detects_tar_gz() {
        assert_eq!(
            kind_from_url("https://example.com/plugin.tar.gz"),
            Some(ArchiveKind::TarGz)
        );
        assert_eq!(
            kind_from_url("https://example.com/plugin.tgz"),
            Some(ArchiveKind::TarGz)
        );
    }

    #[test]
    fn kind_from_url_returns_none_for_unknown() {
        assert_eq!(kind_from_url("https://example.com/plugin.rar"), None);
        assert_eq!(kind_from_url("https://example.com/plugin"), None);
    }

    #[test]
    fn sha256_hex_matches_known_digest() {
        let dir = std::env::temp_dir().join(format!("bebok-dl-sha-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.txt");
        std::fs::write(&file, b"hello world").unwrap();
        let hex = sha256_hex(&file).unwrap();
        assert_eq!(
            hex,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_sha256_ok() {
        let dir = std::env::temp_dir().join(format!("bebok-dl-verify-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.txt");
        std::fs::write(&file, b"hello world").unwrap();
        verify_sha256(
            &file,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9",
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_sha256_mismatch_errors() {
        let dir =
            std::env::temp_dir().join(format!("bebok-dl-verify-err-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.txt");
        std::fs::write(&file, b"hello world").unwrap();
        let err = verify_sha256(
            &file,
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap_err();
        assert!(format!("{err}").contains("SHA-256 mismatch"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unpack_zip_slip_is_rejected() {
        use std::io::Write;

        let dir = std::env::temp_dir().join(format!("bebok-dl-zipslip-{}", uuid::Uuid::new_v4()));
        let slot = dir.join("slot");
        std::fs::create_dir_all(&slot).unwrap();
        let archive_path = dir.join("evil.zip");

        // Create a zip that tries to write outside the slot.
        let zip_file = std::fs::File::create(&archive_path).unwrap();
        let mut zip = zip::ZipWriter::new(zip_file);
        zip.start_file(
            "../../../etc/evil.txt",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"evil").unwrap();
        zip.finish().unwrap();

        let err = unpack_zip(&archive_path, &slot).unwrap_err();
        assert!(
            format!("{err}").contains("escapes slot"),
            "zip-slip must be caught: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unpack_zip_clean_file_extracted() {
        use std::io::Write;

        let dir = std::env::temp_dir().join(format!("bebok-dl-zipclean-{}", uuid::Uuid::new_v4()));
        let slot = dir.join("slot");
        std::fs::create_dir_all(&slot).unwrap();
        let archive_path = dir.join("clean.zip");

        let zip_file = std::fs::File::create(&archive_path).unwrap();
        let mut zip = zip::ZipWriter::new(zip_file);
        zip.start_file(
            "bebok-plugin.json",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(br#"{"name":"test","version":"1.0.0"}"#)
            .unwrap();
        zip.start_file("bin/test", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"#!/bin/sh\necho ok").unwrap();
        zip.finish().unwrap();

        unpack_zip(&archive_path, &slot).unwrap();
        assert!(slot.join("bebok-plugin.json").exists());
        assert!(slot.join("bin/test").exists());
        let content = std::fs::read_to_string(slot.join("bebok-plugin.json")).unwrap();
        assert!(content.contains("test"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_validation_rejects_absolute() {
        let root = Path::new("/tmp/test-slot");
        let err = validate_entry_path(root, "/etc/passwd").unwrap_err();
        assert!(
            format!("{err}").contains("absolute"),
            "absolute path must be rejected: {err}"
        );
    }
}
