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
        return without_windows_verbatim_prefix(canon.to_string_lossy().as_ref());
    }
    if path.is_absolute() {
        return without_windows_verbatim_prefix(path.to_string_lossy().as_ref());
    }
    let absolute = std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf());
    without_windows_verbatim_prefix(absolute.to_string_lossy().as_ref())
}

fn without_windows_verbatim_prefix(path: &str) -> String {
    #[cfg(windows)]
    {
        if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{unc}");
        }
        if let Some(local) = path.strip_prefix(r"\\?\") {
            return local.to_string();
        }
    }
    path.to_string()
}

#[cfg(all(test, windows))]
mod path_tests {
    use super::*;

    #[test]
    fn normalize_path_removes_windows_verbatim_prefix() {
        let dir = std::env::temp_dir().join(format!("bebok-path-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let normalized = normalize_path(&dir);
        assert!(!normalized.starts_with(r"\\?\"), "{normalized}");
        assert_eq!(Path::new(&normalized), dir);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn removes_drive_and_unc_prefixes() {
        assert_eq!(without_windows_verbatim_prefix(r"\\?\C:\work"), r"C:\work");
        assert_eq!(
            without_windows_verbatim_prefix(r"\\?\UNC\server\share"),
            r"\\server\share"
        );
    }
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

// ---------------------------------------------------------------------------
// Tool output compression
// ---------------------------------------------------------------------------

/// Strip ANSI escape codes (SGR sequences) from `text`.
/// Handles `\x1b[...m`, `\x1b[...~`, OSC sequences, and single-char escapes.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                // CSI: ESC [ ... final_byte (0x40–0x7E)
                chars.next(); // consume '['
                while let Some(&ch) = chars.peek() {
                    chars.next();
                    if ('@'..='~').contains(&ch) {
                        break;
                    }
                }
            } else if chars.peek() == Some(&']') {
                // OSC: ESC ] ... BEL or ESC \
                chars.next(); // consume ']'
                for ch in chars.by_ref() {
                    if ch == '\x07' || ch == '\x1b' {
                        break;
                    }
                }
            }
            // Single-char escape: already consumed by the outer next().
        } else {
            out.push(c);
        }
    }
    out
}

/// Collapse runs of 3+ blank lines to at most 2. Strips trailing whitespace
/// from each line. The intent is to compress noisy terminal output while
/// preserving single blank line separations that the model uses for structure.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run: u32 = 0;
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run <= 2 {
                out.push('\n');
            }
        } else {
            blank_run = 0;
            out.push_str(trimmed);
            out.push('\n');
        }
    }
    // Remove trailing blank lines.
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Compress tool output before it enters the transcript: strip ANSI codes,
/// collapse excessive whitespace. This is applied once at capture time so the
/// saved transcript is always compact.
///
/// This function does NOT truncate — `truncate_output` handles size limiting
/// separately (call compress first, then truncate).
pub fn compress_tool_output(text: &str) -> String {
    let stripped = strip_ansi(text);
    collapse_whitespace(&stripped)
}

// ---------------------------------------------------------------------------
// Tool output truncation (smart head+tail)
// ---------------------------------------------------------------------------

/// Truncate a tool output to `max` bytes, preserving the head (for context)
/// and the tail (where results usually live), with a summary marker in
/// between.
///
/// For short outputs (< 10 lines), uses a 30%/70% head/tail split on the
/// whole text. For multi-line output, keeps the first 20 lines and as many
/// tail lines as fit in the remaining budget.
pub fn truncate_output(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }

    let lines: Vec<&str> = text.lines().collect();
    let line_count = lines.len();
    let dropped_bytes = text.len().saturating_sub(max) + 128;

    // For short outputs (< 10 lines), use simple head+tail on the whole text.
    if line_count < 10 {
        let head_budget = (max * 30) / 100;
        let tail_budget = max.saturating_sub(head_budget + 128);

        let head: String = text.chars().take(head_budget).collect();
        let tail: String = text
            .chars()
            .rev()
            .take(tail_budget)
            .collect::<String>()
            .chars()
            .rev()
            .collect();

        return format!(
            "{head}\n...[truncated {dropped_bytes} bytes, {line_count} lines]...\n{tail}"
        );
    }

    // Multi-line: keep first 20 lines + last N lines.
    let head_lines: usize = 20.min(line_count / 3);
    let marker = format!("...[{line_count} lines total, {dropped_bytes} bytes truncated]...");

    let head_bytes: usize = lines[..head_lines].iter().map(|l| l.len() + 1).sum();
    let marker_bytes = marker.len() + 2;
    let remaining = max.saturating_sub(head_bytes + marker_bytes + 256);
    let avg_line_len = (text.len() / line_count).max(1);
    let tail_lines = (remaining / avg_line_len)
        .max(5)
        .min(line_count - head_lines);

    let head: String = lines[..head_lines].join("\n");
    let tail_start = line_count.saturating_sub(tail_lines);
    let tail: String = lines[tail_start..].join("\n");

    format!("{head}\n{marker}\n{tail}")
}

