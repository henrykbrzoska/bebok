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

/// `.bebok/` segment (shared parent of worktrees and plugin slots in
/// `<root>/.bebok/.gitignore`).
const PLUGINS_SLOT_PARENT: &str = ".bebok";
/// Gitignore entry hiding local plugin checkout slots while keeping
/// `plugins/*.json` declarations committable (directory-only pattern).
pub const PLUGINS_SLOT_ENTRY: &str = "plugins/*/";

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
    /// Credential / authentication failure on a remote operation.
    AuthFailed(String),
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
            Self::AuthFailed(m) => write!(f, "git authentication failed: {m}"),
            Self::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for GitError {}

impl From<GitError> for crate::error::CoreError {
    fn from(e: GitError) -> Self {
        match e {
            GitError::InvalidInput(m) | GitError::NotRepo(m) => Self::BadRequest(m),
            GitError::AuthFailed(m) => Self::BadRequest(m),
            other => Self::Other(other.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// runner
// ---------------------------------------------------------------------------

/// Run `git <args>` in `cwd` with the standard short timeout. `None` when the
/// executable is missing, fails to spawn, or exceeds [`GIT_TIMEOUT`] (the
/// child is killed). A non-zero exit is *not* `None` — inspect
/// [`GitOutput::success`].
pub async fn run(cwd: &Path, args: &[&str]) -> Option<GitOutput> {
    run_timeout(cwd, args, GIT_TIMEOUT).await
}

/// Run `git <args>` in `cwd` with an explicit timeout. The public [`run`]
/// delegates to this with [`GIT_TIMEOUT`]; network-heavy ops (clone/fetch/pull/
/// push) use a longer timeout via [`exec_net`].
pub(crate) async fn run_timeout(cwd: &Path, args: &[&str], timeout: Duration) -> Option<GitOutput> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }
    let output = tokio::time::timeout(timeout, cmd.output())
        .await
        .ok()?
        .ok()?;
    Some(GitOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// Longer-running network operation: clone/fetch/pull/push. Uses a 300 s
/// timeout and maps credential / auth failures to [`GitError::AuthFailed`].
pub async fn exec_net(dir: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
    let out = run_timeout(dir, args, Duration::from_secs(300))
        .await
        .ok_or(GitError::Unavailable)?;
    if out.success {
        return Ok(out);
    }
    Err(map_git_auth(&out.stderr))
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
// Git panel (read ops): status / log / branches / show
// ---------------------------------------------------------------------------

/// One file row of `git status --porcelain=v1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitFileStatus {
    pub path: String,
    pub x: char,
    pub y: char,
    pub staged: bool,
    pub unstaged: bool,
    pub untracked: bool,
    pub renamed_from: Option<String>,
}

/// Full working-tree status: branch, ahead/behind vs upstream, per-file rows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GitStatus {
    pub branch: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub files: Vec<GitFileStatus>,
}

/// Parse `git status --porcelain=v1 -b` output (without `-z`).
fn parse_status_porcelain(text: &str) -> GitStatus {
    let mut status = GitStatus::default();
    let mut pending_rename: Option<String> = None;
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            // `## main...origin/main [ahead 1, behind 2]`,
            // `## HEAD (no branch)`, `## No commits yet on main`.
            if let Some(rest) = header.strip_prefix("No commits yet on ") {
                status.branch = Some(rest.split_whitespace().next().unwrap_or(rest).to_string());
            } else if header.starts_with("HEAD (no branch)") {
                status.branch = None;
            } else {
                let branch = header
                    .split("...")
                    .next()
                    .unwrap_or(header)
                    .split_whitespace()
                    .next()
                    .unwrap_or(header);
                status.branch = Some(branch.to_string());
                for part in header.split([',', '[', ']']) {
                    let part = part.trim();
                    if let Some(n) = part.strip_prefix("ahead ") {
                        status.ahead = n.parse().unwrap_or(0);
                    } else if let Some(n) = part.strip_prefix("behind ") {
                        status.behind = n.parse().unwrap_or(0);
                    }
                }
            }
            continue;
        }
        let mut chars = line.chars();
        let (Some(x), Some(y)) = (chars.next(), chars.next()) else {
            continue;
        };
        let path = line.get(3..).unwrap_or("").to_string();
        if path.is_empty() {
            continue;
        }
        // Renames: `R  new <- old` in non-`-z` porcelain.
        if x == 'R' || y == 'R' {
            if let Some((new, old)) = path.split_once(" <- ") {
                status.files.push(GitFileStatus {
                    path: new.to_string(),
                    x,
                    y,
                    staged: true,
                    unstaged: y != ' ' && y != '?',
                    untracked: false,
                    renamed_from: Some(old.to_string()),
                });
                continue;
            }
            pending_rename = Some(path);
            continue;
        }
        if let Some(old) = pending_rename.take() {
            let _ = old;
        }
        status.files.push(GitFileStatus {
            path,
            x,
            y,
            staged: x != ' ' && x != '?',
            unstaged: y != ' ' && y != '?',
            untracked: x == '?' && y == '?',
            renamed_from: None,
        });
    }
    status
}

/// `git status` for `dir`: branch, ahead/behind, per-file rows.
/// No upstream -> ahead/behind 0. Non-repo -> `NotRepo`.
pub async fn status(dir: &Path) -> Result<GitStatus, GitError> {
    if !dir.is_dir() {
        return Err(GitError::NotRepo(dir.display().to_string()));
    }
    let out = run(
        dir,
        &["status", "--porcelain=v1", "-b", "--untracked-files=all"],
    )
    .await
    .ok_or(GitError::Unavailable)?;
    if !out.success {
        return Err(GitError::NotRepo(dir.display().to_string()));
    }
    let mut status = parse_status_porcelain(&out.stdout);
    // Fall back to rev-list when the header carried no ahead/behind info
    // (older git, or unusual upstream configs).
    if status.ahead == 0
        && status.behind == 0
        && let Some(counts) = run(dir, &["rev-list", "--left-right", "--count", "@{u}...HEAD"])
            .await
            .filter(|o| o.success)
            .and_then(|o| {
                let mut it = o.stdout.split_whitespace();
                let (a, b) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?);
                Some((a, b))
            })
    {
        // rev-list prints `behind ahead` for `@{u}...HEAD` order.
        status.behind = counts.0;
        status.ahead = counts.1;
    }
    Ok(status)
}

/// One commit row for the history view (graph lanes are computed client-side
/// from `parents`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitLogEntry {
    pub hash: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    pub ts: i64,
    pub subject: String,
    pub body: String,
    pub refs: Vec<String>,
}

/// Reject anything that is not a safe relative path for `git log -- <path>`.
fn validate_rel_path(path: &str) -> Result<(), GitError> {
    let bad = |m: &str| Err(GitError::InvalidInput(format!("invalid path: {m}")));
    if path.is_empty() || path.len() > 1024 {
        return bad("empty or too long");
    }
    if path.starts_with('-') || path.starts_with('/') || path.contains('\0') {
        return bad("must be a relative path");
    }
    if path.contains("..") || path.chars().any(char::is_whitespace) {
        return bad("must not contain '..' or whitespace");
    }
    // Windows drive / UNC prefixes.
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        return bad("absolute paths are not allowed");
    }
    if path.starts_with("\\\\") {
        return bad("absolute paths are not allowed");
    }
    Ok(())
}

/// `git log` for `dir` (topo order, newest first). Empty repo -> empty vec.
pub async fn log(
    dir: &Path,
    limit: usize,
    skip: usize,
    path: Option<&str>,
) -> Result<Vec<GitLogEntry>, GitError> {
    if !dir.is_dir() {
        return Err(GitError::NotRepo(dir.display().to_string()));
    }
    if let Some(p) = path {
        validate_rel_path(p)?;
    }
    let limit = limit.clamp(1, 200).to_string();
    let skip = skip.to_string();
    let format = "--format=%H\x1f%P\x1f%an\x1f%ae\x1f%ct\x1f%D\x1f%s\x1f%b\x1e".to_string();
    let mut args = vec![
        "log",
        "--topo-order",
        "--parents",
        "--no-color",
        "--max-count",
        &limit,
        "--skip",
        &skip,
        &format,
    ];
    let owned;
    if let Some(p) = path {
        owned = p.to_string();
        // Follow renames so a file's history survives `git mv`.
        args.push("--follow");
        args.push("--");
        args.push(&owned);
    }
    let out = run(dir, &args).await.ok_or(GitError::Unavailable)?;
    if !out.success {
        // `your current branch does not have any commits yet` -> empty.
        if out.stderr.contains("does not have any commits yet")
            || out.stderr.contains("unknown revision")
        {
            return Ok(Vec::new());
        }
        return Err(GitError::Failed(stderr_summary(&out)));
    }
    let mut entries = Vec::new();
    for record in out.stdout.split('\x1e') {
        let record = record.trim_start_matches('\n');
        if record.trim().is_empty() {
            continue;
        }
        let mut f = record.splitn(8, '\x1f');
        let (
            Some(hash),
            Some(parents),
            Some(author),
            Some(email),
            Some(ts),
            Some(deco),
            Some(subject),
            body,
        ) = (
            f.next(),
            f.next(),
            f.next(),
            f.next(),
            f.next(),
            f.next(),
            f.next(),
            f.next(),
        )
        else {
            continue;
        };
        entries.push(GitLogEntry {
            hash: hash.trim().to_string(),
            parents: parents.split_whitespace().map(str::to_string).collect(),
            author: author.to_string(),
            email: email.to_string(),
            ts: ts.trim().parse().unwrap_or(0),
            subject: subject.to_string(),
            body: body.unwrap_or("").trim().to_string(),
            refs: deco
                .split(", ")
                .filter_map(|r| {
                    let r = r.trim().trim_start_matches("HEAD -> ").trim();
                    (!r.is_empty()).then(|| r.to_string())
                })
                .collect(),
        });
    }
    Ok(entries)
}

