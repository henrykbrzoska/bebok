//! Default shell selection and environment scrubbing for spawned PTYs.
//!
//! The child shell must not inherit server auth material (`BEBOK_PASSWORD`,
//! provider API keys, ...). We scrub secrets from the inherited environment and
//! keep everything else so `PATH`/`HOME`/`TERM` behave normally.

/// The default interactive shell for the current OS.
///
/// Windows uses PowerShell by default (ConPTY is the only supported backend
/// there), but honours `BEBOK_SHELL`. On Unix we prefer `BEBOK_SHELL`
/// (explicit override used by the Android/iOS embedding to point at a bundled
/// shell), then `$SHELL`, then `/bin/bash`, then `/bin/sh`.
pub fn default_shell() -> String {
    #[cfg(windows)]
    {
        if let Ok(shell) = std::env::var("BEBOK_SHELL")
            && !shell.is_empty()
        {
            return shell;
        }
        return "powershell.exe".to_string();
    }

    #[cfg(not(windows))]
    {
        if let Ok(shell) = std::env::var("BEBOK_SHELL").or_else(|_| std::env::var("SHELL")) {
            if !shell.is_empty() {
                return shell;
            }
        }
        if std::path::Path::new("/bin/bash").exists() {
            return "/bin/bash".to_string();
        }
        "/bin/sh".to_string()
    }
}

/// Inherited environment minus secret variables. Returns `(key, value)` pairs
/// ready for [`portable_pty::CommandBuilder::env`].
pub fn scrubbed_env() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(key, _)| !is_secret(key))
        .collect()
}

/// Heuristic: a variable name that looks like a credential is scrubbed.
fn is_secret(key: &str) -> bool {
    let up = key.to_ascii_uppercase();
    up.contains("PASSWORD")
        || up.contains("API_KEY")
        || up.contains("TOKEN")
        || up.contains("SECRET")
        || up.contains("CREDENTIAL")
        || up == "BEBOK_PASSWORD"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_scrubbed() {
        assert!(is_secret("ZAI_API_KEY"));
        assert!(is_secret("OPENAI_API_KEY"));
        assert!(is_secret("BEBOK_PASSWORD"));
        assert!(is_secret("GITHUB_TOKEN"));
        assert!(is_secret("DB_SECRET"));
        assert!(!is_secret("PATH"));
        assert!(!is_secret("HOME"));
        assert!(!is_secret("TERM"));
        assert!(!is_secret("LANG"));
    }

    #[test]
    fn scrubbed_env_has_no_secrets() {
        let env = scrubbed_env();
        for (key, _) in &env {
            assert!(!is_secret(key), "secret {key} leaked into the PTY env");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_shell_override_selects_pwsh() {
        use std::sync::{Mutex, OnceLock};

        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _lock = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let prior = std::env::var_os("BEBOK_SHELL");
        unsafe { std::env::set_var("BEBOK_SHELL", "pwsh") };
        assert_eq!(default_shell(), "pwsh");
        match prior {
            Some(value) => unsafe { std::env::set_var("BEBOK_SHELL", value) },
            None => unsafe { std::env::remove_var("BEBOK_SHELL") },
        }
    }
}