/// Truncate to at most `max_bytes`, backing up to a UTF-8 char boundary.
pub fn truncate_chars(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Compute the effective tool output cap for a given conversation position.
///
/// Early in a conversation (few messages), outputs are kept at full size since
/// they're actively useful. As the conversation grows, old tool outputs get
/// pruned anyway, so new outputs can be smaller without loss:
///
/// - 0-4 messages:  cap = configured tool_output_cap (full)
/// - 5-15 messages: cap = min(configured, 24 KiB)
/// - 16-30 messages: cap = min(configured, 16 KiB)
/// - 31+ messages:  cap = min(configured, 8 KiB)
#[deprecated(
    note = "Phase 4: persist path now uses static tool_output_cap; prune_for_budget handles request-time shrinking"
)]
#[allow(dead_code)]
pub fn dynamic_output_cap(configured_cap: usize, message_count: usize) -> usize {
    match message_count {
        0..=4 => configured_cap,
        5..=15 => configured_cap.min(24 * 1024),
        16..=30 => configured_cap.min(16 * 1024),
        _ => configured_cap.min(8 * 1024),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_sgr() {
        let input = "normal \x1b[31mred\x1b[0m normal";
        assert_eq!(strip_ansi(input), "normal red normal");
    }

    #[test]
    fn strip_ansi_removes_csi_cursor() {
        let input = "line1\x1b[2Aline2";
        assert_eq!(strip_ansi(input), "line1line2");
    }

    #[test]
    fn strip_ansi_removes_osc() {
        let input = "\x1b]0;title\x07rest";
        assert_eq!(strip_ansi(input), "rest");
    }

    #[test]
    fn collapse_blank_lines() {
        // 4 blank lines between a/b -> collapsed to 2; 3 between b/c -> collapsed to 2
        let input = "a\n\n\n\n\nb\n\n\n\nc";
        assert_eq!(collapse_whitespace(input), "a\n\n\nb\n\n\nc");
    }

    #[test]
    fn compress_combines() {
        // ANSI stripped, then 4 blank lines collapsed to 2
        let input = "Hello \x1b[31mworld\x1b[0m\n\n\n\n\nDone";
        assert_eq!(compress_tool_output(input), "Hello world\n\n\nDone");
    }

    #[test]
    fn truncate_small_unchanged() {
        let text = "short";
        assert_eq!(truncate_output(text, 100), "short");
    }

    #[test]
    fn truncate_preserves_head_tail() {
        // Build a multi-line text over 300 bytes so the 200-byte cap triggers
        // the multi-line path (>= 10 lines).
        let lines: Vec<String> = (0..15)
            .map(|i| format!("line-{:02} some filler text to make it longer", i))
            .collect();
        let mut text = lines.join("\n");
        text.push_str("\nresult: 42");
        assert!(text.len() > 300, "text must exceed 300 bytes");
        let out = truncate_output(&text, 200);
        assert!(out.starts_with("line-00"));
        assert!(out.contains("result: 42"));
        assert!(out.contains("truncated"));
    }

    #[test]
    #[allow(deprecated)]
    fn dynamic_cap_decreases() {
        assert_eq!(dynamic_output_cap(32768, 0), 32768);
        assert_eq!(dynamic_output_cap(32768, 10), 24576);
        assert_eq!(dynamic_output_cap(32768, 20), 16384);
        assert_eq!(dynamic_output_cap(32768, 40), 8192);
    }

    #[test]
    #[allow(deprecated)]
    fn dynamic_cap_never_exceeds_config() {
        assert_eq!(dynamic_output_cap(4096, 0), 4096);
        assert_eq!(dynamic_output_cap(4096, 40), 4096);
    }

    /// Phase 4 regression: the persist pipeline (compress → truncate) must
    /// produce identical output regardless of conversation length. Shrinking
    /// based on message count is now reserved for request-time pruning only.
    #[test]
    fn persist_output_invariant_to_message_count() {
        let raw = "normal text \x1b[31mwith\x1b[0m ANSI\n\n\n\n\nand blank lines\nresult: 42\n";
        let cap = 4096;
        let compressed = compress_tool_output(raw);
        let text_short = truncate_output(&compressed, cap);
        let text_long = truncate_output(&compressed, cap);

        // The persist path now uses the static cap directly; assert
        // identity regardless of what "message count" would have been.
        assert_eq!(
            text_short, text_long,
            "persisted tool output must be identical regardless of conversation position"
        );
        // Verify the output is the compressed text (under the cap, no truncation).
        assert_eq!(text_short, compressed);
    }

    /// Explicitly show that the old dynamic_output_cap would have shrunk the
    /// cap at high message counts, while the current static cap does not.
    #[test]
    fn static_cap_does_not_shrink_with_conversation_length() {
        let configured_cap = 32 * 1024; // 32 KiB
        // Build a tool output that fits the static cap but exceeds what
        // dynamic_output_cap(32768, 40) would allow (= 8 KiB).
        let long_output = "x".repeat(16 * 1024);
        let compressed = compress_tool_output(&long_output);

        let persist_result = truncate_output(&compressed, configured_cap);
        #[allow(deprecated)]
        let old_result = truncate_output(&compressed, dynamic_output_cap(configured_cap, 40));

        // Static cap keeps full output; old dynamic cap would have truncated.
        assert_eq!(persist_result, long_output);
        assert!(old_result.len() < long_output.len());
    }
}
