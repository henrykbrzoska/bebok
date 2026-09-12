//! Session-scoped file-change tracking (WP-CHANGES / F6-8).
//!
//! The agent loop (`agent/exec.rs`) calls [`Tracker::snapshot_before_write`]
//! right before one of the file-mutating tools (`write_file`, `append_file`,
//! `edit_file`) runs. The first write to a path within a session captures a
//! *baseline*: the file's bytes as they were, or a "did not exist" marker for
//! a brand-new file. Later writes to the same path are no-ops for tracking,
//! so the recorded diff always spans every edit the session made.
//!
//! Layout (created lazily under the session directory):
//!
//! ```text
//! <session dir>/changes/index.json   { "version": 1, "entries": [ { path, existed, file } ] }
//! <session dir>/changes/0001.snap    raw bytes of the pre-write file (absent when !existed)
//! ```
//!
//! Diffs and reverts prefer the git `HEAD` version of a tracked file when the
//! project is a git work tree (meaningful across sessions); otherwise they use
//! the stored snapshot. Git is driven through the `git` CLI (no libgit2).

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Sub-directory of the session directory holding snapshots.
pub const CHANGES_DIR: &str = "changes";
const INDEX_FILE: &str = "index.json";
const DIFF_CONTEXT: usize = 3;

/// Tools whose `path` argument is snapshotted before execution.
pub const TRACKED_TOOLS: &[&str] = &["write_file", "append_file", "edit_file"];

/// True when `tool` is one of the file-mutating tools this module tracks.
pub fn is_tracked_tool(tool: &str) -> bool {
    TRACKED_TOOLS.contains(&tool)
}

/// Serialises index read-modify-write across concurrent sessions in-process.
static INDEX_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotMeta {
    /// Project-relative path, forward slashes.
    path: String,
    /// False when the file did not exist before the first write.
    existed: bool,
    /// Snapshot file name inside `changes/` (only when `existed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entries: Vec<SnapshotMeta>,
}

/// Which baseline a diff / revert is computed against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BaselineKind {
    Git,
    Snapshot,
}

/// One tracked file as listed by `GET /session/{id}/changes`.
#[derive(Debug, Clone, Serialize)]
pub struct ChangeEntry {
    pub path: String,
    pub added: usize,
    pub removed: usize,
    pub baseline: BaselineKind,
    /// Whether the file currently exists on disk.
    pub exists: bool,
}

/// `GET /session/{id}/changes/diff` result.
#[derive(Debug, Clone, Serialize)]
pub struct ChangeDiff {
    pub path: String,
    pub diff: String,
    pub baseline: BaselineKind,
    pub added: usize,
    pub removed: usize,
}

/// `POST /session/{id}/changes/revert` result.
#[derive(Debug, Clone, Serialize)]
pub struct RevertResult {
    pub path: String,
    pub baseline: BaselineKind,
    /// False when the baseline was "did not exist" and the file was removed.
    pub exists: bool,
}

enum Baseline {
    Git(Vec<u8>),
    /// `None` = the file did not exist before the session touched it.
    Snapshot(Option<Vec<u8>>),
}

impl Baseline {
    fn kind(&self) -> BaselineKind {
        match self {
            Baseline::Git(_) => BaselineKind::Git,
            Baseline::Snapshot(_) => BaselineKind::Snapshot,
        }
    }
}

/// Per-session tracker bound to the project root + session directory.
pub struct Tracker {
    root: PathBuf,
    changes_dir: PathBuf,
}

