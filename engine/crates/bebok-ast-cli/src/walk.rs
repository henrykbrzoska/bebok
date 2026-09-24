use std::collections::HashSet;
use std::path::Path;

/// Collect up to `max_files` files matching the given extensions.
/// Uses the `ignore` crate to respect `.gitignore`.
pub fn collect_files(root: &Path, exts: &[String], max_files: usize) -> Vec<std::path::PathBuf> {
    let ext_set: HashSet<&str> = exts.iter().map(|s| s.as_str()).collect();
    let mut results = Vec::new();

    let walker = ignore::WalkBuilder::new(root)
        .hidden(false) // include hidden files (e.g. .bebok)
        .git_ignore(true)
        .max_depth(Some(20))
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if ext_set.contains(ext) {
            results.push(path.to_path_buf());
            if results.len() >= max_files {
                break;
            }
        }
    }

    results
}
