//! Binary discovery for CLI backends.
//!
//! Mirrors `bebok-tools/src/browser/discovery.rs` (a minimal `which` plus a
//! `BEBOK_<X>_BIN` override so a user can point at an exotic install), with one
//! difference that matters here: on Windows an agentic CLI is usually a Node
//! package, and npm installs those as `.cmd` shims — `claude.cmd`, never
//! `claude.exe`. Looking only for `.exe` (as the browser probe does) would
//! therefore miss the normal install of every CLI backend, so the lookup walks
//! `PATHEXT` and the fixed `.exe`/`.cmd`/`.bat` list.
//!
//! Precedence for [`discover_binary`], highest first:
//! 1. the backend's `env_override()` variable, when it points at a real file
//!    (an explicit user choice always wins over whatever `PATH` happens to
//!    contain — the point of the variable is to escape a wrong install);
//! 2. the backend's `binary_names()` in order, via [`which_on_path`];
//! 3. `None`.

use std::path::{Path, PathBuf};

use crate::backend::CliBackend;

/// Extensions tried on Windows when `PATHEXT` is unset. `.cmd`/`.bat` are in the
/// list because npm/pnpm/yarn install `claude.cmd` and `claude.ps1`.
const FALLBACK_EXTS: &[&str] = &[".exe", ".cmd", ".bat"];