impl Tracker {
    pub fn new(root: impl AsRef<Path>, session_dir: &Path) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            changes_dir: session_dir.join(CHANGES_DIR),
        }
    }

    /// Capture the pre-write state of `input` (an agent-supplied path) unless
    /// a snapshot already exists for it. Returns `Ok(true)` when a new snapshot
    /// was captured, `Ok(false)` when one existed already.
    pub fn snapshot_before_write(&self, input: &str) -> Result<bool, String> {
        let rel = normalize_rel(input)?;
        let full = bebok_tools::resolve_in_root(&self.root, &rel)?;
        let _guard = INDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut index = self.read_index();
        if index.entries.iter().any(|e| e.path == rel) {
            return Ok(false);
        }
        std::fs::create_dir_all(&self.changes_dir)
            .map_err(|e| format!("cannot create changes dir: {e}"))?;
        let meta = match std::fs::read(&full) {
            Ok(bytes) => {
                let name = format!("{:04}.snap", index.entries.len() + 1);
                std::fs::write(self.changes_dir.join(&name), bytes)
                    .map_err(|e| format!("cannot write snapshot: {e}"))?;
                SnapshotMeta {
                    path: rel,
                    existed: true,
                    file: Some(name),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => SnapshotMeta {
                path: rel,
                existed: false,
                file: None,
            },
            Err(e) => return Err(format!("cannot read {input}: {e}")),
        };
        index.entries.push(meta);
        self.write_index(&index)?;
        Ok(true)
    }

    /// Every tracked path with `+`/`-` line counts against its baseline.
    pub fn list(&self) -> Vec<ChangeEntry> {
        let index = self.read_index();
        let git = GitProbe::new(&self.root);
        index
            .entries
            .iter()
            .map(|meta| {
                let baseline = self.baseline_for(meta, &git);
                let current = self.current_bytes(&meta.path);
                let (added, removed) = match render_diff(&meta.path, &baseline, current.as_deref())
                {
                    Some(text) => count_lines(&text),
                    None => (0, 0),
                };
                ChangeEntry {
                    path: meta.path.clone(),
                    added,
                    removed,
                    baseline: baseline.kind(),
                    exists: current.is_some(),
                }
            })
            .collect()
    }

    /// Unified diff of one tracked path against its baseline.
    pub fn diff(&self, input: &str) -> Result<ChangeDiff, String> {
        let rel = normalize_rel(input)?;
        let meta = self.entry(&rel)?;
        let git = GitProbe::new(&self.root);
        let baseline = self.baseline_for(&meta, &git);
        let current = self.current_bytes(&rel);
        let diff = render_diff(&rel, &baseline, current.as_deref()).unwrap_or_default();
        let (added, removed) = count_lines(&diff);
        Ok(ChangeDiff {
            path: rel,
            diff,
            baseline: baseline.kind(),
            added,
            removed,
        })
    }

    /// Restore one tracked path to its baseline (exact bytes, or absence).
    /// This is not a tracked write: the snapshot stays as it was.
    pub fn revert(&self, input: &str) -> Result<RevertResult, String> {
        let rel = normalize_rel(input)?;
        let meta = self.entry(&rel)?;
        let git = GitProbe::new(&self.root);
        let baseline = self.baseline_for(&meta, &git);
        let full = bebok_tools::resolve_in_root(&self.root, &rel)?;
        let kind = baseline.kind();
        let exists = match baseline {
            Baseline::Git(bytes) | Baseline::Snapshot(Some(bytes)) => {
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("cannot create parent dir: {e}"))?;
                }
                std::fs::write(&full, bytes).map_err(|e| format!("cannot write {rel}: {e}"))?;
                true
            }
            Baseline::Snapshot(None) => {
                match std::fs::remove_file(&full) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("cannot remove {rel}: {e}")),
                }
                false
            }
        };
        Ok(RevertResult {
            path: rel,
            baseline: kind,
            exists,
        })
    }

    fn entry(&self, rel: &str) -> Result<SnapshotMeta, String> {
        self.read_index()
            .entries
            .into_iter()
            .find(|e| e.path == rel)
            .ok_or_else(|| format!("no tracked change for {rel}"))
    }

    fn baseline_for(&self, meta: &SnapshotMeta, git: &GitProbe) -> Baseline {
        if let Some(bytes) = git.head_bytes(&meta.path) {
            return Baseline::Git(bytes);
        }
        if !meta.existed {
            return Baseline::Snapshot(None);
        }
        let bytes = meta
            .file
            .as_ref()
            .and_then(|name| std::fs::read(self.changes_dir.join(name)).ok());
        // A vanished snapshot file degrades to "did not exist" rather than
        // failing the whole listing.
        Baseline::Snapshot(bytes)
    }

    fn current_bytes(&self, rel: &str) -> Option<Vec<u8>> {
        let full = bebok_tools::resolve_in_root(&self.root, rel).ok()?;
        std::fs::read(full).ok()
    }

    fn read_index(&self) -> Index {
        std::fs::read(self.changes_dir.join(INDEX_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn write_index(&self, index: &Index) -> Result<(), String> {
        let index_out = Index {
            version: 1,
            entries: index.entries.clone(),
        };
        let json = serde_json::to_vec_pretty(&index_out)
            .map_err(|e| format!("cannot encode index: {e}"))?;
        std::fs::write(self.changes_dir.join(INDEX_FILE), json)
            .map_err(|e| format!("cannot write index: {e}"))
    }
}

/// Canonical project-relative form: forward slashes, no `.` segments, no
/// traversal / absolute components (mirrors `resolve_in_root`'s rules).
pub fn normalize_rel(input: &str) -> Result<String, String> {
    let portable = input.trim().replace('\\', "/");
    let mut parts: Vec<String> = Vec::new();
    for component in Path::new(&portable).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            _ => return Err("path escapes project root".into()),
        }
    }
    if parts.is_empty() {
        return Err("path escapes project root".into());
    }
    Ok(parts.join("/"))
}

