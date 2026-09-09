use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Current unix time in milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// FNV-1a 64-bit hash (stable across Rust versions and platforms).
pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Deterministic directory hash (hex) used for the on-disk instance directory.
pub fn hash_dir(directory: &str) -> String {
    format!("{:016x}", fnv1a64(directory.as_bytes()))
}

/// Normalize a directory path: canonicalize when it exists, otherwise make it
/// absolute. Returns a stable string used as the instance key.
pub fn normalize_path(path: &Path) -> String {
    if let Ok(canon) = std::fs::canonicalize(path) {
        return canon.to_string_lossy().to_string();
    }
    if path.is_absolute() {
        return path.to_string_lossy().to_string();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

/// Atomically write `bytes` to `path` (tmp file + rename).
pub async fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(parent).await?;

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "out".to_string());
    let tmp = parent.join(format!(".{}.tmp-{}", file_name, uuid::Uuid::new_v4()));

    let path = path.to_path_buf();
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    })
    .await
    .map_err(|e| std::io::Error::other(e.to_string()))??;

    Ok(())
}

/// Append a line to a file (append-only journal, e.g. `index.jsonl`).
pub async fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let path = path.to_path_buf();
    let line = line.to_string();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::fs::OpenOptions;
        let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        Ok(())
    })
    .await
    .map_err(|e| std::io::Error::other(e.to_string()))?
}

/// Truncate a tool output to `max` bytes, preserving the tail (a marker is
/// inserted at the head). Keeps long outputs bounded for the LLM context.
pub fn truncate_output(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(64);
    let head: String = text.chars().take(64).collect();
    let tail: String = text.chars().skip(text.chars().count().saturating_sub(keep)).collect();
    format!("{head}\n...[truncated {} bytes]...\n{tail}", text.len() - keep)
}
