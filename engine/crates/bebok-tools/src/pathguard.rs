//! Resolve agent-supplied file paths without leaving the instance root.

use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

const ESCAPE: &str = "path escapes project root";

/// Reject traversal and absolute paths on both Windows and Unix, then check
/// every existing ancestor after symlink resolution. Call this immediately
/// before a filesystem operation; it does not protect against a concurrent
/// symlink replacement between the check and that operation.
pub(crate) fn resolve_in_root(root: &Path, input: &str) -> Result<PathBuf, String> {
    let portable = input.replace('\\', "/");
    let bytes = portable.as_bytes();
    let drive_prefix = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if portable.is_empty() || portable.starts_with('/') || drive_prefix {
        return Err(ESCAPE.into());
    }

    let relative = Path::new(&portable);
    if relative.is_absolute()
        || relative.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
        || !relative
            .components()
            .any(|part| matches!(part, Component::Normal(_)))
    {
        return Err(ESCAPE.into());
    }

    let canonical_root =
        std::fs::canonicalize(root).map_err(|e| format!("cannot resolve project root: {e}"))?;
    let full = root.join(relative);
    let mut ancestor = full.as_path();
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                // A dangling symlink is rejected as well: a later write could
                // otherwise follow it to a new file outside the root.
                let canonical = std::fs::canonicalize(ancestor)
                    .map_err(|e| format!("cannot resolve path: {e}"))?;
                if !canonical.starts_with(&canonical_root) {
                    return Err(ESCAPE.into());
                }
                break;
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                ancestor = ancestor.parent().ok_or(ESCAPE)?;
            }
            Err(e) => return Err(format!("cannot inspect path: {e}")),
        }
    }
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_portable_traversal_and_absolute_paths() {
        let root = std::env::temp_dir();
        for path in [
            "../../secret",
            r"..\..\secret",
            "/tmp/secret",
            r"C:\secret",
            r"\\server\share\secret",
        ] {
            assert!(resolve_in_root(&root, path).is_err(), "{path}");
        }
    }

    #[test]
    fn rejects_symlink_to_outside() {
        let base = std::env::temp_dir().join(format!("bebok-guard-{}", uuid::Uuid::new_v4()));
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let outside = base.join("secret.txt");
        std::fs::write(&outside, "secret").unwrap();
        let link = root.join("link.txt");
        #[cfg(unix)]
        let created = std::os::unix::fs::symlink(&outside, &link);
        #[cfg(windows)]
        let created = std::os::windows::fs::symlink_file(&outside, &link);
        if let Err(e) = created {
            // Windows installations without Developer Mode cannot create
            // symlinks as an ordinary user.
            #[cfg(windows)]
            if e.kind() == ErrorKind::PermissionDenied || e.raw_os_error() == Some(1314) {
                std::fs::remove_dir_all(base).unwrap();
                return;
            }
            panic!("cannot create symlink fixture: {e}");
        }
        assert!(resolve_in_root(&root, "link.txt").is_err());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret");
        std::fs::remove_dir_all(base).unwrap();
    }
}