/// Render the unified diff for one path, or `None` when nothing changed.
fn render_diff(rel: &str, baseline: &Baseline, current: Option<&[u8]>) -> Option<String> {
    let (old_text, old_label) = match baseline {
        Baseline::Git(bytes) | Baseline::Snapshot(Some(bytes)) => (
            String::from_utf8_lossy(bytes).into_owned(),
            format!("a/{rel}"),
        ),
        Baseline::Snapshot(None) => (String::new(), "/dev/null".to_string()),
    };
    let (new_text, new_label) = match current {
        Some(bytes) => (
            String::from_utf8_lossy(bytes).into_owned(),
            format!("b/{rel}"),
        ),
        None => (String::new(), "/dev/null".to_string()),
    };
    let text =
        bebok_tools::diff::unified_diff(&old_text, &new_text, &old_label, &new_label, DIFF_CONTEXT);
    if text.is_empty() { None } else { Some(text) }
}

/// Count `+`/`-` body lines of a unified diff (skipping the two file headers).
pub fn count_lines(diff: &str) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for (i, line) in diff.lines().enumerate() {
        if i < 2 && (line.starts_with("---") || line.starts_with("+++")) {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

/// One-shot git probe for a project root: answers "is this a work tree?" once
/// and fetches `HEAD:<path>` for tracked files.
struct GitProbe {
    root: PathBuf,
    is_work_tree: bool,
}

impl GitProbe {
    fn new(root: &Path) -> Self {
        let is_work_tree = git(root, &["rev-parse", "--is-inside-work-tree"])
            .is_some_and(|out| out.status.success() && out.stdout.starts_with(b"true"));
        Self {
            root: root.to_path_buf(),
            is_work_tree,
        }
    }

    /// `HEAD` bytes of `rel` when the project is a git work tree, the file is
    /// tracked and `HEAD` has it; `None` otherwise (fall back to the snapshot).
    fn head_bytes(&self, rel: &str) -> Option<Vec<u8>> {
        if !self.is_work_tree {
            return None;
        }
        let tracked = git(&self.root, &["ls-files", "--error-unmatch", "--", rel])
            .is_some_and(|out| out.status.success());
        if !tracked {
            return None;
        }
        // `./<rel>` is resolved relative to the command's cwd (the project
        // root), which may itself sit below the repository root.
        let spec = format!("HEAD:./{rel}");
        let out = git(&self.root, &["show", &spec])?;
        out.status.success().then_some(out.stdout)
    }
}

/// Run `git` in `cwd`; `None` when the executable is missing or fails to spawn.
fn git(cwd: &Path, args: &[&str]) -> Option<std::process::Output> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: never flash a console when the engine runs headless.
        cmd.creation_flags(0x0800_0000);
    }
    cmd.output().ok()
}