/// One local branch row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitBranch {
    pub name: String,
    pub current: bool,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub last_commit: String,
    pub last_ts: i64,
}

/// Local branches with upstream tracking info.
pub async fn branches(dir: &Path) -> Result<Vec<GitBranch>, GitError> {
    if !dir.is_dir() {
        return Err(GitError::NotRepo(dir.display().to_string()));
    }
    let current = run(dir, &["branch", "--show-current"])
        .await
        .ok_or(GitError::Unavailable)?;
    if !current.success {
        return Err(GitError::NotRepo(dir.display().to_string()));
    }
    let current = current.stdout.trim().to_string();
    let out = run(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname:short)%1f%(upstream:short)%1f%(upstream:track,nobracket)%1f%(objectname:short)%1f%(committerdate:unix)",
            "refs/heads",
        ],
    )
    .await
    .ok_or(GitError::Unavailable)?;
    if !out.success {
        return Err(GitError::Failed(stderr_summary(&out)));
    }
    let mut list = Vec::new();
    for line in out.stdout.lines() {
        let mut f = line.split('\x1f');
        let (Some(name), upstream, track, Some(last_commit), Some(last_ts)) =
            (f.next(), f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let (mut ahead, mut behind) = (0usize, 0usize);
        if let Some(t) = track {
            for part in t.split(", ") {
                let part = part.trim();
                if let Some(n) = part.strip_prefix("ahead ") {
                    ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix("behind ") {
                    behind = n.parse().unwrap_or(0);
                }
            }
        }
        list.push(GitBranch {
            name: name.to_string(),
            current: name == current,
            upstream: upstream.filter(|u| !u.is_empty()).map(str::to_string),
            ahead,
            behind,
            last_commit: last_commit.to_string(),
            last_ts: last_ts.trim().parse().unwrap_or(0),
        });
    }
    Ok(list)
}

/// One file touched by a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitShowFile {
    pub status: String,
    pub path: String,
}

/// Full `git show` payload: subject, unified diff, touched files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitShow {
    pub hash: String,
    pub subject: String,
    pub diff: String,
    pub files: Vec<GitShowFile>,
}

/// `git show <rev>` after [`validate_ref`]-style checks. Diff capped at 2 MB.
pub async fn show(dir: &Path, rev: &str) -> Result<GitShow, GitError> {
    if !dir.is_dir() {
        return Err(GitError::NotRepo(dir.display().to_string()));
    }
    validate_ref_public(rev)?;
    let out = run(
        dir,
        &["show", "--patch", "--no-color", "--no-ext-diff", rev, "--"],
    )
    .await
    .ok_or(GitError::Unavailable)?;
    if !out.success {
        return Err(GitError::Failed(stderr_summary(&out)));
    }
    let mut diff = out.stdout;
    if diff.len() > 2_000_000 {
        diff.truncate(2_000_000);
    }
    let names = run(dir, &["show", "--name-status", "--format=", rev, "--"])
        .await
        .ok_or(GitError::Unavailable)?;
    let mut files = Vec::new();
    if names.success {
        for line in names.stdout.lines() {
            let mut f = line.splitn(2, '\t');
            if let (Some(s), Some(p)) = (f.next(), f.next()) {
                files.push(GitShowFile {
                    status: s.to_string(),
                    path: p.to_string(),
                });
            }
        }
    }
    let hash = run(dir, &["rev-parse", rev])
        .await
        .and_then(|o| o.ok_line())
        .unwrap_or_default();
    let subject = run(dir, &["show", "--no-patch", "--format=%s", rev, "--"])
        .await
        .and_then(|o| o.ok_line())
        .unwrap_or_default();
    Ok(GitShow {
        hash,
        subject,
        diff,
        files,
    })
}

// ---------------------------------------------------------------------------
// Per-file read ops: diff hunks, blame
// ---------------------------------------------------------------------------

/// One unified-diff hunk header `@@ -old_start,old_lines +new_start,new_lines @@`.
/// An omitted count means 1 (git prints `@@ -3 +3 @@` for single-line hunks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Hunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
}

/// Hunks of one file's diff. Produced with `--unified=0`, so only the headers
/// carry information and no context lines are returned.
pub type Hunks = Vec<Hunk>;

/// Parse one `@@ -a,b +c,d @@ ...` header line.
fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@ ")?;
    let mut parts = rest.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let range = |s: &str| -> Option<(usize, usize)> {
        match s.split_once(',') {
            Some((start, lines)) => Some((start.parse().ok()?, lines.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (old_start, old_lines) = range(old)?;
    let (new_start, new_lines) = range(new)?;
    Some(Hunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
    })
}

fn parse_hunks(diff: &str) -> Hunks {
    diff.lines()
        .filter(|l| l.starts_with("@@"))
        .filter_map(parse_hunk_header)
        .collect()
}

/// Hunks of `path` against `rev` (default `HEAD`), working tree on the other
/// side. An untracked file falls back to `diff --no-index /dev/null <path>`,
/// so every line shows up as added.
pub async fn diff_file(dir: &Path, path: &str, rev: Option<&str>) -> Result<Hunks, GitError> {
    ensure_repo(dir).await?;
    validate_pathspec(path)?;
    let rev = rev
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or("HEAD");
    validate_ref_public(rev)?;
    let tracked = run(dir, &["ls-files", "--error-unmatch", "--", path])
        .await
        .ok_or(GitError::Unavailable)?
        .success;
    let out = if tracked {
        exec(
            dir,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--unified=0",
                rev,
                "--",
                path,
            ],
        )
        .await?
    } else {
        // `--no-index` exits 1 whenever the files differ, which is the normal
        // case here; only empty stdout plus stderr means a real failure.
        let out = run(
            dir,
            &[
                "diff",
                "--no-index",
                "--no-color",
                "--no-ext-diff",
                "--unified=0",
                "/dev/null",
                path,
            ],
        )
        .await
        .ok_or(GitError::Unavailable)?;
        if !out.success && out.stdout.is_empty() && !out.stderr.is_empty() {
            return Err(GitError::Failed(stderr_summary(&out)));
        }
        out
    };
    Ok(parse_hunks(&out.stdout))
}

/// One line of `git blame --line-porcelain` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlameLine {
    /// 1-based line number in the blamed revision (or working tree).
    pub line: usize,
    pub hash: String,
    pub author: String,
    pub author_mail: String,
    pub ts: i64,
    pub summary: String,
    pub content: String,
}

/// Parse `--line-porcelain` output: each line starts with a
/// `<hash> <orig> <final> [count]` header, carries full metadata, and ends
/// with the source text prefixed by a tab.
fn parse_blame_porcelain(text: &str) -> Vec<BlameLine> {
    let mut out = Vec::new();
    let mut cur: Option<BlameLine> = None;
    for raw in text.lines() {
        if let Some(content) = raw.strip_prefix('\t') {
            if let Some(mut line) = cur.take() {
                line.content = content.to_string();
                out.push(line);
            }
            continue;
        }
        let mut fields = raw.split(' ');
        let first = fields.next().unwrap_or("");
        let is_header = (first.len() == 40 || first.len() == 64)
            && first.bytes().all(|b| b.is_ascii_hexdigit());
        if is_header {
            // Skip the original line number, keep the final one.
            let line = fields.nth(1).and_then(|n| n.parse().ok()).unwrap_or(0);
            cur = Some(BlameLine {
                line,
                hash: first.to_string(),
                author: String::new(),
                author_mail: String::new(),
                ts: 0,
                summary: String::new(),
                content: String::new(),
            });
            continue;
        }
        let Some(entry) = cur.as_mut() else {
            continue;
        };
        if let Some(v) = raw.strip_prefix("author ") {
            entry.author = v.to_string();
        } else if let Some(v) = raw.strip_prefix("author-mail ") {
            entry.author_mail = v.trim_matches(|c| c == '<' || c == '>').to_string();
        } else if let Some(v) = raw.strip_prefix("author-time ") {
            entry.ts = v.parse().unwrap_or(0);
        } else if let Some(v) = raw.strip_prefix("summary ") {
            entry.summary = v.to_string();
        }
    }
    out
}

/// `git blame --line-porcelain [rev] -- <path>`. Without `rev` the working
/// tree is blamed (uncommitted lines show as "Not Committed Yet").
pub async fn blame_file(
    dir: &Path,
    path: &str,
    rev: Option<&str>,
) -> Result<Vec<BlameLine>, GitError> {
    ensure_repo(dir).await?;
    validate_pathspec(path)?;
    let rev = rev.map(str::trim).filter(|r| !r.is_empty());
    let mut args = vec!["blame", "--line-porcelain"];
    if let Some(r) = rev {
        validate_ref_public(r)?;
        args.push(r);
    }
    args.push("--");
    args.push(path);
    let out = exec(dir, &args).await?;
    Ok(parse_blame_porcelain(&out.stdout))
}

/// Largest blob [`file_content`] returns, in bytes.
pub const FILE_CONTENT_MAX_BYTES: u64 = 1_000_000;

/// Text of one file at a revision. `exists` is false when the path is not
/// present there (untracked, added later, deleted, or a directory).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileContent {
    pub content: String,
    pub exists: bool,
}

