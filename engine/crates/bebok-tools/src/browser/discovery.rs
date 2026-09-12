//! Chromium / Chrome / Edge binary discovery for the `browser_*` tools.
//!
//! `chromiumoxide` can auto-detect a browser on its own, but its probe order
//! (env var, `PATH`, registry, fixed paths) is opaque to the user and its
//! error message is generic. Bebok runs its own, documented probe first so the
//! "no browser found" text can list exactly what was tried; chromiumoxide's
//! detection is only the final fallback.
//!
//! Probe order:
//! 1. `BEBOK_BROWSER` — explicit path (any platform).
//! 2. `CHROME` — chromiumoxide's own env var (kept for compatibility).
//! 3. Platform-specific:
//!    * Windows — `%ProgramFiles%` and `%ProgramFiles(x86)%` (plus
//!      `%LocalAppData%` for per-user Chrome installs):
//!      `Google\Chrome\Application\chrome.exe`,
//!      `Microsoft\Edge\Application\msedge.exe`, `Chromium\Application\chrome.exe`.
//!    * Linux / other unix — `chromium`, `chromium-browser`, `google-chrome`,
//!      `google-chrome-stable`, `microsoft-edge`, `microsoft-edge-stable` on
//!      `PATH`.
//!    * macOS — the usual `/Applications/*.app` bundles, then `PATH`.

use std::path::{Path, PathBuf};

/// Environment variables consulted before any platform probe (in order).
pub const ENV_VARS: &[&str] = &["BEBOK_BROWSER", "CHROME"];

/// Locate a Chromium-family executable. `None` when nothing was found.
pub fn discover_binary() -> Option<PathBuf> {
    discover_with(|k| std::env::var_os(k).map(PathBuf::from), Path::exists, which_on_path)
}

/// Human-readable list of the locations probed, for the "not found" error.
pub fn probe_description() -> String {
    let env = ENV_VARS.join(", ");
    let platform = if cfg!(windows) {
        "Program Files / Program Files (x86) / LocalAppData: Google Chrome, Microsoft Edge, Chromium"
    } else if cfg!(target_os = "macos") {
        "/Applications: Google Chrome, Microsoft Edge, Chromium; then PATH"
    } else {
        "PATH: chromium, chromium-browser, google-chrome, google-chrome-stable, microsoft-edge"
    };
    format!("env {env}; {platform}")
}

/// Testable core of [`discover_binary`]: the environment, the filesystem
/// existence check and the `PATH` lookup are injected.
pub(crate) fn discover_with<E, X, W>(env: E, exists: X, which: W) -> Option<PathBuf>
where
    E: Fn(&str) -> Option<PathBuf>,
    X: Fn(&Path) -> bool,
    W: Fn(&str) -> Option<PathBuf>,
{
    for key in ENV_VARS {
        if let Some(p) = env(key)
            && !p.as_os_str().is_empty()
            && exists(&p)
        {
            return Some(p);
        }
    }
    for candidate in platform_candidates(&env) {
        if exists(&candidate) {
            return Some(candidate);
        }
    }
    for name in path_names() {
        if let Some(p) = which(name) {
            return Some(p);
        }
    }
    None
}

/// Fixed install locations to probe on this platform (before `PATH`).
#[cfg(windows)]
pub(crate) fn platform_candidates<E>(env: &E) -> Vec<PathBuf>
where
    E: Fn(&str) -> Option<PathBuf>,
{
    const REL: &[&[&str]] = &[
        &["Google", "Chrome", "Application", "chrome.exe"],
        &["Microsoft", "Edge", "Application", "msedge.exe"],
        &["Chromium", "Application", "chrome.exe"],
    ];
    let mut roots: Vec<PathBuf> = Vec::new();
    for key in ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"] {
        if let Some(root) = env(key)
            && !root.as_os_str().is_empty()
            && !roots.contains(&root)
        {
            roots.push(root);
        }
    }
    let mut out = Vec::new();
    for root in roots {
        for rel in REL {
            let mut p = root.clone();
            for seg in *rel {
                p.push(seg);
            }
            out.push(p);
        }
    }
    out
}

