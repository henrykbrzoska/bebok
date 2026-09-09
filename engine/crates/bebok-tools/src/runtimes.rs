//! Language runtime executable paths (configurable in the settings GUI).
//!
//! Tools and the MCP bridge resolve interpreter/compiler executables through
//! these paths instead of assuming hardcoded names. Defaults are the bare
//! command names (resolved via `PATH`); users override them in the `runtimes`
//! config section. At engine startup, if nothing is configured, the engine
//! auto-detects the absolute paths and persists them to the global config.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

/// Known runtime keys (python, python3, node, php, docker, git).
pub const RUNTIME_KEYS: [&str; 6] = ["python", "python3", "node", "php", "docker", "git"];

/// Resolved executable paths for the supported runtimes.
#[derive(Debug, Clone, Serialize)]
pub struct Runtimes {
    pub python: String,
    pub python3: String,
    pub node: String,
    pub php: String,
    pub docker: String,
    pub git: String,
}

impl Default for Runtimes {
    fn default() -> Self {
        Self {
            python: "python".to_string(),
            python3: "python3".to_string(),
            node: "node".to_string(),
            php: "php".to_string(),
            docker: "docker".to_string(),
            git: "git".to_string(),
        }
    }
}

impl Runtimes {
    /// Merge a `runtimes` config section (`{ "python": "/usr/bin/python3", .. }`)
    /// over the defaults.
    pub fn from_config(value: &Value) -> Self {
        let mut out = Self::default();
        let Some(obj) = value.as_object() else {
            return out;
        };
        for (key, field) in [
            ("python", &mut out.python),
            ("python3", &mut out.python3),
            ("node", &mut out.node),
            ("php", &mut out.php),
            ("docker", &mut out.docker),
            ("git", &mut out.git),
        ] {
            if let Some(s) = obj.get(key).and_then(Value::as_str) {
                let s = s.trim();
                if !s.is_empty() {
                    *field = s.to_string();
                }
            }
        }
        out
    }

    /// Auto-detect the absolute path of each runtime by searching `PATH`.
    /// Falls back to the bare command name when not found.
    pub fn detect() -> Self {
        let d = Self::default();
        Self {
            python: detect_path(&d.python).unwrap_or_else(|| PathBuf::from(&d.python)).to_string_lossy().to_string(),
            python3: detect_path(&d.python3).unwrap_or_else(|| PathBuf::from(&d.python3)).to_string_lossy().to_string(),
            node: detect_path(&d.node).unwrap_or_else(|| PathBuf::from(&d.node)).to_string_lossy().to_string(),
            php: detect_path(&d.php).unwrap_or_else(|| PathBuf::from(&d.php)).to_string_lossy().to_string(),
            docker: detect_path(&d.docker).unwrap_or_else(|| PathBuf::from(&d.docker)).to_string_lossy().to_string(),
            git: detect_path(&d.git).unwrap_or_else(|| PathBuf::from(&d.git)).to_string_lossy().to_string(),
        }
    }

    /// Resolve a runtime key (also accepts aliases) to its executable path.
    pub fn resolve(&self, name: &str) -> Option<&str> {
        match name {
            "python" => Some(&self.python),
            "python3" => Some(&self.python3),
            "node" | "nodejs" | "node.js" => Some(&self.node),
            "php" => Some(&self.php),
            "docker" => Some(&self.docker),
            "git" => Some(&self.git),
            _ => None,
        }
    }
}

/// Find the absolute path of `name` on `PATH` (returns `None` if not found).
fn detect_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        for ext in ["exe", "cmd", "bat"] {
            let candidate = dir.join(format!("{name}.{ext}"));
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_bare_names() {
        let r = Runtimes::default();
        assert_eq!(r.resolve("python3"), Some("python3"));
        assert_eq!(r.resolve("node"), Some("node"));
        assert_eq!(r.resolve("nodejs"), Some("node"));
        assert_eq!(r.resolve("php"), Some("php"));
        assert_eq!(r.resolve("docker"), Some("docker"));
        assert_eq!(r.resolve("ruby"), None);
    }

    #[test]
    fn from_config_overrides_and_ignores_blanks() {
        let v: Value = serde_json::from_str(
            r#"{ "python": "/usr/bin/python3", "python3": "  ", "node": "/opt/node/bin/node", "docker": "/usr/bin/docker" }"#,
        )
        .unwrap();
        let r = Runtimes::from_config(&v);
        assert_eq!(r.python, "/usr/bin/python3");
        assert_eq!(r.python3, "python3", "blank value falls back to default");
        assert_eq!(r.node, "/opt/node/bin/node");
        assert_eq!(r.php, "php");
        assert_eq!(r.docker, "/usr/bin/docker");
    }
}