/// Content of `path` at `rev` (default `HEAD`), read with `git cat-file blob`
/// from `<rev>:./<path>` so it is relative to `dir` like the other per-file
/// ops and skips textconv / smudge filters. A missing path is `exists: false`,
/// not an error. Binary blobs and blobs over [`FILE_CONTENT_MAX_BYTES`] are
/// rejected with `InvalidInput`.
pub async fn file_content(
    dir: &Path,
    path: &str,
    rev: Option<&str>,
) -> Result<FileContent, GitError> {
    ensure_repo(dir).await?;
    validate_pathspec(path)?;
    let rev = rev
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or("HEAD");
    validate_ref_public(rev)?;
    if rev.contains(':') {
        return Err(GitError::InvalidInput(format!("invalid ref: {rev}")));
    }
    let object = format!("{rev}:./{path}");
    let object: &str = &object;
    let missing = FileContent {
        content: String::new(),
        exists: false,
    };

    let size = run(dir, &["cat-file", "-s", object])
        .await
        .ok_or(GitError::Unavailable)?;
    if !size.success {
        return Ok(missing);
    }
    let bytes: u64 = size.stdout.trim().parse().unwrap_or(u64::MAX);
    if bytes > FILE_CONTENT_MAX_BYTES {
        return Err(GitError::InvalidInput(format!(
            "file too large: {bytes} bytes (limit {FILE_CONTENT_MAX_BYTES})"
        )));
    }

    let out = run(dir, &["cat-file", "blob", object])
        .await
        .ok_or(GitError::Unavailable)?;
    if !out.success {
        // Size resolved but it is not a blob (a directory / tree at this path).
        return Ok(missing);
    }
    if out.stdout.contains('\0') {
        return Err(GitError::InvalidInput(
            "binary file: content is not shown".to_string(),
        ));
    }
    Ok(FileContent {
        content: out.stdout,
        exists: true,
    })
}

/// Public ref validation shared by `show` and later write ops: no options,
/// no whitespace, no parent traversal.
pub fn validate_ref_public(rev: &str) -> Result<(), GitError> {
    let trimmed = rev.trim();
    if trimmed.is_empty() {
        return Err(GitError::InvalidInput("ref is empty".to_string()));
    }
    if trimmed.starts_with('-')
        || trimmed.contains("..")
        || trimmed.chars().any(char::is_whitespace)
    {
        return Err(GitError::InvalidInput(format!("invalid ref: {trimmed}")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Git panel (write ops): stage / commit / reset / stash / checkout /
// branch / cherry-pick / conflicts
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Repository discovery: the registered project root is not always the repo
// root (e.g. project `~/site` with the repo in `~/site/htdocs/.git`).
// ---------------------------------------------------------------------------

/// `dir/.git` exists (a directory, or a file for worktrees / submodules).
fn has_dot_git(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// Directories never scanned for nested repos (big, vendored or ours).
const NESTED_REPO_SKIP: &[&str] = &["node_modules", "vendor", "target", "dist", "build"];

/// How deep below the project root [`project_repo_dir`] looks for a `.git`.
const NESTED_REPO_DEPTH: usize = 2;

/// Directory to run project-wide git commands in (status, branches, log, ...).
///
/// - `root` itself, or any of its ancestors, holds `.git` -> `root` (git finds
///   the repo by walking up, as before).
/// - otherwise the shallowest nested repo within [`NESTED_REPO_DEPTH`] levels
///   (alphabetical among equals), e.g. `root/htdocs`.
/// - nothing found -> `root` (callers then report `NotRepo` as before).
pub fn project_repo_dir(root: &Path) -> PathBuf {
    if root.ancestors().any(has_dot_git) {
        return root.to_path_buf();
    }
    let mut level = vec![root.to_path_buf()];
    for _ in 0..NESTED_REPO_DEPTH {
        let mut next = Vec::new();
        for dir in &level {
            let Ok(read) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut children: Vec<PathBuf> = read
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter(|e| {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    !name.starts_with('.') && !NESTED_REPO_SKIP.contains(&name.as_ref())
                })
                .map(|e| e.path())
                .collect();
            children.sort();
            next.extend(children);
        }
        if let Some(found) = next.iter().find(|d| has_dot_git(d)) {
            return found.clone();
        }
        level = next;
    }
    root.to_path_buf()
}

/// Repo directory for a file-scoped op on `rel` (relative to `root`) and the
/// path rewritten relative to that directory. Walks up from the file's
/// directory towards `root` and stops at the first `.git`, so a nested repo
/// (`root/htdocs/.git`) wins over the project root. No nested `.git` ->
/// `(root, rel)` unchanged. An invalid `rel` is passed through untouched so
/// the op's own validation reports it.
pub fn file_repo_dir(root: &Path, rel: &str) -> (PathBuf, String) {
    let unchanged = || (root.to_path_buf(), rel.to_string());
    let rel_path = Path::new(rel);
    if rel.is_empty()
        || !rel_path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return unchanged();
    }
    let full = root.join(rel_path);
    let start = if full.is_dir() {
        full.as_path()
    } else {
        match full.parent() {
            Some(p) => p,
            None => return unchanged(),
        }
    };
    for dir in start.ancestors() {
        if dir == root || !dir.starts_with(root) {
            break;
        }
        if has_dot_git(dir) {
            let Ok(inner) = full.strip_prefix(dir) else {
                return unchanged();
            };
            let inner = inner.to_string_lossy().replace('\\', "/");
            // `rel` was the repo dir itself -> the whole repo.
            let inner = if inner.is_empty() {
                ".".to_string()
            } else {
                inner
            };
            return (dir.to_path_buf(), inner);
        }
    }
    unchanged()
}

/// Shared prelude for every mutating op: the directory exists and is a git
/// work tree. Returns `NotRepo` otherwise (never `Failed`).
async fn ensure_repo(dir: &Path) -> Result<(), GitError> {
    if !dir.is_dir() {
        return Err(GitError::NotRepo(dir.display().to_string()));
    }
    let is_repo = run(dir, &["rev-parse", "--is-inside-work-tree"])
        .await
        .ok_or(GitError::Unavailable)?;
    if !(is_repo.success && is_repo.stdout.trim() == "true") {
        return Err(GitError::NotRepo(dir.display().to_string()));
    }
    Ok(())
}

/// Run `git <args>` in `dir`; `None` (missing binary) -> `Unavailable`,
/// non-zero exit -> `Failed` with git's message.
async fn exec(dir: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
    let out = run(dir, args).await.ok_or(GitError::Unavailable)?;
    if !out.success {
        return Err(GitError::Failed(stderr_summary(&out)));
    }
    Ok(out)
}

/// A caller-supplied path for stage/unstage: a relative path, never an
/// option, absolute path or parent traversal. Whitespace is allowed (file
/// names may contain spaces); the value is always passed after `--`.
pub fn validate_pathspec(path: &str) -> Result<(), GitError> {
    let bad = |m: &str| Err(GitError::InvalidInput(format!("invalid path: {m}")));
    if path.is_empty() || path.len() > 1024 || path.contains('\0') {
        return bad("empty, too long, or NUL");
    }
    if path.starts_with('-') || path.starts_with('/') || path.starts_with('\\') {
        return bad("must be a relative path");
    }
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        return bad("absolute paths are not allowed");
    }
    if path.split(['/', '\\']).any(|seg| seg == "..") {
        return bad("must not contain '..'");
    }
    Ok(())
}

fn validate_paths(paths: &[String]) -> Result<(), GitError> {
    if paths.is_empty() {
        return Err(GitError::InvalidInput("no paths given".to_string()));
    }
    if paths.len() > 500 {
        return Err(GitError::InvalidInput("too many paths".to_string()));
    }
    for p in paths {
        validate_pathspec(p)?;
    }
    Ok(())
}

/// `git add -- <paths>`.
pub async fn stage(dir: &Path, paths: &[String]) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    validate_paths(paths)?;
    let mut args = vec!["add", "--"];
    args.extend(paths.iter().map(String::as_str));
    exec(dir, &args).await.map(|_| ())
}

/// `git reset -q -- <paths>` (unstage). On an unborn HEAD the reset fails,
/// so fall back to `git rm --cached -- <paths>`.
pub async fn unstage(dir: &Path, paths: &[String]) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    validate_paths(paths)?;
    let mut args = vec!["reset", "-q", "--"];
    args.extend(paths.iter().map(String::as_str));
    if run(dir, &args).await.ok_or(GitError::Unavailable)?.success {
        return Ok(());
    }
    let mut rm = vec!["rm", "--cached", "-r", "-q", "--"];
    rm.extend(paths.iter().map(String::as_str));
    exec(dir, &rm).await.map(|_| ())
}

/// `git commit [-q --amend] -m <message>`; returns the new `HEAD` hash.
/// Empty messages are rejected before git runs.
pub async fn commit(dir: &Path, message: &str, amend: bool) -> Result<String, GitError> {
    ensure_repo(dir).await?;
    let message = message.trim();
    if message.is_empty() {
        return Err(GitError::InvalidInput(
            "commit message is empty".to_string(),
        ));
    }
    if message.len() > 100_000 {
        return Err(GitError::InvalidInput(
            "commit message is too long".to_string(),
        ));
    }
    if amend {
        exec(dir, &["commit", "-q", "--amend", "-m", message]).await?;
    } else {
        exec(dir, &["commit", "-q", "-m", message]).await?;
    }
    let out = exec(dir, &["rev-parse", "HEAD"]).await?;
    Ok(out.stdout.trim().to_string())
}

/// Reset mode for [`reset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResetMode {
    Soft,
    Mixed,
    Hard,
}

/// `git reset --<mode> <rev> --`. `Hard` discards work-tree changes; the
/// route layer requires an explicit `confirm: true` for it.
pub async fn reset(dir: &Path, mode: ResetMode, rev: &str) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    validate_ref_public(rev)?;
    let flag = match mode {
        ResetMode::Soft => "--soft",
        ResetMode::Mixed => "--mixed",
        ResetMode::Hard => "--hard",
    };
    exec(dir, &["reset", flag, rev.trim(), "--"])
        .await
        .map(|_| ())
}

/// One stash entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitStash {
    pub index: usize,
    pub name: String,
    pub message: String,
}

