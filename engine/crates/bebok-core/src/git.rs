//! Git CLI helpers (WP-GIT / F6-14, F6-15): repository probing for
//! `GET /projects/{id}/git` and worktree-backed sessions.
//!
//! Everything shells out to the `git` executable via `tokio::process::Command`
//! (never `git2`/libgit2). Arguments are always passed as separate elements,
//! never a single shell-quoted string, so the same code runs on Windows and
//! Unix. Every invocation has a hard timeout and is killed on drop; a missing
//! binary, a non-repo directory, a detached HEAD or a missing remote all
//! degrade to `None`/`false` answers instead of errors.
//!
//! Worktrees created for sessions live under `<root>/.bebok/worktrees/<branch>`
//! (built with `Path::join`, never string concatenation). `.bebok/.gitignore`
//! is written (if missing) so the directory never shows up in `git status`.
//!
//! `change_tracking.rs` (WP-CHANGES) keeps its own private one-shot `git()`
//! runner for HEAD lookups; this module is the shared, public surface for
//! everything else and intentionally mirrors that helper's shape.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::util::normalize_path;

/// Hard cap for a single `git` invocation. Worktree creation on a large
/// repository can take a few seconds; probes are near-instant.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Path segments below the project root that hold session worktrees.
pub const WORKTREES_SUBDIR: [&str; 2] = [".bebok", "worktrees"];

/// Captured result of one `git` run.
#[derive(Debug, Clone)]
pub struct GitOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl GitOutput {
    /// Trimmed stdout when the command succeeded, else `None`.
    pub fn ok_line(&self) -> Option<String> {
        if !self.success {
            return None;
        }
        let line = self.stdout.trim();
        (!line.is_empty()).then(|| line.to_string())
    }
}

/// Failure of a git-backed operation, mapped to HTTP codes by the route layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitError {
    /// `git` is not on `PATH` (or failed to spawn / timed out).
    Unavailable,
    /// The directory is not inside a git work tree.
    NotRepo(String),
    /// Caller input rejected before anything was run (bad branch, bad path).
    InvalidInput(String),
    /// `git` ran and reported a failure (`stderr` attached).
    Failed(String),
    /// Filesystem failure around the git call.
    Io(String),
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(f, "git is not available on this host"),
            Self::NotRepo(p) => write!(f, "not a git repository: {p}"),
            Self::InvalidInput(m) => write!(f, "{m}"),
            Self::Failed(m) => write!(f, "git failed: {m}"),
            Self::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for GitError {}