/// True when a `git` executable is reachable on this host (tests skip
/// git-dependent assertions otherwise).
pub fn git_available() -> bool {
    git(&std::env::temp_dir(), &["--version"]).is_some_and(|out| out.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        base: PathBuf,
        root: PathBuf,
        session: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let base =
                std::env::temp_dir().join(format!("bebok-changes-{tag}-{}", uuid::Uuid::new_v4()));
            let root = base.join("root");
            let session = base.join("session");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::create_dir_all(&session).unwrap();
            Self {
                base,
                root,
                session,
            }
        }

        fn tracker(&self) -> Tracker {
            Tracker::new(&self.root, &self.session)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn normalizes_and_rejects_paths() {
        assert_eq!(normalize_rel("./src\\a.rs").unwrap(), "src/a.rs");
        assert_eq!(normalize_rel("a/./b.txt").unwrap(), "a/b.txt");
        for bad in ["", ".", "../x", "/abs", r"C:\x", "a/../../b"] {
            assert!(normalize_rel(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn counts_plus_minus_lines_excluding_headers() {
        let diff = "--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n-old\n+new\n--x\n keep";
        assert_eq!(count_lines(diff), (1, 2));
        assert_eq!(count_lines(""), (0, 0));
    }

    #[test]
    fn snapshots_existing_file_once() {
        let fx = Fixture::new("existing");
        std::fs::write(fx.root.join("a.txt"), "one\ntwo\n").unwrap();
        let tracker = fx.tracker();
        assert!(tracker.snapshot_before_write("a.txt").unwrap());
        // Simulate the tool write, then a second write: no new snapshot.
        std::fs::write(fx.root.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        assert!(!tracker.snapshot_before_write("./a.txt").unwrap());
        assert!(fx.session.join(CHANGES_DIR).join("0001.snap").exists());
        assert_eq!(
            std::fs::read_to_string(fx.session.join(CHANGES_DIR).join("0001.snap")).unwrap(),
            "one\ntwo\n"
        );

        let list = tracker.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, "a.txt");
        assert_eq!((list[0].added, list[0].removed), (2, 1));
        assert_eq!(list[0].baseline, BaselineKind::Snapshot);
        assert!(list[0].exists);

        let diff = tracker.diff("a.txt").unwrap();
        assert!(diff.diff.contains("-two\n"), "{}", diff.diff);
        assert!(diff.diff.contains("+TWO\n"));
        assert!(diff.diff.contains("+three"));
        assert_eq!(diff.baseline, BaselineKind::Snapshot);
    }

    #[test]
    fn new_file_records_absence_and_revert_removes_it() {
        let fx = Fixture::new("newfile");
        let tracker = fx.tracker();
        assert!(tracker.snapshot_before_write("sub/new.txt").unwrap());
        // The changes dir exists lazily, without a snapshot file.
        assert!(fx.session.join(CHANGES_DIR).join(INDEX_FILE).exists());
        assert!(!fx.session.join(CHANGES_DIR).join("0001.snap").exists());

        // Nothing written yet: the file is listed as absent with 0/0.
        let list = tracker.list();
        assert_eq!(list[0].path, "sub/new.txt");
        assert!(!list[0].exists);
        assert_eq!((list[0].added, list[0].removed), (0, 0));

        std::fs::create_dir_all(fx.root.join("sub")).unwrap();
        std::fs::write(fx.root.join("sub/new.txt"), "a\nb\n").unwrap();
        let diff = tracker.diff("sub\\new.txt").unwrap();
        assert!(diff.diff.starts_with("--- /dev/null\n+++ b/sub/new.txt\n"));
        assert_eq!((diff.added, diff.removed), (2, 0));

        let reverted = tracker.revert("sub/new.txt").unwrap();
        assert!(!reverted.exists);
        assert!(!fx.root.join("sub/new.txt").exists());
        // Reverting twice is harmless.
        assert!(tracker.revert("sub/new.txt").is_ok());
    }

    #[test]
    fn revert_restores_exact_snapshot_bytes() {
        let fx = Fixture::new("revert");
        let original = b"\xEF\xBB\xBFline\r\nbytes \xFF\x00end";
        std::fs::write(fx.root.join("bin.dat"), original).unwrap();
        let tracker = fx.tracker();
        tracker.snapshot_before_write("bin.dat").unwrap();
        std::fs::write(fx.root.join("bin.dat"), "changed").unwrap();
        let reverted = tracker.revert("bin.dat").unwrap();
        assert!(reverted.exists);
        assert_eq!(std::fs::read(fx.root.join("bin.dat")).unwrap(), original);
        // A tracked path can be reverted after the agent deleted the file.
        std::fs::remove_file(fx.root.join("bin.dat")).unwrap();
        assert!(!tracker.list()[0].exists);
        tracker.revert("bin.dat").unwrap();
        assert_eq!(std::fs::read(fx.root.join("bin.dat")).unwrap(), original);
    }

    #[test]
    fn rejects_escaping_paths_and_unknown_entries() {
        let fx = Fixture::new("escape");
        let tracker = fx.tracker();
        assert!(tracker.snapshot_before_write("../outside.txt").is_err());
        assert!(tracker.snapshot_before_write("/etc/passwd").is_err());
        assert!(tracker.diff("nope.txt").is_err());
        assert!(tracker.revert("nope.txt").is_err());
        assert!(tracker.list().is_empty());
    }

    #[test]
    fn git_tracked_file_diffs_against_head() {
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let fx = Fixture::new("git");
        let run = |args: &[&str]| {
            let out = git(&fx.root, args).expect("git spawns");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "t"]);
        run(&["config", "core.autocrlf", "false"]);
        std::fs::write(fx.root.join("tracked.txt"), "committed\n").unwrap();
        std::fs::write(fx.root.join("untracked.txt"), "loose\n").unwrap();
        run(&["add", "tracked.txt"]);
        run(&["commit", "-q", "-m", "init"]);
        // The session's first write sees a working copy that already drifted
        // from HEAD: the diff must still be against HEAD, not the snapshot.
        std::fs::write(fx.root.join("tracked.txt"), "committed\nlocal edit\n").unwrap();

        let tracker = fx.tracker();
        tracker.snapshot_before_write("tracked.txt").unwrap();
        tracker.snapshot_before_write("untracked.txt").unwrap();
        std::fs::write(
            fx.root.join("tracked.txt"),
            "committed\nlocal edit\nagent\n",
        )
        .unwrap();
        std::fs::write(fx.root.join("untracked.txt"), "loose\nagent\n").unwrap();

        let tracked = tracker.diff("tracked.txt").unwrap();
        assert_eq!(tracked.baseline, BaselineKind::Git);
        assert!(tracked.diff.contains("+local edit\n"), "{}", tracked.diff);
        assert!(tracked.diff.contains("+agent"));
        assert_eq!((tracked.added, tracked.removed), (2, 0));

        let untracked = tracker.diff("untracked.txt").unwrap();
        assert_eq!(untracked.baseline, BaselineKind::Snapshot);
        assert_eq!((untracked.added, untracked.removed), (1, 0));

        let list = tracker.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].baseline, BaselineKind::Git);
        assert_eq!(list[1].baseline, BaselineKind::Snapshot);

        let reverted = tracker.revert("tracked.txt").unwrap();
        assert_eq!(reverted.baseline, BaselineKind::Git);
        assert_eq!(
            std::fs::read_to_string(fx.root.join("tracked.txt")).unwrap(),
            "committed\n"
        );
    }
}