/// `git stash list`.
pub async fn stash_list(dir: &Path) -> Result<Vec<GitStash>, GitError> {
    ensure_repo(dir).await?;
    let out = exec(dir, &["stash", "list", "--format=%gd%x1f%s"]).await?;
    let mut stashes = Vec::new();
    for line in out.stdout.lines() {
        let Some((name, message)) = line.split_once('\x1f') else {
            continue;
        };
        let index = name
            .trim()
            .strip_prefix("stash@{")
            .and_then(|n| n.strip_suffix('}'))
            .and_then(|n| n.parse().ok())
            .unwrap_or(stashes.len());
        stashes.push(GitStash {
            index,
            name: name.trim().to_string(),
            message: message.to_string(),
        });
    }
    Ok(stashes)
}

/// `git stash push -u [-m <message>]`.
pub async fn stash_push(dir: &Path, message: Option<&str>) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    if let Some(m) = message {
        let m = m.trim();
        if m.len() > 1024 {
            return Err(GitError::InvalidInput(
                "stash message is too long".to_string(),
            ));
        }
        if !m.is_empty() {
            return exec(dir, &["stash", "push", "-u", "-m", m])
                .await
                .map(|_| ());
        }
    }
    exec(dir, &["stash", "push", "-u"]).await.map(|_| ())
}

fn stash_ref(index: usize) -> String {
    format!("stash@{{{index}}}")
}

/// `git stash pop stash@{n}`.
pub async fn stash_pop(dir: &Path, index: usize) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    exec(dir, &["stash", "pop", &stash_ref(index)])
        .await
        .map(|_| ())
}

/// `git stash apply stash@{n}`.
pub async fn stash_apply(dir: &Path, index: usize) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    exec(dir, &["stash", "apply", &stash_ref(index)])
        .await
        .map(|_| ())
}

/// `git stash drop stash@{n}`. The route layer requires `confirm: true`.
pub async fn stash_drop(dir: &Path, index: usize) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    exec(dir, &["stash", "drop", &stash_ref(index)])
        .await
        .map(|_| ())
}

/// `git checkout [-f] <target> --`. `force` discards local changes; the
/// route layer requires an explicit `confirm: true` for it.
pub async fn checkout(dir: &Path, target: &str, force: bool) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    validate_ref_public(target)?;
    if force {
        exec(dir, &["checkout", "-f", target.trim(), "--"])
            .await
            .map(|_| ())
    } else {
        exec(dir, &["checkout", target.trim(), "--"])
            .await
            .map(|_| ())
    }
}

/// `git branch <name> [base]`. Does not switch to the new branch.
pub async fn branch_create(dir: &Path, name: &str, base: Option<&str>) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    validate_branch(name)?;
    if let Some(base) = base {
        validate_ref_public(base)?;
        exec(dir, &["branch", name.trim(), base.trim()])
            .await
            .map(|_| ())
    } else {
        exec(dir, &["branch", name.trim()]).await.map(|_| ())
    }
}

/// `git branch -d|-D <name>`. `force` (`-D`) needs `confirm: true` at the
/// route layer: it can orphan commits.
pub async fn branch_delete(dir: &Path, name: &str, force: bool) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    validate_branch(name)?;
    let flag = if force { "-D" } else { "-d" };
    exec(dir, &["branch", flag, name.trim()]).await.map(|_| ())
}

/// `git branch -m <from> <to>`.
pub async fn branch_rename(dir: &Path, from: &str, to: &str) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    validate_branch(from)?;
    validate_branch(to)?;
    exec(dir, &["branch", "-m", from.trim(), to.trim()])
        .await
        .map(|_| ())
}

/// `git cherry-pick <rev>`. A conflict surfaces as `Failed` with git's
/// message; resolve with [`cherry_pick_abort`] / [`cherry_pick_continue`].
pub async fn cherry_pick(dir: &Path, rev: &str) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    validate_ref_public(rev)?;
    exec(dir, &["cherry-pick", rev.trim()]).await.map(|_| ())
}

/// `git cherry-pick --abort`.
pub async fn cherry_pick_abort(dir: &Path) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    exec(dir, &["cherry-pick", "--abort"]).await.map(|_| ())
}

/// `git cherry-pick --continue` (`GIT_EDITOR=true` keeps it non-interactive).
pub async fn cherry_pick_continue(dir: &Path) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    exec(dir, &["cherry-pick", "--continue"]).await.map(|_| ())
}

/// One path with unresolved merge conflicts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitConflict {
    pub path: String,
    pub stages: usize,
}

/// Paths with unresolved conflicts (`git ls-files -u`, de-duplicated).
pub async fn conflicts(dir: &Path) -> Result<Vec<GitConflict>, GitError> {
    ensure_repo(dir).await?;
    let out = exec(dir, &["ls-files", "-u"]).await?;
    let mut seen = std::collections::BTreeMap::new();
    for line in out.stdout.lines() {
        if let Some(path) = line.split('\t').nth(1) {
            *seen.entry(path.to_string()).or_insert(0usize) += 1;
        }
    }
    Ok(seen
        .into_iter()
        .map(|(path, stages)| GitConflict { path, stages })
        .collect())
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

/// Make sure `<root>/.bebok/.gitignore` ignores plugin slot directories
/// (`plugins/*/`). Plugin *declarations* (`<root>/.bebok/plugins/*.json`)
/// stay committable — only the per-plugin slot directories
/// (`<root>/.bebok/plugins/<name>/`, e.g. the `bebok-index` checkout) are
/// ignored, hence the trailing-slash directory pattern with a wildcard
/// instead of ignoring `plugins/` outright. Written only when missing (or
/// when it lacks the entry) - the project's own top-level `.gitignore` is
/// never touched, because that is the user's file.
pub fn ensure_plugin_slots_ignored(root: &Path) -> Result<(), GitError> {
    let dot_bebok = root.join(PLUGINS_SLOT_PARENT);
    std::fs::create_dir_all(&dot_bebok).map_err(|e| GitError::Io(e.to_string()))?;
    let ignore = dot_bebok.join(".gitignore");
    let existing = std::fs::read_to_string(&ignore).unwrap_or_default();
    let already = existing.lines().any(|l| {
        let l = l.trim();
        l == PLUGINS_SLOT_ENTRY
            || l == "plugins/*"
            || l == "/plugins/*/"
            || l == "plugins/bebok-index/"
            || l == "*"
    });
    if already {
        return Ok(());
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if text.is_empty() {
        text.push_str("# Written by Bebok: local plugin checkout slots (declarations *.json stay committable).\n");
    }
    text.push_str(PLUGINS_SLOT_ENTRY);
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
    // A failure for a path git no longer knows (already pruned) is fine: the
    // directory is removed from disk below.
    if !out.success && resolved.exists() {
        return Err(GitError::Failed(stderr_summary(&out)));
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
    if trimmed.starts_with('-')
        || trimmed.contains("..")
        || trimmed.chars().any(char::is_whitespace)
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

/// True when `stderr` (or stdout, as a fallback) reads like a credential /
/// authentication failure from a remote operation.
pub fn is_auth_error(stderr: &str) -> bool {
    let text = stderr.trim().to_ascii_lowercase();
    let keys = [
        "authentication failed",
        "could not read username",
        "password",
        "host key verification failed",
        "permission denied (publickey)",
        "fatal: could not read from remote repository",
    ];
    keys.iter().any(|k| text.contains(k))
}

/// Map a failing network output to [`GitError::AuthFailed`] when it looks like
/// an auth problem, otherwise [`GitError::Failed`].
pub fn map_git_auth(stderr: &str) -> GitError {
    if is_auth_error(stderr) {
        let text = stderr.trim();
        GitError::AuthFailed(if text.is_empty() {
            "authentication failed".to_string()
        } else {
            text.to_string()
        })
    } else {
        GitError::Failed(stderr_summary(&GitOutput {
            success: false,
            stdout: String::new(),
            stderr: stderr.to_string(),
        }))
    }
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

// ---------------------------------------------------------------------------
// Phase 3: remotes / sync / config / clone / tools / forge
// ---------------------------------------------------------------------------

/// One remote with its fetch URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteInfo {
    pub name: String,
    pub url: String,
}

/// Scope for `git config set`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigScope {
    Local,
    Global,
}

// -- validators -------------------------------------------------------------

pub fn validate_remote_name(name: &str) -> Result<(), GitError> {
    let bad = |m: &str| Err(GitError::InvalidInput(format!("invalid remote name: {m}")));
    if name.is_empty() || name.len() > 100 || name.contains('\0') {
        return bad("empty, too long, or NUL");
    }
    if name.chars().any(char::is_whitespace) || name.starts_with('-') || name.contains("..") {
        return bad("must be a simple name");
    }
    Ok(())
}

pub fn validate_remote_url(url: &str) -> Result<(), GitError> {
    let bad = |m: &str| Err(GitError::InvalidInput(format!("invalid remote url: {m}")));
    if url.is_empty() || url.len() > 2048 || url.contains('\0') {
        return bad("empty, too long, or NUL");
    }
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("file://") || lower.starts_with("http://") || lower.starts_with("ftp://") {
        return bad("only https:// and ssh:// / git@ URLs are allowed");
    }
    if lower.contains("localhost") || lower.contains("127.0.0.1") || lower.contains("::1") {
        return bad("local URLs are not allowed as remotes");
    }
    if let Some(at) = url.find('@') {
        // Only the authority carries credentials: for `ssh://git@host` the
        // scheme's own `:` sits before the `@` and must not count as a
        // password separator.
        let start = url.find("://").map(|i| i + 3).unwrap_or(0).min(at);
        let authority = &url[start..at];
        if authority.contains(':') {
            return bad("URLs with credentials are not allowed");
        }
    }
    if lower.starts_with("https://") {
        let host = url[8..].split('/').next().unwrap_or("");
        if host.is_empty() || host.starts_with('-') || host.contains('\0') || host.contains(' ') {
            return bad("invalid host");
        }
    } else if lower.starts_with("ssh://") {
        let host = url[6..].split('/').next().unwrap_or("");
        if host.is_empty() || host.starts_with('-') || host.contains('\0') || host.contains(' ') {
            return bad("invalid host");
        }
    } else if url.starts_with("git@") || url.contains(':') {
        let colon = url.find(':').unwrap_or(url.len());
        let host = &url[..colon];
        if host.is_empty() || host.starts_with('-') || host.contains('\0') || host.contains(' ') {
            return bad("invalid host");
        }
    }
    Ok(())
}

pub fn validate_config_key(key: &str) -> Result<(), GitError> {
    let bad = |m: &str| Err(GitError::InvalidInput(format!("invalid config key: {m}")));
    if key.is_empty() || key.len() > 256 || key.contains('\0') || key.contains(char::is_whitespace)
    {
        return bad("empty, too long, NUL, or whitespace");
    }
    let fixed = [
        "user.name",
        "user.email",
        "core.editor",
        "core.autocrlf",
        "core.safecrlf",
        "commit.gpgsign",
        "pull.rebase",
        "push.default",
        "color.ui",
        "init.defaultBranch",
    ];
    if fixed.contains(&key) {
        return Ok(());
    }
    let branch_key_ok = key
        .strip_prefix("branch.")
        .and_then(|rest| rest.rsplit_once('.'))
        .is_some_and(|(name, field)| {
            (field == "remote" || field == "merge") && validate_branch(name).is_ok()
        });
    if branch_key_ok {
        return Ok(());
    }
    bad(&format!("key {key} is not in the allowed set"))
}

pub fn validate_clone_dest(path: &str) -> Result<(), GitError> {
    let bad = |m: &str| {
        Err(GitError::InvalidInput(format!(
            "invalid clone destination: {m}"
        )))
    };
    if path.is_empty() || path.len() > 1024 || path.contains('\0') {
        return bad("empty, too long, or NUL");
    }
    if path.starts_with('-') || path.contains(char::is_whitespace) {
        return bad("must not start with '-' or contain whitespace");
    }
    if path.split('/').any(|seg| seg == "..") || path.split('\\').any(|seg| seg == "..") {
        return bad("must not contain '..'");
    }
    Ok(())
}

pub fn validate_refspec(s: &str) -> Result<(), GitError> {
    let bad = |m: &str| Err(GitError::InvalidInput(format!("invalid refspec: {m}")));
    if s.is_empty() || s.len() > 1024 || s.contains('\0') {
        return bad("empty, too long, or NUL");
    }
    if s.starts_with('-') || s.contains("..") || s.chars().any(char::is_whitespace) {
        return bad("must be a simple refspec");
    }
    Ok(())
}

// -- remotes ---------------------------------------------------------------

pub async fn remotes_list(dir: &Path) -> Result<Vec<RemoteInfo>, GitError> {
    ensure_repo(dir).await?;
    let out = exec(dir, &["remote", "-v"]).await?;
    let mut seen: std::collections::BTreeMap<String, RemoteInfo> =
        std::collections::BTreeMap::new();
    for line in out.stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let name = parts.next().unwrap_or("");
        let url = parts.next().unwrap_or("");
        let kind = parts.next().unwrap_or("");
        if name.is_empty() || url.is_empty() {
            continue;
        }
        let entry = seen.entry(name.to_string()).or_insert_with(|| RemoteInfo {
            name: name.to_string(),
            url: url.to_string(),
        });
        if kind == "(fetch)" {
            entry.url = url.to_string();
        }
    }
    Ok(seen.into_values().collect())
}

pub async fn remotes_set_remote_url(dir: &Path, remote: &str, url: &str) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    validate_remote_name(remote)?;
    validate_remote_url(url)?;
    exec(dir, &["remote", "set-url", remote, url]).await?;
    Ok(())
}

pub async fn remotes_fetch(dir: &Path, remote: &str) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    validate_remote_name(remote)?;
    exec_net(dir, &["fetch", remote]).await?;
    Ok(())
}