/// Resolve one executable name against `PATH`.
///
/// Deliberately tiny: no `which` dependency, no shelling out, and no
/// permission/`access(2)` probing beyond `is_file()` — a non-executable regular
/// file on `PATH` is still reported so the caller's spawn error names the path
/// the user actually has, instead of a confusing "not found".
pub fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let pathext = std::env::var("PATHEXT").ok();
    let names = candidate_names(name, cfg!(windows), pathext.as_deref());
    for dir in std::env::split_paths(&path) {
        // An empty `PATH` segment means "current directory" for most shells;
        // skipping it is intentional — resolving a CLI out of the project tree
        // would let a repository ship the binary that runs on the user's behalf.
        if dir.as_os_str().is_empty() {
            continue;
        }
        for n in &names {
            let candidate = dir.join(n);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The file names to try for `name` on `PATH`, in order.
///
/// On unixes this is the bare name. On Windows it is every `PATHEXT` extension
/// (or [`FALLBACK_EXTS`] when the variable is unset/empty) and then the bare
/// name, so a caller that already knows the exact file (e.g. `claude.cmd`) still
/// resolves. A name that already carries an extension is used verbatim —
/// appending `.exe` to `claude.cmd` would only ever miss.
fn candidate_names(name: &str, windows: bool, pathext: Option<&str>) -> Vec<String> {
    if !windows || Path::new(name).extension().is_some() {
        return vec![name.to_string()];
    }
    let from_env: Vec<String> = pathext
        .unwrap_or_default()
        .split(';')
        .map(str::trim)
        .filter(|ext| !ext.is_empty())
        .map(|ext| {
            if ext.starts_with('.') {
                ext.to_string()
            } else {
                format!(".{ext}")
            }
        })
        .collect();
    let exts: Vec<String> = if from_env.is_empty() {
        FALLBACK_EXTS.iter().map(|e| e.to_string()).collect()
    } else {
        from_env
    };
    let mut names: Vec<String> = exts.iter().map(|ext| format!("{name}{ext}")).collect();
    names.push(name.to_string());
    names
}

/// Resolve a backend's binary: env override, then `PATH`.
pub fn discover_binary(backend: &dyn CliBackend) -> Option<PathBuf> {
    discover_with(
        backend,
        |key| std::env::var_os(key).map(PathBuf::from),
        Path::is_file,
        which_on_path,
    )
}

/// Testable core of [`discover_binary`]: the environment, the file-existence
/// check and the `PATH` lookup are injected so discovery can be exercised
/// without touching the real `PATH` of the test process.
fn discover_with<E, X, W>(backend: &dyn CliBackend, env: E, exists: X, which: W) -> Option<PathBuf>
where
    E: Fn(&str) -> Option<PathBuf>,
    X: Fn(&Path) -> bool,
    W: Fn(&str) -> Option<PathBuf>,
{
    if let Some(p) = env(backend.env_override())
        && !p.as_os_str().is_empty()
        && exists(&p)
    {
        return Some(p);
    }
    for name in backend.binary_names() {
        if let Some(p) = which(name) {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{CliCapabilities, CliError, CliEvent, CliRequest, CliStreamState};
    use crate::spec::CliSpec;
    use std::collections::HashMap;

    /// Minimal backend whose only interesting surface is the discovery data.
    struct FakeBackend {
        names: &'static [&'static str],
        env: &'static str,
    }

    impl CliBackend for FakeBackend {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn label(&self) -> &'static str {
            "Fake"
        }
        fn capabilities(&self) -> CliCapabilities {
            CliCapabilities::default()
        }
        fn default_models(&self) -> Vec<String> {
            vec!["fake-model".into()]
        }
        fn binary_names(&self) -> &'static [&'static str] {
            self.names
        }
        fn env_override(&self) -> &'static str {
            self.env
        }
        fn build_command(
            &self,
            _spec: &CliSpec,
            _req: &CliRequest,
            _probe: &crate::backend::CliProbe,
            _scratch: &mut crate::backend::TurnScratch,
        ) -> Result<(String, Vec<String>), CliError> {
            Ok((String::from("fake"), Vec::new()))
        }
        fn parse_line(
            &self,
            _line: &str,
            _st: &mut CliStreamState,
        ) -> Result<Option<CliEvent>, CliError> {
            Ok(None)
        }
    }

    fn env_from<'a>(map: &'a HashMap<&'a str, &'a str>) -> impl Fn(&str) -> Option<PathBuf> + 'a {
        move |k| map.get(k).map(PathBuf::from)
    }

    #[test]
    fn env_override_wins_when_the_file_exists() {
        let backend = FakeBackend {
            names: &["claude", "claude-code"],
            env: "BEBOK_CLAUDE_BIN",
        };
        let mut env = HashMap::new();
        env.insert("BEBOK_CLAUDE_BIN", "/opt/claude/bin/claude");
        let found = discover_with(
            &backend,
            env_from(&env),
            |p| p == Path::new("/opt/claude/bin/claude"),
            |_| Some(PathBuf::from("/usr/bin/claude")),
        );
        assert_eq!(found, Some(PathBuf::from("/opt/claude/bin/claude")));
    }

    #[test]
    fn env_override_pointing_nowhere_is_skipped() {
        let backend = FakeBackend {
            names: &["claude"],
            env: "BEBOK_CLAUDE_BIN",
        };
        let mut env = HashMap::new();
        env.insert("BEBOK_CLAUDE_BIN", "/nope/claude");
        let found = discover_with(&backend, env_from(&env), |_| false, |_| None);
        assert_eq!(found, None);
    }

    #[test]
    fn empty_env_override_is_skipped() {
        let backend = FakeBackend {
            names: &["claude"],
            env: "BEBOK_CLAUDE_BIN",
        };
        let mut env = HashMap::new();
        env.insert("BEBOK_CLAUDE_BIN", "");
        let found = discover_with(&backend, env_from(&env), |_| true, |_| None);
        assert_eq!(found, None);
    }

    #[test]
    fn falls_through_to_the_first_binary_name_that_resolves() {
        let backend = FakeBackend {
            names: &["claude", "claude-code"],
            env: "BEBOK_CLAUDE_BIN",
        };
        let found = discover_with(
            &backend,
            |_| None,
            |_| false,
            |name| (name == "claude-code").then(|| PathBuf::from("/usr/local/bin/claude-code")),
        );
        assert_eq!(found, Some(PathBuf::from("/usr/local/bin/claude-code")));
    }

    #[test]
    fn returns_none_when_nothing_resolves() {
        let backend = FakeBackend {
            names: &["claude", "claude-code"],
            env: "BEBOK_CLAUDE_BIN",
        };
        let found = discover_with(&backend, |_| None, |_| false, |_| None);
        assert_eq!(found, None);
    }

    #[test]
    fn unix_candidates_are_the_bare_name() {
        assert_eq!(candidate_names("claude", false, None), vec!["claude"]);
        // PATHEXT is meaningless off Windows and must not be honoured.
        assert_eq!(
            candidate_names("claude", false, Some(".EXE;.CMD")),
            vec!["claude"]
        );
    }

    #[test]
    fn windows_candidates_prefer_cmd_before_bare_name() {
        let names = candidate_names("claude", true, None);
        assert_eq!(
            names,
            vec!["claude.exe", "claude.cmd", "claude.bat", "claude"]
        );
        // The npm shim is probed before the extension-less form.
        let cmd = names.iter().position(|n| n == "claude.cmd").unwrap();
        let bare = names.iter().position(|n| n == "claude").unwrap();
        assert!(cmd < bare);
    }

    #[test]
    fn windows_candidates_honour_pathext_when_set() {
        let names = candidate_names("claude", true, Some(".CMD;.EXE"));
        assert_eq!(names, vec!["claude.CMD", "claude.EXE", "claude"]);

        // Entries without a leading dot and blank entries are normalised/dropped.
        let names = candidate_names("claude", true, Some("EXE;;  "));
        assert_eq!(names, vec!["claude.EXE", "claude"]);

        // An empty PATHEXT falls back to the fixed list.
        let names = candidate_names("claude", true, Some(""));
        assert_eq!(
            names,
            vec!["claude.exe", "claude.cmd", "claude.bat", "claude"]
        );
    }

    #[test]
    fn windows_name_with_an_extension_is_used_verbatim() {
        assert_eq!(
            candidate_names("claude.cmd", true, None),
            vec!["claude.cmd"]
        );
    }

    #[test]
    fn which_on_path_finds_an_existing_executable_on_the_real_path() {
        // The test binary itself lives on a real directory; look it up under its
        // file name so this stays independent of the host's PATH contents.
        let exe = std::env::current_exe().expect("test binary path");
        let file_name = exe.file_name().and_then(|n| n.to_str()).expect("name");
        let parent = exe.parent().map(Path::to_path_buf);
        // `which_on_path` needs the directory on PATH; when it is not (e.g. the
        // test runner uses an absolute path only) there is nothing to assert.
        let on_path = match (parent, std::env::var_os("PATH")) {
            (Some(parent), Some(path)) => std::env::split_paths(&path).any(|d| d == parent),
            _ => false,
        };
        if on_path {
            assert_eq!(which_on_path(file_name), Some(exe));
        }
        assert_eq!(which_on_path("bebok-definitely-not-installed-xyz"), None);
    }
}