impl From<GitError> for crate::error::CoreError {
    fn from(e: GitError) -> Self {
        match e {
            GitError::InvalidInput(m) | GitError::NotRepo(m) => Self::BadRequest(m),
            other => Self::Other(other.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// runner
// ---------------------------------------------------------------------------

/// Run `git <args>` in `cwd`. `None` when the executable is missing, fails to
/// spawn, or exceeds [`GIT_TIMEOUT`] (the child is killed). A non-zero exit
/// is *not* `None` - inspect [`GitOutput::success`].
pub async fn run(cwd: &Path, args: &[&str]) -> Option<GitOutput> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Never block on a credential/editor prompt from a headless engine.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: never flash a console when the engine runs headless.
        cmd.creation_flags(0x0800_0000);
    }
    let output = tokio::time::timeout(GIT_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    Some(GitOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// True when a `git` executable answers `--version` on this host (tests skip
/// git-dependent assertions otherwise).
pub async fn git_available() -> bool {
    run(&std::env::temp_dir(), &["--version"])
        .await
        .is_some_and(|out| out.success)
}

// ---------------------------------------------------------------------------
// F6-14: repository probe
// ---------------------------------------------------------------------------

/// `GET /projects/{id}/git` payload. `is_repo == false` leaves every other
/// field at its null/zero default (missing git binary included).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GitInfo {
    pub is_repo: bool,
    /// `git rev-parse --show-toplevel`, as reported by git (forward slashes).
    pub root: Option<String>,
    /// `git rev-parse --abbrev-ref HEAD`; `None` on a detached HEAD.
    pub branch: Option<String>,
    /// `git remote get-url origin`; `None` when there is no `origin`.
    pub remote_url: Option<String>,
    /// `remote_url` points at `github.com`.
    pub is_github: bool,
    /// Number of `git status --porcelain` lines (staged, unstaged, untracked).
    pub dirty_count: Option<usize>,
}

/// Probe `dir`. Never fails: any git trouble yields `is_repo: false`.
pub async fn inspect(dir: &Path) -> GitInfo {
    if !dir.is_dir() {
        return GitInfo::default();
    }
    let is_repo = run(dir, &["rev-parse", "--is-inside-work-tree"])
        .await
        .is_some_and(|out| out.success && out.stdout.trim() == "true");
    if !is_repo {
        return GitInfo::default();
    }
    let root = run(dir, &["rev-parse", "--show-toplevel"])
        .await
        .and_then(|out| out.ok_line());
    let branch = run(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .and_then(|out| out.ok_line())
        // A detached HEAD prints literally `HEAD`.
        .filter(|b| b != "HEAD");
    let remote_url = run(dir, &["remote", "get-url", "origin"])
        .await
        .and_then(|out| out.ok_line());
    let is_github = remote_url
        .as_deref()
        .is_some_and(|url| url.to_ascii_lowercase().contains("github.com"));
    let dirty_count = run(dir, &["status", "--porcelain"])
        .await
        .filter(|out| out.success)
        .map(|out| out.stdout.lines().filter(|l| !l.trim().is_empty()).count());
    GitInfo {
        is_repo,
        root,
        branch,
        remote_url,
        is_github,
        dirty_count,
    }
}

// ---------------------------------------------------------------------------
// F6-15: worktrees
// ---------------------------------------------------------------------------

/// `<root>/.bebok/worktrees`.
pub fn worktrees_dir(root: &Path) -> PathBuf {
    WORKTREES_SUBDIR
        .iter()
        .fold(root.to_path_buf(), |p, seg| p.join(seg))
}

/// Validate a branch name the client asked for. Beyond git's own ref rules,
/// the name doubles as the worktree's relative path below `.bebok/worktrees`,
/// so it must be a safe relative path: only `[A-Za-z0-9._/-]`, no empty or
/// `.`/`..` segments, no leading `-`/`.`, no trailing `/` or `.lock`.
pub fn validate_branch(branch: &str) -> Result<(), GitError> {
    let bad = |m: &str| Err(GitError::InvalidInput(format!("invalid branch name: {m}")));
    if branch.is_empty() {
        return bad("empty");
    }
    if branch.len() > 200 {
        return bad("too long");
    }
    if !branch
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
    {
        return bad("only letters, digits, '.', '_', '-' and '/' are allowed");
    }
    if branch.starts_with('-') || branch.starts_with('.') || branch.starts_with('/') {
        return bad("must not start with '-', '.' or '/'");
    }
    if branch.ends_with('/') || branch.ends_with('.') || branch.ends_with(".lock") {
        return bad("must not end with '/', '.' or '.lock'");
    }
    if branch.contains("..") || branch.contains("//") || branch.contains("@{") {
        return bad("must not contain '..', '//' or '@{'");
    }
    for segment in branch.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." || segment.starts_with('.') {
            return bad("invalid path segment");
        }
    }
    Ok(())
}

/// Worktree directory for `branch` below `root` (validated).
pub fn worktree_path(root: &Path, branch: &str) -> Result<PathBuf, GitError> {
    validate_branch(branch)?;
    let mut path = worktrees_dir(root);
    for segment in branch.split('/') {
        path.push(segment);
    }
    Ok(path)
}

/// A session directory recognised as a Bebok worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    /// The project root that owns `.bebok/worktrees/`.
    pub root: PathBuf,
    /// Branch name (path below `.bebok/worktrees`, `/`-joined).
    pub branch: String,
}

/// Recognise `<root>/.bebok/worktrees/<branch...>` from its components alone -
/// no filesystem access, no separator assumptions. `None` for any other path.
pub fn worktree_info(directory: &Path) -> Option<WorktreeInfo> {
    let components: Vec<Component<'_>> = directory.components().collect();
    let names: Vec<Option<&str>> = components
        .iter()
        .map(|c| match c {
            Component::Normal(n) => n.to_str(),
            _ => None,
        })
        .collect();
    // Last occurrence wins, so a project that itself lives inside another
    // project's worktree still resolves to the nearest owner.
    let index = (0..names.len().saturating_sub(2)).rev().find(|&i| {
        names[i] == Some(WORKTREES_SUBDIR[0]) && names[i + 1] == Some(WORKTREES_SUBDIR[1])
    })?;
    let branch_parts: Vec<&str> = names[index + 2..]
        .iter()
        .map(|n| (*n).unwrap_or(""))
        .collect();
    if branch_parts.is_empty() || branch_parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let root: PathBuf = components[..index].iter().collect();
    Some(WorktreeInfo {
        root,
        branch: branch_parts.join("/"),
    })
}

/// True when `directory` is a worktree owned by `root`.
pub fn is_worktree_of(directory: &Path, root: &Path) -> bool {
    worktree_info(directory).is_some_and(|info| same_path(&info.root, root))
}

/// Make sure `<root>/.bebok/.gitignore` ignores `worktrees/`. Written only
/// when missing (or when it lacks the entry) - the project's own top-level
/// `.gitignore` is never touched, because that is the user's file.
pub fn ensure_worktrees_ignored(root: &Path) -> Result<(), GitError> {
    let dot_bebok = root.join(WORKTREES_SUBDIR[0]);
    std::fs::create_dir_all(&dot_bebok).map_err(|e| GitError::Io(e.to_string()))?;
    let ignore = dot_bebok.join(".gitignore");
    let entry = format!("{}/", WORKTREES_SUBDIR[1]);
    let existing = std::fs::read_to_string(&ignore).unwrap_or_default();
    let already = existing.lines().any(|l| {
        let l = l.trim();
        l == entry || l == WORKTREES_SUBDIR[1] || l == format!("/{entry}") || l == "*"
    });
    if already {
        return Ok(());
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if text.is_empty() {
        text.push_str("# Written by Bebok: git worktrees of \"Run in a git worktree\" sessions.\n");
    }
    text.push_str(&entry);
    text.push('\n');
    std::fs::write(&ignore, text).map_err(|e| GitError::Io(e.to_string()))
}

/// Create `<root>/.bebok/worktrees/<branch>` with `git worktree add`.
///
/// A branch that already exists locally is checked out as-is (`base` is
/// ignored); otherwise it is created from `base` (default: the current
/// `HEAD`). The worktree's `.bebok/` is seeded with the project's own
/// `.bebok/` files (config, agents, ...) that are not already present, so a
/// gitignored project config still applies inside the worktree.
/// Returns the worktree path (not yet normalised).
pub async fn add_worktree(
    root: &Path,
    branch: &str,
    base: Option<&str>,
) -> Result<PathBuf, GitError> {
    let path = worktree_path(root, branch)?;
    if let Some(base) = base {
        validate_ref(base)?;
    }
    if !root.is_dir() {
        return Err(GitError::NotRepo(root.display().to_string()));
    }
    let is_repo = run(root, &["rev-parse", "--is-inside-work-tree"])
        .await
        .ok_or(GitError::Unavailable)?;
    if !(is_repo.success && is_repo.stdout.trim() == "true") {
        return Err(GitError::NotRepo(root.display().to_string()));
    }
    if path.exists() {
        return Err(GitError::InvalidInput(format!(
            "worktree already exists: {}",
            path.display()
        )));
    }
    ensure_worktrees_ignored(root)?;

    let path_arg = path.to_string_lossy().to_string();
    let branch_ref = format!("refs/heads/{branch}");
    let exists = run(root, &["rev-parse", "--verify", "--quiet", &branch_ref])
        .await
        .ok_or(GitError::Unavailable)?
        .success;
    let out = if exists {
        run(root, &["worktree", "add", &path_arg, branch]).await
    } else {
        let mut args = vec!["worktree", "add", "-b", branch, &path_arg];
        if let Some(base) = base {
            args.push(base);
        }
        run(root, &args).await
    }
    .ok_or(GitError::Unavailable)?;
    if !out.success {
        return Err(GitError::Failed(stderr_summary(&out)));
    }
    seed_bebok_dir(root, &path);
    Ok(path)
}

/// Validate `path` as a worktree of `root` that is safe to remove: it must
/// resolve to a location strictly below `<root>/.bebok/worktrees` (so this can
/// never become an arbitrary `rm -rf`). Returns the normalised path.
pub fn validate_worktree_removal(root: &Path, path: &Path) -> Result<PathBuf, GitError> {
    if !path.is_absolute() {
        return Err(GitError::InvalidInput(
            "worktree path must be absolute".to_string(),
        ));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(GitError::InvalidInput(
            "worktree path must not contain '..'".to_string(),
        ));
    }
    // Normalise both sides (canonical when they exist, verbatim prefix
    // stripped) so symlinks and Windows case / 8.3 spellings cannot sneak a
    // foreign path in; the raw comparison covers a worktree directory that is
    // already gone from disk.
    let resolved = PathBuf::from(normalize_path(path));
    let resolved_root = PathBuf::from(normalize_path(root));
    if !(is_worktree_of(&resolved, &resolved_root) || is_worktree_of(path, root)) {
        return Err(GitError::InvalidInput(format!(
            "not a Bebok worktree of this project: {}",
            path.display()
        )));
    }
    Ok(resolved)
}

/// `git worktree remove --force <path>` after [`validate_worktree_removal`].
/// `--force` because the user has explicitly confirmed removal of a worktree
/// whose session is already gone; the branch itself is kept. A path that git
/// no longer knows (already pruned) is removed from disk directly.
pub async fn remove_worktree(root: &Path, path: &Path) -> Result<PathBuf, GitError> {
    let resolved = validate_worktree_removal(root, path)?;
    let path_arg = resolved.to_string_lossy().to_string();
    let out = run(root, &["worktree", "remove", "--force", &path_arg])
        .await
        .ok_or(GitError::Unavailable)?;
    if !out.success {
        if resolved.exists() {
            return Err(GitError::Failed(stderr_summary(&out)));
        }
        // Not registered with git any more: nothing to do.
    }
    if resolved.exists() {
        std::fs::remove_dir_all(&resolved).map_err(|e| GitError::Io(e.to_string()))?;
    }
    // Drop stale administrative entries git may have left behind.
    let _ = run(root, &["worktree", "prune"]).await;
    Ok(resolved)
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// `base` must look like a ref/commit-ish, never an option (`-x`) or a path
/// with parent traversal - it is passed verbatim to `git worktree add`.
fn validate_ref(base: &str) -> Result<(), GitError> {
    let trimmed = base.trim();
    if trimmed.is_empty() {
        return Err(GitError::InvalidInput("base ref is empty".to_string()));
    }
    if trimmed.starts_with('-') || trimmed.contains("..") || trimmed.chars().any(char::is_whitespace)
    {
        return Err(GitError::InvalidInput(format!(
            "invalid base ref: {trimmed}"
        )));
    }
    Ok(())
}

fn stderr_summary(out: &GitOutput) -> String {
    let text = if out.stderr.trim().is_empty() {
        out.stdout.trim()
    } else {
        out.stderr.trim()
    };
    if text.is_empty() {
        "unknown error".to_string()
    } else {
        text.to_string()
    }
}

/// Copy `<root>/.bebok/*` (except `worktrees/` and `.gitignore`) into the new
/// worktree's `.bebok/`, skipping files that already exist there (a tracked
/// `.bebok/` wins). Best effort: failures are logged, never fatal.
fn seed_bebok_dir(root: &Path, worktree: &Path) {
    let source = root.join(WORKTREES_SUBDIR[0]);
    if !source.is_dir() {
        return;
    }
    let target = worktree.join(WORKTREES_SUBDIR[0]);
    if let Err(e) = copy_missing(&source, &target, true) {
        tracing::warn!(
            "could not seed {} from {}: {e}",
            target.display(),
            source.display()
        );
    }
}

fn copy_missing(source: &Path, target: &Path, top: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if top && (name == WORKTREES_SUBDIR[1] || name == ".gitignore") {
            continue;
        }
        let from = entry.path();
        let to = target.join(&name);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_missing(&from, &to, false)?;
        } else if file_type.is_file() && !to.exists() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Path equality for owner checks: byte-exact on Unix, case-insensitive on
/// Windows (both sides normally come from `normalize_path`, but a client may
/// echo a differently-cased drive letter).
fn same_path(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Throwaway directory under the OS temp dir (no drive letters or home
    /// directory assumed); removed on drop.
    struct Fixture {
        base: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir().join(format!("bebok-git-{tag}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&base).unwrap();
            Self { base }
        }

        fn dir(&self, name: &str) -> PathBuf {
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

    /// Run `git <args>` in `cwd` and require success (test-only).
    async fn git_ok(cwd: &Path, args: &[&str]) -> GitOutput {
        let out = run(cwd, args).await.expect("git spawns");
        assert!(out.success, "git {args:?} failed: {}", out.stderr);
        out
    }

    /// `git init` + identity + one commit on `main`, so `HEAD` resolves.
    async fn init_repo(dir: &Path) {
        git_ok(dir, &["init", "-q"]).await;
        git_ok(dir, &["symbolic-ref", "HEAD", "refs/heads/main"]).await;
        git_ok(dir, &["config", "user.email", "bebok@example.com"]).await;
        git_ok(dir, &["config", "user.name", "Bebok Test"]).await;
        git_ok(dir, &["config", "commit.gpgsign", "false"]).await;
        std::fs::write(dir.join("README.md"), "hello\n").unwrap();
        git_ok(dir, &["add", "README.md"]).await;
        git_ok(dir, &["commit", "-q", "-m", "init"]).await;
    }

    macro_rules! require_git {
        () => {
            if !git_available().await {
                eprintln!("skipping: git not on PATH");
                return;
            }
        };
    }

    // -- pure helpers (no git needed) --------------------------------------

    #[test]
    fn worktrees_dir_is_built_from_path_segments() {
        let root = Path::new("proj");
        assert_eq!(
            worktrees_dir(root),
            Path::new("proj").join(".bebok").join("worktrees")
        );
    }

    #[test]
    fn validate_branch_accepts_safe_names_and_rejects_traversal() {
        for ok in ["bebok/session-1a2b", "feature", "a.b-c_d", "x/y/z"] {
            assert!(validate_branch(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "-x", ".hidden", "a/../b", "a//b", "a/", "/a", "a b", "a\\b", "a.lock",
            "a/.git", "x@{1}", "ü",
        ] {
            assert!(validate_branch(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn worktree_path_nests_slash_separated_branches() {
        let root = Path::new("proj");
        let path = worktree_path(root, "bebok/feat").unwrap();
        assert_eq!(path, worktrees_dir(root).join("bebok").join("feat"));
        assert!(worktree_path(root, "../escape").is_err());
    }

    #[test]
    fn worktree_info_recognises_session_directories_by_components() {
        let root = std::env::temp_dir().join("proj");
        let wt = worktrees_dir(&root).join("bebok").join("feat");
        let info = worktree_info(&wt).expect("recognised");
        assert_eq!(info.root, root);
        assert_eq!(info.branch, "bebok/feat");
        assert!(is_worktree_of(&wt, &root));
        assert!(!is_worktree_of(&root, &root));
        assert!(worktree_info(&root).is_none());
        // The worktrees directory itself is not a worktree.
        assert!(worktree_info(&worktrees_dir(&root)).is_none());
        // A different owner is not matched.
        assert!(!is_worktree_of(&wt, &std::env::temp_dir().join("other")));
    }

    #[test]
    fn ensure_worktrees_ignored_writes_once_and_appends_when_missing() {
        let fx = Fixture::new("ignore");
        let root = fx.dir("proj");
        ensure_worktrees_ignored(&root).unwrap();
        let file = root.join(".bebok").join(".gitignore");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.lines().any(|l| l == "worktrees/"), "{text}");
        // Idempotent.
        ensure_worktrees_ignored(&root).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
        // An existing file without the entry gets it appended (no newline
        // at the end of the existing content).
        std::fs::write(&file, "config.json").unwrap();
        ensure_worktrees_ignored(&root).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text, "config.json\nworktrees/\n");
    }

    #[test]
    fn validate_worktree_removal_refuses_anything_outside_the_worktrees_dir() {
        let fx = Fixture::new("removal");
        let root = fx.dir("proj");
        let other = fx.dir("other");
        assert!(validate_worktree_removal(&root, &other).is_err());
        assert!(validate_worktree_removal(&root, &root).is_err());
        assert!(validate_worktree_removal(&root, &worktrees_dir(&root)).is_err());
        assert!(validate_worktree_removal(&root, Path::new("relative/path")).is_err());
        assert!(validate_worktree_removal(&root, &worktrees_dir(&root).join("..").join("x")).is_err());
        // A genuine (even not-yet-existing) worktree path is accepted.
        let wt = worktrees_dir(&root).join("bebok").join("feat");
        assert!(validate_worktree_removal(&root, &wt).is_ok());
    }

    // -- against a real temp repository ------------------------------------

    #[tokio::test]
    async fn inspect_reports_not_a_repo_for_a_plain_directory() {
        let fx = Fixture::new("plain");
        let dir = fx.dir("plain");
        let info = inspect(&dir).await;
        assert_eq!(info, GitInfo::default());
        assert!(!info.is_repo);
        // A missing directory is equally quiet.
        assert_eq!(inspect(&fx.base.join("missing")).await, GitInfo::default());
    }

    #[tokio::test]
    async fn inspect_reports_branch_remote_and_dirty_count() {
        require_git!();
        let fx = Fixture::new("inspect");
        let repo = fx.dir("repo");
        init_repo(&repo).await;

        let clean = inspect(&repo).await;
        assert!(clean.is_repo);
        assert_eq!(clean.branch.as_deref(), Some("main"));
        assert!(clean.root.is_some());
        assert_eq!(clean.remote_url, None, "no remote yet");
        assert!(!clean.is_github);
        assert_eq!(clean.dirty_count, Some(0));

        git_ok(
            &repo,
            &["remote", "add", "origin", "https://github.com/acme/widgets.git"],
        )
        .await;
        std::fs::write(repo.join("README.md"), "changed\n").unwrap();
        std::fs::write(repo.join("new.txt"), "untracked\n").unwrap();

        let dirty = inspect(&repo).await;
        assert_eq!(
            dirty.remote_url.as_deref(),
            Some("https://github.com/acme/widgets.git")
        );
        assert!(dirty.is_github);
        assert_eq!(dirty.dirty_count, Some(2));

        // Detached HEAD -> no branch, still a repo.
        git_ok(&repo, &["checkout", "-q", "--detach"]).await;
        let detached = inspect(&repo).await;
        assert!(detached.is_repo);
        assert_eq!(detached.branch, None);
    }

    #[tokio::test]
    async fn add_and_remove_worktree_round_trip() {
        require_git!();
        let fx = Fixture::new("worktree");
        let repo = fx.dir("repo");
        init_repo(&repo).await;
        // A project config that should follow the session into the worktree.
        std::fs::create_dir_all(repo.join(".bebok")).unwrap();
        std::fs::write(repo.join(".bebok").join("config.json"), "{}").unwrap();

        let path = add_worktree(&repo, "bebok/session-1", None).await.unwrap();
        assert_eq!(path, worktrees_dir(&repo).join("bebok").join("session-1"));
        assert!(path.join("README.md").is_file(), "checkout materialised");
        assert!(path.join(".bebok").join("config.json").is_file(), "config seeded");
        assert_eq!(inspect(&path).await.branch.as_deref(), Some("bebok/session-1"));
        // The main checkout stays clean: `.bebok/worktrees` is ignored.
        let main = inspect(&repo).await;
        assert_eq!(main.branch.as_deref(), Some("main"));
        let status = git_ok(&repo, &["status", "--porcelain"]).await;
        assert!(
            !status.stdout.contains("worktrees"),
            "worktrees dir must be gitignored: {}",
            status.stdout
        );

        // Adding the same branch twice is refused before git runs.
        assert!(matches!(
            add_worktree(&repo, "bebok/session-1", None).await,
            Err(GitError::InvalidInput(_))
        ));
        // A bad base ref is refused.
        assert!(add_worktree(&repo, "bebok/other", Some("--bad")).await.is_err());
        assert!(add_worktree(&repo, "bebok/other", Some("no-such-ref")).await.is_err());

        // Removal only accepts paths inside `.bebok/worktrees`.
        assert!(remove_worktree(&repo, &repo).await.is_err());
        assert!(remove_worktree(&repo, &fx.base).await.is_err());
        // Dirty worktree still goes (explicit user confirmation, --force).
        std::fs::write(path.join("README.md"), "dirty\n").unwrap();
        remove_worktree(&repo, &path).await.unwrap();
        assert!(!path.exists());
        let list = git_ok(&repo, &["worktree", "list", "--porcelain"]).await;
        assert!(!list.stdout.contains("session-1"), "{}", list.stdout);
        // The branch survives the worktree removal.
        assert!(
            git_ok(&repo, &["branch", "--list", "bebok/session-1"])
                .await
                .stdout
                .contains("bebok/session-1")
        );
        // Re-adding an existing branch checks it out again (base ignored).
        let again = add_worktree(&repo, "bebok/session-1", Some("main")).await.unwrap();
        assert!(again.join("README.md").is_file());
    }

    #[tokio::test]
    async fn add_worktree_rejects_a_non_repository() {
        require_git!();
        let fx = Fixture::new("norepo");
        let plain = fx.dir("plain");
        assert!(matches!(
            add_worktree(&plain, "bebok/x", None).await,
            Err(GitError::NotRepo(_))
        ));
        assert!(matches!(
            add_worktree(&fx.base.join("missing"), "bebok/x", None).await,
            Err(GitError::NotRepo(_))
        ));
    }
}