pub async fn remotes_pull(dir: &Path, remote: &str, refspec: Option<&str>) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    validate_remote_name(remote)?;
    let mut args = vec!["pull", remote];
    if let Some(rs) = refspec {
        validate_refspec(rs)?;
        args.push(rs);
    }
    exec_net(dir, &args).await?;
    Ok(())
}

pub async fn remotes_push(dir: &Path, remote: &str, refspec: &str) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    validate_remote_name(remote)?;
    validate_refspec(refspec)?;
    exec_net(dir, &["push", remote, refspec]).await?;
    Ok(())
}

// -- config -----------------------------------------------------------------

pub async fn config_get(dir: &Path, key: &str) -> Result<String, GitError> {
    ensure_repo(dir).await?;
    validate_config_key(key)?;
    let out = run(dir, &["config", "--get", key])
        .await
        .ok_or(GitError::Unavailable)?;
    if out.success {
        let line = out.stdout.trim().to_string();
        if line.is_empty() {
            return Err(GitError::InvalidInput("key not set".to_string()));
        }
        return Ok(line);
    }
    if out.stderr.trim().is_empty() {
        return Err(GitError::InvalidInput("key not set".to_string()));
    }
    Err(map_git_auth(&out.stderr))
}

pub async fn config_set(
    dir: &Path,
    key: &str,
    value: &str,
    scope: ConfigScope,
) -> Result<(), GitError> {
    ensure_repo(dir).await?;
    validate_config_key(key)?;
    if value.is_empty() || value.len() > 100_000 || value.contains('\0') {
        return Err(GitError::InvalidInput("invalid config value".to_string()));
    }
    let flag = match scope {
        ConfigScope::Local => "--local",
        ConfigScope::Global => "--global",
    };
    exec(dir, &["config", flag, key, value]).await?;
    Ok(())
}

// -- clone -----------------------------------------------------------------

pub async fn clone(source: &str, dest: &str, branch: Option<&str>) -> Result<String, GitError> {
    validate_remote_url(source)?;
    validate_clone_dest(dest)?;
    let mut args = vec!["clone"];
    if let Some(b) = branch {
        validate_ref(b)?;
        args.push("--branch");
        args.push(b);
    }
    args.push("--");
    args.push(source);
    args.push(dest);
    let out = run_timeout(&std::env::temp_dir(), &args, Duration::from_secs(300))
        .await
        .ok_or(GitError::Unavailable)?;
    if !out.success {
        return Err(map_git_auth(&out.stderr));
    }
    Ok(dest.trim().to_string())
}

// -- tools ------------------------------------------------------------------

pub fn git_available_tools() -> Vec<String> {
    vec!["git".to_string()]
}

// -- forge -----------------------------------------------------------------

/// One allowlist row: `<group> <action> <flag hints> <allowed --json fields>`.
type ForgeAllow = (
    &'static str,
    &'static str,
    &'static [&'static str],
    Option<&'static [&'static str]>,
);

fn validate_forge_args(
    tool: &str,
    args: &[String],
    allowed: &[ForgeAllow],
) -> Result<Vec<String>, GitError> {
    if args.is_empty() {
        return Err(GitError::InvalidInput(format!(
            "{tool} command not allowed: (no arguments)"
        )));
    }
    let group = args[0].as_str();
    let action = args.get(1).map(|s| s.as_str()).unwrap_or("");
    for &(g, a, _flags, json_set) in allowed {
        if group == g && action == a {
            let mut out = Vec::new();
            let mut i = 2usize;
            let mut saw_json = false;
            while i < args.len() {
                let arg = &args[i];
                if arg == "--json" {
                    if saw_json {
                        return Err(GitError::InvalidInput(format!("{tool} duplicate --json")));
                    }
                    saw_json = true;
                    i += 1;
                    let fields = if i < args.len() {
                        let raw = &args[i];
                        let fields: Vec<&str> = raw.split(',').map(|s| s.trim()).collect();
                        i += 1;
                        fields
                    } else {
                        return Err(GitError::InvalidInput(format!(
                            "{tool} --json requires a field list"
                        )));
                    };
                    if let Some(allowed_fields) = json_set {
                        for f in &fields {
                            if !allowed_fields.contains(f) {
                                return Err(GitError::InvalidInput(format!(
                                    "{tool} unknown --json field: {f}"
                                )));
                            }
                        }
                    }
                    out.push("--json".to_string());
                    out.push(fields.join(",").to_string());
                    continue;
                }
                if arg.starts_with("--") {
                    return Err(GitError::InvalidInput(format!(
                        "{tool} unexpected flag: {arg}"
                    )));
                }
                out.push(arg.clone());
                i += 1;
            }
            return Ok(out);
        }
    }
    Err(GitError::InvalidInput(format!(
        "{tool} command not allowed: {group} {action}"
    )))
}

fn allowlist_gh() -> &'static [ForgeAllow] {
    &[
        (
            "repo",
            "view",
            &["--json"],
            Some(&[
                "name",
                "owner",
                "private",
                "url",
                "sshUrl",
                "httpUrl",
                "description",
            ]),
        ),
        ("issue", "list", &["--state", "--limit"], None),
        ("pr", "list", &["--state", "--limit"], None),
        (
            "pr",
            "view",
            &["--json"],
            Some(&["number", "title", "state", "url"]),
        ),
    ]
}

fn allowlist_glab() -> &'static [ForgeAllow] {
    &[
        (
            "repo",
            "view",
            &["--json"],
            Some(&[
                "name",
                "owner",
                "private",
                "url",
                "sshUrl",
                "httpUrl",
                "description",
            ]),
        ),
        ("issue", "list", &["--state", "--limit"], None),
        ("pr", "list", &["--state", "--limit"], None),
        (
            "pr",
            "view",
            &["--json"],
            Some(&["number", "title", "state", "url"]),
        ),
    ]
}

