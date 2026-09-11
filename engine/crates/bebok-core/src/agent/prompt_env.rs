//! Host-environment notes injected into an agent's system prompt.
//!
//! The agent presets are platform-agnostic but the `bash` tool is not: it runs
//! `cmd /C` on Windows and a POSIX `sh -c` on macOS/Linux. Telling the model
//! *which* host it is on is what makes it emit the right syntax.
//!
//! This lives in one place on purpose. Prompt assembly happens on three paths
//! (`services/turn.rs` for user sessions, `agent/task_tool.rs` and
//! `agent/fleet_tool.rs` for delegated sub-agents) and they used to drift:
//! sub-agents were never told the host OS, so on Windows they wrote POSIX
//! pipelines that `cmd /C` could not run and the delegated `task` failed
//! while the same work succeeded on Linux.

/// Human-readable host OS label (`Windows`, `macOS`, `Linux`, …).
pub fn host_os_label() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    }
}

/// System-prompt note naming the host OS and the shell dialect the `bash` tool
/// will therefore use. Append this to every assembled prompt — including
/// delegated sub-agents.
pub fn host_os_note() -> String {
    format!(
        "Host OS: {}. The `bash` tool runs that OS's native shell: `cmd /C` on \
         Windows, POSIX `sh -c` on macOS/Linux. On Windows use `cmd` syntax \
         (or, preferably, the native file/search tools) — never POSIX-only \
         pipelines like `head`, `tail`, `grep`, `wc` or `|` chains.",
        host_os_label()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_is_not_empty() {
        assert!(!host_os_label().is_empty());
    }

    #[test]
    fn note_names_the_host_os() {
        let note = host_os_note();
        assert!(note.starts_with("Host OS: "), "{note}");
        assert!(note.contains(host_os_label()), "{note}");
    }

    #[test]
    fn label_matches_the_build_target() {
        #[cfg(windows)]
        assert_eq!(host_os_label(), "Windows");
        #[cfg(target_os = "macos")]
        assert_eq!(host_os_label(), "macOS");
        #[cfg(target_os = "linux")]
        assert_eq!(host_os_label(), "Linux");
    }
}