#[cfg(target_os = "macos")]
pub(crate) fn platform_candidates<E>(_env: &E) -> Vec<PathBuf>
where
    E: Fn(&str) -> Option<PathBuf>,
{
    [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) fn platform_candidates<E>(_env: &E) -> Vec<PathBuf>
where
    E: Fn(&str) -> Option<PathBuf>,
{
    // Linux relies on PATH; no fixed locations beyond what `which` finds.
    Vec::new()
}

/// Executable names looked up on `PATH` (all platforms; on Windows `.exe` is
/// appended by the lookup).
pub(crate) fn path_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["chrome", "msedge", "chromium"]
    } else {
        &[
            "chromium",
            "chromium-browser",
            "google-chrome",
            "google-chrome-stable",
            "microsoft-edge",
            "microsoft-edge-stable",
        ]
    }
}

/// Minimal `which`: scan `PATH` for `name` (with `PATHEXT`-style `.exe` on
/// Windows). No external dependency needed.
fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{name}.exe"), name.to_string()]
    } else {
        vec![name.to_string()]
    };
    for dir in std::env::split_paths(&path) {
        for n in &names {
            let candidate = dir.join(n);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_from<'a>(map: &'a HashMap<&'a str, &'a str>) -> impl Fn(&str) -> Option<PathBuf> + 'a {
        move |k| map.get(k).map(PathBuf::from)
    }

    #[test]
    fn bebok_browser_env_wins_when_it_exists() {
        let mut env = HashMap::new();
        env.insert("BEBOK_BROWSER", "/opt/my-chrome");
        env.insert("CHROME", "/opt/other");
        let found = discover_with(
            env_from(&env),
            |p| p == Path::new("/opt/my-chrome"),
            |_| None,
        );
        assert_eq!(found, Some(PathBuf::from("/opt/my-chrome")));
    }

    #[test]
    fn env_var_pointing_nowhere_is_skipped() {
        let mut env = HashMap::new();
        env.insert("BEBOK_BROWSER", "/nope");
        let found = discover_with(env_from(&env), |_| false, |_| None);
        assert_eq!(found, None);
    }

    #[test]
    fn falls_back_to_path_lookup() {
        let env: HashMap<&str, &str> = HashMap::new();
        let found = discover_with(
            env_from(&env),
            |_| false,
            |name| (name == path_names()[0]).then(|| PathBuf::from("/usr/bin").join(name)),
        );
        assert_eq!(found, Some(PathBuf::from("/usr/bin").join(path_names()[0])));
    }

    #[cfg(windows)]
    #[test]
    fn windows_probes_program_files_for_chrome_and_edge() {
        let mut env = HashMap::new();
        env.insert("ProgramFiles", "C:\\PF");
        env.insert("ProgramFiles(x86)", "C:\\PF86");
        let candidates = platform_candidates(&env_from(&env));
        let chrome: PathBuf = ["C:\\PF", "Google", "Chrome", "Application", "chrome.exe"]
            .iter()
            .collect();
        let edge: PathBuf = ["C:\\PF86", "Microsoft", "Edge", "Application", "msedge.exe"]
            .iter()
            .collect();
        assert!(candidates.contains(&chrome), "{candidates:?}");
        assert!(candidates.contains(&edge), "{candidates:?}");

        // The first existing candidate is returned.
        let found = discover_with(env_from(&env), |p| p == edge, |_| None);
        assert_eq!(found, Some(edge));
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn linux_has_no_fixed_candidates_and_probes_path_names() {
        let env: HashMap<&str, &str> = HashMap::new();
        assert!(platform_candidates(&env_from(&env)).is_empty());
        let names = path_names();
        assert!(names.contains(&"chromium"));
        assert!(names.contains(&"chromium-browser"));
        assert!(names.contains(&"google-chrome"));
        assert!(names.contains(&"google-chrome-stable"));
    }

    #[test]
    fn probe_description_mentions_env_vars() {
        let d = probe_description();
        assert!(d.contains("BEBOK_BROWSER"));
        assert!(d.contains("CHROME"));
    }
}