pub async fn forge_gh(dir: &Path, args: &[String]) -> Result<String, GitError> {
    ensure_repo(dir).await?;
    let cleaned = validate_forge_args("gh", args, allowlist_gh())?;
    let out = run_timeout(
        dir,
        &cleaned.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        Duration::from_secs(30),
    )
    .await
    .ok_or(GitError::Unavailable)?;
    if !out.success {
        return Err(map_git_auth(&out.stderr));
    }
    let text = out.stdout.trim().to_string();
    if text.len() > 500_000 {
        return Ok(text[..500_000].to_string());
    }
    Ok(text)
}

pub async fn forge_glab(dir: &Path, args: &[String]) -> Result<String, GitError> {
    ensure_repo(dir).await?;
    let cleaned = validate_forge_args("glab", args, allowlist_glab())?;
    let out = run_timeout(
        dir,
        &cleaned.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        Duration::from_secs(30),
    )
    .await
    .ok_or(GitError::Unavailable)?;
    if !out.success {
        return Err(map_git_auth(&out.stderr));
    }
    let text = out.stdout.trim().to_string();
    if text.len() > 500_000 {
        return Ok(text[..500_000].to_string());
    }
    Ok(text)
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
            let base =
                std::env::temp_dir().join(format!("bebok-git-{tag}-{}", uuid::Uuid::new_v4()));
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
    fn parse_hunk_header_reads_counts_and_defaults() {
        assert_eq!(
            parse_hunk_header("@@ -3,2 +5,4 @@ fn x()"),
            Some(Hunk {
                old_start: 3,
                old_lines: 2,
                new_start: 5,
                new_lines: 4
            })
        );
        assert_eq!(
            parse_hunk_header("@@ -3 +3 @@"),
            Some(Hunk {
                old_start: 3,
                old_lines: 1,
                new_start: 3,
                new_lines: 1
            })
        );
        assert_eq!(
            parse_hunk_header("@@ -0,0 +1,7 @@"),
            Some(Hunk {
                old_start: 0,
                old_lines: 0,
                new_start: 1,
                new_lines: 7
            })
        );
        assert_eq!(parse_hunk_header("not a header"), None);
    }

    #[test]
    fn parse_blame_porcelain_reads_each_line() {
        let text = "\
0123456789012345678901234567890123456789 1 1 1
author Ann
author-mail <ann@example.com>
author-time 1700000000
summary first
filename a.txt
\thello
0123456789012345678901234567890123456789 2 2 1
author Bob
author-mail <bob@example.com>
author-time 1700000100
summary second
filename a.txt
\tworld
";
        let lines = parse_blame_porcelain(text);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].line, 1);
        assert_eq!(lines[0].author, "Ann");
        assert_eq!(lines[0].author_mail, "ann@example.com");
        assert_eq!(lines[0].ts, 1_700_000_000);
        assert_eq!(lines[0].content, "hello");
        assert_eq!(lines[1].line, 2);
        assert_eq!(lines[1].summary, "second");
        assert_eq!(lines[1].content, "world");
    }

    #[tokio::test]
    async fn file_diff_and_blame_round_trip() {
        require_git!();
        let fx = Fixture::new("file-ops");
        let repo = fx.dir("repo");
        init_repo(&repo).await;

        std::fs::write(repo.join("README.md"), "hello\nmore\n").unwrap();
        let hunks = diff_file(&repo, "README.md", None).await.unwrap();
        assert_eq!(
            hunks,
            vec![Hunk {
                old_start: 1,
                old_lines: 0,
                new_start: 2,
                new_lines: 1
            }]
        );

        std::fs::write(repo.join("new.txt"), "a\nb\n").unwrap();
        let untracked = diff_file(&repo, "new.txt", None).await.unwrap();
        assert_eq!(
            untracked,
            vec![Hunk {
                old_start: 0,
                old_lines: 0,
                new_start: 1,
                new_lines: 2
            }]
        );

        let at_head = blame_file(&repo, "README.md", Some("HEAD")).await.unwrap();
        assert_eq!(at_head.len(), 1);
        assert_eq!(at_head[0].author, "Bebok Test");
        assert_eq!(at_head[0].content, "hello");

        let worktree = blame_file(&repo, "README.md", None).await.unwrap();
        assert_eq!(worktree.len(), 2);
        assert_eq!(worktree[1].author, "Not Committed Yet");

        assert!(matches!(
            diff_file(&repo, "../escape", None).await,
            Err(GitError::InvalidInput(_))
        ));
        assert!(matches!(
            blame_file(&repo, "README.md", Some("--output=/tmp/x")).await,
            Err(GitError::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn file_content_round_trip_and_rejects_bad_input() {
        require_git!();
        let fx = Fixture::new("file-content");
        let repo = fx.dir("repo");
        init_repo(&repo).await;

        // Uncommitted edit: HEAD still has the old text, the disk has the new.
        std::fs::write(repo.join("README.md"), "changed\n").unwrap();
        let at_head = file_content(&repo, "README.md", None).await.unwrap();
        assert_eq!(
            at_head,
            FileContent {
                content: "hello\n".to_string(),
                exists: true
            }
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("README.md")).unwrap(),
            "changed\n"
        );
        assert_eq!(
            file_content(&repo, "README.md", Some("HEAD"))
                .await
                .unwrap(),
            at_head
        );

        git_ok(&repo, &["commit", "-q", "-am", "change"]).await;
        let now = file_content(&repo, "README.md", None).await.unwrap();
        assert_eq!(now.content, "changed\n");
        let before = file_content(&repo, "README.md", Some("HEAD~1"))
            .await
            .unwrap();
        assert_eq!(before.content, "hello\n");

        // Untracked file: not in HEAD -> exists false, no error.
        std::fs::write(repo.join("new.txt"), "a\nb\n").unwrap();
        let untracked = file_content(&repo, "new.txt", None).await.unwrap();
        assert_eq!(
            untracked,
            FileContent {
                content: String::new(),
                exists: false
            }
        );

        // Binary blob committed to the repo is refused.
        std::fs::write(repo.join("bin.dat"), b"a\0b").unwrap();
        git_ok(&repo, &["add", "bin.dat"]).await;
        git_ok(&repo, &["commit", "-q", "-m", "bin"]).await;
        assert!(matches!(
            file_content(&repo, "bin.dat", None).await,
            Err(GitError::InvalidInput(_))
        ));

        // Traversal, absolute paths and option-like revs are rejected.
        assert!(matches!(
            file_content(&repo, "../escape", None).await,
            Err(GitError::InvalidInput(_))
        ));
        assert!(matches!(
            file_content(&repo, "/etc/passwd", None).await,
            Err(GitError::InvalidInput(_))
        ));
        assert!(matches!(
            file_content(&repo, "README.md", Some("--output=/tmp/x")).await,
            Err(GitError::InvalidInput(_))
        ));
    }

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
            "", "-x", ".hidden", "a/../b", "a//b", "a/", "/a", "a b", "a\\b", "a.lock", "a/.git",
            "x@{1}", "ü",
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
    fn ensure_plugin_slots_ignored_writes_once_and_appends_when_missing() {
        let fx = Fixture::new("plugin-ignore");
        let root = fx.dir("proj");
        ensure_plugin_slots_ignored(&root).unwrap();
        let file = root.join(".bebok").join(".gitignore");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.lines().any(|l| l == PLUGINS_SLOT_ENTRY),
            "missing {PLUGINS_SLOT_ENTRY} in: {text}"
        );
        // Idempotent.
        ensure_plugin_slots_ignored(&root).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
        // An existing file without the entry gets it appended (no trailing
        // newline in the existing content).
        std::fs::write(&file, "worktrees/").unwrap();
        ensure_plugin_slots_ignored(&root).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text, "worktrees/\nplugins/*/\n");
    }

    #[tokio::test]
    async fn plugin_slot_ignored_but_decl_json_not() {
        require_git!();
        let fx = Fixture::new("plugin-slot");
        let repo = fx.dir("repo");
        init_repo(&repo).await;
        // Slot dir + declaration file, as install_plugin leaves them.
        std::fs::create_dir_all(repo.join(".bebok").join("plugins").join("bebok-index")).unwrap();
        std::fs::write(
            repo.join(".bebok").join("plugins").join("bebok-index.json"),
            "{}\n",
        )
        .unwrap();
        ensure_plugin_slots_ignored(&repo).unwrap();

        // `git check-ignore -q` exits 0 (ignored) vs 1 (not ignored).
        let slot = run(
            &repo,
            &["check-ignore", "-q", ".bebok/plugins/bebok-index/"],
        )
        .await
        .expect("git spawns");
        assert!(slot.success, "slot dir must be ignored");
        // Also ignored when addressed without the trailing slash.
        let slot_bare = run(&repo, &["check-ignore", "-q", ".bebok/plugins/bebok-index"])
            .await
            .expect("git spawns");
        assert!(slot_bare.success, "slot dir (bare path) must be ignored");
        let decl = run(
            &repo,
            &["check-ignore", "-q", ".bebok/plugins/bebok-index.json"],
        )
        .await
        .expect("git spawns");
        assert!(!decl.success, "declaration *.json must stay committable");
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
        assert!(
            validate_worktree_removal(&root, &worktrees_dir(&root).join("..").join("x")).is_err()
        );
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
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/widgets.git",
            ],
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

    #[test]
    fn parse_status_porcelain_covers_header_files_and_rename() {
        let text = "## main...origin/main [ahead 2, behind 1]\nM  staged.txt\n M unstaged.txt\n?? new.txt\nR  new-name.txt <- old-name.txt\n";
        let status = parse_status_porcelain(text);
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!((status.ahead, status.behind), (2, 1));
        assert_eq!(status.files.len(), 4);
        assert!(status.files[0].staged && !status.files[0].untracked);
        assert!(status.files[1].unstaged);
        assert!(status.files[2].untracked);
        assert!(status.files[3].staged);

        let detached = parse_status_porcelain("## HEAD (no branch)\n M a.txt\n");
        assert_eq!(detached.branch, None);
        assert_eq!(detached.files.len(), 1);

        let fresh = parse_status_porcelain("## No commits yet on main\nA  a.txt\n");
        assert_eq!(fresh.branch.as_deref(), Some("main"));
    }

    #[tokio::test]
    async fn status_log_branches_show_round_trip() {
        require_git!();
        let fx = Fixture::new("panel-read");
        let repo = fx.dir("repo");
        init_repo(&repo).await;

        let clean = status(&repo).await.unwrap();
        assert_eq!(clean.branch.as_deref(), Some("main"));
        assert!(clean.files.is_empty());

        std::fs::write(repo.join("README.md"), "changed\n").unwrap();
        std::fs::write(repo.join("new.txt"), "untracked\n").unwrap();
        let dirty = status(&repo).await.unwrap();
        assert_eq!(dirty.files.len(), 2);
        assert!(dirty.files.iter().any(|f| f.untracked));

        git_ok(&repo, &["add", "README.md"]).await;
        let staged = status(&repo).await.unwrap();
        assert!(
            staged
                .files
                .iter()
                .any(|f| f.staged && f.path == "README.md")
        );

        let entries = log(&repo, 50, 0, None).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].subject, "init");
        assert!(entries[0].parents.is_empty());
        let limited = log(&repo, 50, 5, None).await.unwrap();
        assert!(limited.is_empty());
        assert!(log(&repo, 50, 0, Some("../escape")).await.is_err());

        let branches = branches(&repo).await.unwrap();
        assert_eq!(branches.len(), 1);
        assert!(branches[0].current);
        assert_eq!(branches[0].name, "main");

        let shown = show(&repo, "HEAD").await.unwrap();
        assert_eq!(shown.subject, "init");
        assert!(!shown.diff.is_empty());
        assert!(shown.files.iter().any(|f| f.path == "README.md"));
        assert!(show(&repo, "--bad").await.is_err());
        assert!(status(&fx.dir("plain")).await.is_err());
    }

    #[test]
    fn validate_pathspec_accepts_names_and_rejects_escape() {
        for ok in ["a.txt", "dir/b.txt", "sp ace.txt", "żółć.txt"] {
            assert!(validate_pathspec(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "-x",
            "/abs",
            "\\abs",
            "C:/win",
            "../up",
            "a/../../b",
            "a\0b",
        ] {
            assert!(validate_pathspec(bad).is_err(), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn write_ops_round_trip() {
        require_git!();
        let fx = Fixture::new("panel-write");
        let repo = fx.dir("repo");
        init_repo(&repo).await;

        // stage / unstage
        std::fs::write(repo.join("w.txt"), "v1\n").unwrap();
        assert!(stage(&repo, &["w.txt".to_string()]).await.is_ok());
        assert!(stage(&repo, &[]).await.is_err());
        assert!(stage(&repo, &["../escape".to_string()]).await.is_err());
        let st = status(&repo).await.unwrap();
        assert!(st.files.iter().any(|f| f.staged && f.path == "w.txt"));
        unstage(&repo, &["w.txt".to_string()]).await.unwrap();
        let st = status(&repo).await.unwrap();
        assert!(!st.files.iter().any(|f| f.staged && f.path == "w.txt"));

        // commit + amend
        assert!(commit(&repo, "   ", false).await.is_err());
        git_ok(&repo, &["add", "w.txt"]).await;
        let hash = commit(&repo, "add w", false).await.unwrap();
        assert_eq!(hash.len(), 40);
        let entries = log(&repo, 5, 0, None).await.unwrap();
        assert_eq!(entries[0].subject, "add w");
        std::fs::write(repo.join("w.txt"), "v2\n").unwrap();
        git_ok(&repo, &["add", "w.txt"]).await;
        commit(&repo, "amended", true).await.unwrap();
        let entries = log(&repo, 5, 0, None).await.unwrap();
        assert_eq!(entries[0].subject, "amended");

        // reset soft keeps changes staged, hard drops them
        std::fs::write(repo.join("r.txt"), "r\n").unwrap();
        git_ok(&repo, &["add", "r.txt"]).await;
        commit(&repo, "add r", false).await.unwrap();
        reset(&repo, ResetMode::Soft, "HEAD~1").await.unwrap();
        let st = status(&repo).await.unwrap();
        assert!(st.files.iter().any(|f| f.staged && f.path == "r.txt"));
        reset(&repo, ResetMode::Hard, "HEAD").await.unwrap();
        let st = status(&repo).await.unwrap();
        assert!(!st.files.iter().any(|f| f.path == "r.txt"));

        // stash push / list / pop
        std::fs::write(repo.join("s.txt"), "s\n").unwrap();
        stash_push(&repo, Some("wip s")).await.unwrap();
        assert!(!repo.join("s.txt").exists());
        let list = stash_list(&repo).await.unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].message.contains("wip s"));
        stash_pop(&repo, 0).await.unwrap();
        assert!(repo.join("s.txt").exists());
        assert!(stash_list(&repo).await.unwrap().is_empty());
        stash_push(&repo, None).await.unwrap();
        stash_apply(&repo, 0).await.unwrap();
        stash_drop(&repo, 0).await.unwrap();
        assert!(stash_list(&repo).await.unwrap().is_empty());

        // branch create / rename / delete + checkout
        branch_create(&repo, "feat/x", None).await.unwrap();
        assert!(branch_create(&repo, "../bad", None).await.is_err());
        checkout(&repo, "feat/x", false).await.unwrap();
        assert_eq!(
            status(&repo).await.unwrap().branch.as_deref(),
            Some("feat/x")
        );
        branch_rename(&repo, "feat/x", "feat/y").await.unwrap();
        checkout(&repo, "main", false).await.unwrap();
        branch_delete(&repo, "feat/y", false).await.unwrap();
        assert!(
            branches(&repo)
                .await
                .unwrap()
                .iter()
                .all(|b| b.name != "feat/y")
        );

        // cherry-pick conflict -> conflicts() -> abort
        git_ok(&repo, &["checkout", "-q", "-b", "side"]).await;
        std::fs::write(repo.join("README.md"), "side\n").unwrap();
        git_ok(&repo, &["add", "README.md"]).await;
        git_ok(&repo, &["commit", "-q", "-m", "side change"]).await;
        git_ok(&repo, &["checkout", "-q", "main"]).await;
        std::fs::write(repo.join("README.md"), "main\n").unwrap();
        git_ok(&repo, &["add", "README.md"]).await;
        git_ok(&repo, &["commit", "-q", "-m", "main change"]).await;
        let side = git_ok(&repo, &["rev-parse", "side"])
            .await
            .stdout
            .trim()
            .to_string();
        assert!(cherry_pick(&repo, &side).await.is_err());
        let cf = conflicts(&repo).await.unwrap();
        assert!(cf.iter().any(|c| c.path == "README.md"));
        cherry_pick_abort(&repo).await.unwrap();
        assert!(conflicts(&repo).await.unwrap().is_empty());
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
        assert!(
            path.join(".bebok").join("config.json").is_file(),
            "config seeded"
        );
        assert_eq!(
            inspect(&path).await.branch.as_deref(),
            Some("bebok/session-1")
        );
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
        assert!(
            add_worktree(&repo, "bebok/other", Some("--bad"))
                .await
                .is_err()
        );
        assert!(
            add_worktree(&repo, "bebok/other", Some("no-such-ref"))
                .await
                .is_err()
        );

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
        let again = add_worktree(&repo, "bebok/session-1", Some("main"))
            .await
            .unwrap();
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

    // -- Phase 3: validators (pure, no git needed) -------------------------

    #[test]
    fn validate_remote_name_accepts_simple_names_and_rejects_odd_ones() {
        for ok in ["origin", "upstream", "my-remote_1"] {
            assert!(validate_remote_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", "a b", "a..b", "a\0b"] {
            assert!(validate_remote_name(bad).is_err(), "{bad:?}");
        }
        let long = "a".repeat(101);
        assert!(validate_remote_name(&long).is_err());
    }

    #[test]
    fn validate_remote_url_accepts_network_urls_and_rejects_local_or_credentialed() {
        for ok in [
            "https://github.com/acme/widgets.git",
            "ssh://git@github.com/acme/widgets.git",
            "git@github.com:acme/widgets.git",
            "https://gitlab.com/group/sub/project.git",
        ] {
            assert!(validate_remote_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "file:///tmp/x",
            "http://github.com/acme/x.git",
            "ftp://example.com/x.git",
            "https://localhost/acme/x.git",
            "https://127.0.0.1/acme/x.git",
            "https://user:secret@github.com/acme/x.git",
        ] {
            assert!(validate_remote_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn validate_config_key_allows_the_fixed_set_and_branch_keys_only() {
        for ok in [
            "user.name",
            "user.email",
            "core.editor",
            "pull.rebase",
            "branch.main.remote",
            "branch.feature/x.merge",
        ] {
            assert!(validate_config_key(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "user.name ",
            "alias.co",
            "core.sshCommand",
            "branch.main.pushRemote",
            "branch..remote",
            "a\0b",
        ] {
            assert!(validate_config_key(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn validate_clone_dest_rejects_traversal_and_options() {
        for ok in ["repo", "dir/repo", "a\\b"] {
            assert!(validate_clone_dest(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-rf", "a b", "../up", "a/../../b", "a\0b"] {
            assert!(validate_clone_dest(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn validate_refspec_rejects_options_and_whitespace() {
        for ok in ["main", "refs/heads/main", "main:main", "feature/x"] {
            assert!(validate_refspec(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-f", "a b", "a..b", "a\0b"] {
            assert!(validate_refspec(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn is_auth_error_matches_credential_messages_only() {
        for hit in [
            "fatal: Authentication failed for 'https://example.com/x'",
            "fatal: could not read Username for 'https://example.com'",
            "remote: Invalid password",
            "Host key verification failed.",
            "git@example.com: Permission denied (publickey).",
            "fatal: could not read from remote repository",
        ] {
            assert!(is_auth_error(hit), "{hit}");
        }
        for miss in [
            "",
            "fatal: repository 'https://example.com/x' not found",
            "error: failed to push some refs",
        ] {
            assert!(!is_auth_error(miss), "{miss}");
        }
    }

    // -- Phase 3: forge argument validation --------------------------------

    /// gh/glab are not installed in CI, so the *validation* layer is tested
    /// directly: a disallowed subcommand is rejected before any spawn.
    #[tokio::test]
    async fn forge_rejects_commands_outside_the_allowlist() {
        require_git!();
        let fx = Fixture::new("forge-validate");
        let repo = fx.dir("repo");
        init_repo(&repo).await;

        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for bad in [
            args(&["repo", "delete"]),
            args(&["auth", "token"]),
            args(&["pr", "create"]),
            args(&[]),
            args(&["repo", "view", "--web"]),
            args(&["repo", "view", "--json", "secrets"]),
            args(&["repo", "view", "--json"]),
        ] {
            assert!(
                matches!(forge_gh(&repo, &bad).await, Err(GitError::InvalidInput(_))),
                "{bad:?}"
            );
            assert!(
                matches!(
                    forge_glab(&repo, &bad).await,
                    Err(GitError::InvalidInput(_))
                ),
                "{bad:?}"
            );
        }
        // A non-repository is still refused as a repository error.
        assert!(matches!(
            forge_gh(&fx.dir("plain"), &args(&["repo", "view"])).await,
            Err(GitError::NotRepo(_))
        ));
    }

    // -- Phase 3: against a real temp repository ---------------------------

    #[tokio::test]
    async fn remotes_round_trip() {
        require_git!();
        let fx = Fixture::new("remotes");
        let repo = fx.dir("repo");
        init_repo(&repo).await;

        assert!(remotes_list(&repo).await.unwrap().is_empty());
        git_ok(
            &repo,
            &["remote", "add", "origin", "https://github.com/acme/x.git"],
        )
        .await;
        let listed = remotes_list(&repo).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "origin");
        assert_eq!(listed[0].url, "https://github.com/acme/x.git");

        // set-url rewrites the fetch URL; validation rejects junk.
        assert!(
            remotes_set_remote_url(&repo, "origin", "file:///tmp/x")
                .await
                .is_err()
        );
        assert!(
            remotes_set_remote_url(&repo, "-x", "https://github.com/acme/y.git")
                .await
                .is_err()
        );
        remotes_set_remote_url(&repo, "origin", "https://github.com/acme/y.git")
            .await
            .unwrap();
        let listed = remotes_list(&repo).await.unwrap();
        assert_eq!(listed[0].url, "https://github.com/acme/y.git");
    }

    #[tokio::test]
    async fn config_round_trip_and_unset_key_is_invalid_input() {
        require_git!();
        let fx = Fixture::new("config");
        let repo = fx.dir("repo");
        init_repo(&repo).await;

        config_set(&repo, "user.name", "Bebok Config", ConfigScope::Local)
            .await
            .unwrap();
        assert_eq!(
            config_get(&repo, "user.name").await.unwrap(),
            "Bebok Config"
        );
        // A key never written reads as InvalidInput("key not set").
        match config_get(&repo, "core.editor").await {
            Err(GitError::InvalidInput(m)) => assert_eq!(m, "key not set"),
            other => panic!("expected key not set, got {other:?}"),
        }
        // A key outside the allowed set never reaches git.
        assert!(matches!(
            config_set(&repo, "alias.co", "checkout", ConfigScope::Local).await,
            Err(GitError::InvalidInput(_))
        ));
        assert!(matches!(
            config_get(&repo, "alias.co").await,
            Err(GitError::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn fetch_against_a_bogus_remote_fails_without_hanging() {
        require_git!();
        let fx = Fixture::new("fetch");
        let repo = fx.dir("repo");
        init_repo(&repo).await;
        git_ok(
            &repo,
            &["remote", "add", "origin", "https://git.invalid/acme/x.git"],
        )
        .await;
        // Unknown host: never a success answer (no network clone/fetch test).
        assert!(remotes_fetch(&repo, "origin").await.is_err());
        assert!(remotes_fetch(&repo, "-bad").await.is_err());
    }

    #[tokio::test]
    async fn clone_rejects_bad_urls_before_spawning() {
        let fx = Fixture::new("clone-validate");
        let dest = fx.base.join("target");
        let dest_str = dest.to_string_lossy().to_string();
        assert!(matches!(
            clone("file:///tmp/repo", &dest_str, None).await,
            Err(GitError::InvalidInput(_))
        ));
        assert!(matches!(
            clone("https://localhost/x.git", &dest_str, None).await,
            Err(GitError::InvalidInput(_))
        ));
        assert!(matches!(
            clone("https://github.com/acme/x.git", "../escape", None).await,
            Err(GitError::InvalidInput(_))
        ));
    }

    fn scratch_root(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("bebok-gitdisc-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn file_repo_dir_finds_nested_repo() {
        let root = scratch_root("file");
        let repo = root.join("htdocs");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("app/code")).unwrap();
        std::fs::write(repo.join("app/code/Data.php"), "<?php").unwrap();

        let (dir, rel) = file_repo_dir(&root, "htdocs/app/code/Data.php");
        assert_eq!(dir, repo);
        assert_eq!(rel, "app/code/Data.php");
        // A not-yet-existing file still resolves through its parent dirs.
        let (dir, rel) = file_repo_dir(&root, "htdocs/app/code/New.php");
        assert_eq!(dir, repo);
        assert_eq!(rel, "app/code/New.php");
        // The repo dir itself -> whole repo.
        assert_eq!(
            file_repo_dir(&root, "htdocs"),
            (repo.clone(), ".".to_string())
        );
        // Outside the nested repo, or invalid input -> unchanged.
        assert_eq!(
            file_repo_dir(&root, "README.md"),
            (root.clone(), "README.md".to_string())
        );
        assert_eq!(
            file_repo_dir(&root, "../x"),
            (root.clone(), "../x".to_string())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The route resolves `<project>/<rel>` through `file_repo_dir` before
    /// calling the per-file ops; a repo nested one level below the project
    /// root (`root/sub/.git`) must answer diff / content / blame / log.
    #[tokio::test]
    async fn nested_repo_file_ops_resolve_through_file_repo_dir() {
        require_git!();
        let root = scratch_root("nested-ops");
        let repo = root.join("sub");
        std::fs::create_dir_all(repo.join("dir")).unwrap();
        init_repo(&repo).await;
        std::fs::write(repo.join("dir/file.txt"), "a\nb\n").unwrap();
        git_ok(&repo, &["add", "."]).await;
        git_ok(&repo, &["commit", "-q", "-m", "add file"]).await;
        std::fs::write(repo.join("dir/file.txt"), "a\nB\n").unwrap();

        let (dir, rel) = file_repo_dir(&root, "sub/dir/file.txt");
        assert_eq!(dir, repo);
        assert_eq!(rel, "dir/file.txt");

        let hunks = diff_file(&dir, &rel, None).await.expect("diff resolves");
        assert_eq!(
            hunks,
            vec![Hunk {
                old_start: 2,
                old_lines: 1,
                new_start: 2,
                new_lines: 1,
            }]
        );
        let content = file_content(&dir, &rel, None)
            .await
            .expect("content resolves");
        assert!(content.exists);
        assert_eq!(content.content, "a\nb\n");
        let blame = blame_file(&dir, &rel, None).await.expect("blame resolves");
        assert_eq!(blame.len(), 2);
        let history = log(&dir, 10, 0, Some(&rel)).await.expect("log resolves");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].subject, "add file");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A project root with no repo in it or in its direct children is still
    /// `NotRepo` for every per-file op.
    #[tokio::test]
    async fn plain_root_without_repo_is_not_repo() {
        require_git!();
        let root = scratch_root("plain");
        std::fs::create_dir_all(root.join("sub/dir")).unwrap();
        std::fs::write(root.join("sub/dir/file.txt"), "x\n").unwrap();

        let (dir, rel) = file_repo_dir(&root, "sub/dir/file.txt");
        assert_eq!(dir, root);
        assert_eq!(rel, "sub/dir/file.txt");
        assert!(matches!(
            diff_file(&dir, &rel, None).await,
            Err(GitError::NotRepo(_))
        ));
        assert!(matches!(
            file_content(&dir, &rel, None).await,
            Err(GitError::NotRepo(_))
        ));
        assert!(matches!(
            blame_file(&dir, &rel, None).await,
            Err(GitError::NotRepo(_))
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn project_repo_dir_prefers_root_then_nested() {
        let root = scratch_root("proj");
        // No repo anywhere (temp dir is not inside a repo) -> root.
        assert_eq!(project_repo_dir(&root), root);
        // Repos in skipped / hidden dirs are ignored.
        std::fs::create_dir_all(root.join("vendor/lib/.git")).unwrap();
        std::fs::create_dir_all(root.join(".cache/.git")).unwrap();
        assert_eq!(project_repo_dir(&root), root);
        // Nested repo two levels down is found.
        std::fs::create_dir_all(root.join("web/htdocs/.git")).unwrap();
        assert_eq!(project_repo_dir(&root), root.join("web/htdocs"));
        // A shallower one wins.
        std::fs::create_dir_all(root.join("site/.git")).unwrap();
        assert_eq!(project_repo_dir(&root), root.join("site"));
        // Root repo wins over everything.
        std::fs::create_dir_all(root.join(".git")).unwrap();
        assert_eq!(project_repo_dir(&root), root);
        let _ = std::fs::remove_dir_all(&root);
    }
}
