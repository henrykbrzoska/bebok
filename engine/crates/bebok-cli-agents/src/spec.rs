//! `CliSpec` — the CLI-specific knobs, read out of a provider's `extra` map.
//!
//! No new config sections: a CLI provider is an ordinary entry in
//! `providers[]` with `kind: "cli"`, and everything CLI-specific lives in
//! `extra`, which the JSONC writers already round-trip verbatim.

use serde_json::Value;

use crate::backend::{CliError, CliMode};

/// Default per-turn timeout when `extra.timeout_secs` is absent.
pub const DEFAULT_TIMEOUT_SECS: u64 = 900;
/// Hard floor: below this a CLI cannot even boot.
pub const MIN_TIMEOUT_SECS: u64 = 30;
/// Hard ceiling: a hung CLI must not pin a turn forever.
pub const MAX_TIMEOUT_SECS: u64 = 7200;

/// `extra.profile`: how much of the CLI's own configuration a turn sees.
///
/// * `bebok` (default): Bebok's managed profile on top of the user's settings
///   (`--setting-sources user`, strict MCP).
/// * `user`: the user's configuration as today; only overrides and project
///   files are layered on top.
/// * `isolated`: `CLAUDE_CONFIG_DIR` points at the profile, so none of the
///   user's configuration is read. Needs its own login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProfileMode {
    #[default]
    Bebok,
    User,
    Isolated,
}

/// `extra.system_prompt_mode`: whether Bebok's system prompt is appended to the
/// CLI's default prompt or replaces it. Applies to `bebok` and `isolated`;
/// `user` keeps the historical append.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SystemPromptMode {
    #[default]
    Append,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliSpec {
    /// Backend id (`extra.backend`, e.g. `"claude"`).
    pub backend: String,
    /// Explicit binary path (`extra.command`), bypassing discovery.
    pub command: Option<String>,
    /// Extra user flags appended verbatim (`extra.args`).
    pub args: Vec<String>,
    /// `extra.mode`: `agent` (default) or `text`.
    pub mode: CliMode,
    /// `extra.allow_write`. **False by default**: CLI edits bypass Bebok's
    /// permission engine and change tracking.
    pub allow_write: bool,
    /// `extra.timeout_secs`, clamped to [`MIN_TIMEOUT_SECS`]..=[`MAX_TIMEOUT_SECS`].
    pub timeout_secs: u64,
    /// `extra.max_turns`.
    pub max_turns: Option<u32>,
    /// `extra.profile`.
    pub profile: ProfileMode,
    /// `extra.profile_overrides`: an object deep-merged over the profile's
    /// `settings.json` (objects merge key by key, anything else replaces).
    pub profile_overrides: Option<Value>,
    /// `extra.system_prompt_mode`.
    pub system_prompt_mode: SystemPromptMode,
}

impl CliSpec {
    /// Read a spec out of a provider entry's `extra` map.
    ///
    /// Missing `extra.backend` is an error rather than a default: guessing the
    /// backend from the provider name would silently run the wrong binary.
    pub fn from_extra(extra: &serde_json::Map<String, Value>) -> Result<Self, CliError> {
        let backend = extra
            .get("backend")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                CliError::Config("providers[].extra.backend is required for kind \"cli\"".into())
            })?
            .to_string();

        let mode = match extra.get("mode").and_then(Value::as_str) {
            None => CliMode::Agent,
            Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
                "agent" => CliMode::Agent,
                "text" => CliMode::Text,
                other => {
                    return Err(CliError::Config(format!(
                        "extra.mode must be \"agent\" or \"text\", got \"{other}\""
                    )));
                }
            },
        };

        let command = extra
            .get("command")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let args: Vec<String> = extra
            .get("args")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let timeout_secs = extra
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);

        let max_turns = extra
            .get("max_turns")
            .and_then(Value::as_u64)
            .map(|n| n.min(u32::MAX as u64) as u32)
            .filter(|n| *n > 0);

        let profile = match extra.get("profile").and_then(Value::as_str) {
            None => ProfileMode::Bebok,
            Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
                "bebok" => ProfileMode::Bebok,
                "user" => ProfileMode::User,
                "isolated" => ProfileMode::Isolated,
                other => {
                    return Err(CliError::Config(format!(
                        "extra.profile must be \"bebok\", \"user\" or \"isolated\", got \"{other}\""
                    )));
                }
            },
        };

        let profile_overrides = match extra.get("profile_overrides") {
            None | Some(Value::Null) => None,
            Some(value @ Value::Object(_)) => Some(value.clone()),
            Some(_) => {
                return Err(CliError::Config(
                    "extra.profile_overrides must be a JSON object".into(),
                ));
            }
        };

        let system_prompt_mode = match extra.get("system_prompt_mode").and_then(Value::as_str) {
            None => SystemPromptMode::Append,
            Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
                "append" => SystemPromptMode::Append,
                "replace" => SystemPromptMode::Replace,
                other => {
                    return Err(CliError::Config(format!(
                        "extra.system_prompt_mode must be \"append\" or \"replace\", got \"{other}\""
                    )));
                }
            },
        };

        // SECURITY: a permission bypass smuggled in through the user's own
        // flags would defeat the profile's rules. Refused outright, not
        // filtered, so the user learns why the turn did not run.
        if let Some(bad) = args.iter().find(|arg| {
            arg.contains("bypassPermissions") || arg.as_str() == "--dangerously-skip-permissions"
        }) {
            return Err(CliError::Config(format!(
                "extra.args may not contain \"{bad}\": permission bypass is never allowed for a CLI provider"
            )));
        }

        Ok(Self {
            backend,
            command,
            args,
            mode,
            allow_write: extra
                .get("allow_write")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            timeout_secs,
            max_turns,
            profile,
            profile_overrides,
            system_prompt_mode,
        })
    }

    /// The turn timeout.
    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.timeout_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn extra(v: Value) -> serde_json::Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => panic!("expected object"),
        }
    }

    #[test]
    fn minimal_spec_defaults_to_read_only_agent() {
        let spec = CliSpec::from_extra(&extra(json!({ "backend": "claude" }))).unwrap();
        assert_eq!(spec.backend, "claude");
        assert_eq!(spec.mode, CliMode::Agent);
        assert!(!spec.allow_write, "writing must be opt-in");
        assert_eq!(spec.timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert_eq!(spec.max_turns, None);
        assert!(spec.args.is_empty());
    }

    #[test]
    fn missing_backend_is_an_error() {
        let err = CliSpec::from_extra(&extra(json!({}))).unwrap_err();
        assert!(matches!(err, CliError::Config(_)), "{err:?}");
    }

    #[test]
    fn blank_backend_is_an_error() {
        let err = CliSpec::from_extra(&extra(json!({ "backend": "   " }))).unwrap_err();
        assert!(matches!(err, CliError::Config(_)), "{err:?}");
    }

    #[test]
    fn bad_mode_is_an_error() {
        let err = CliSpec::from_extra(&extra(json!({ "backend": "claude", "mode": "turbo" })))
            .unwrap_err();
        assert!(matches!(err, CliError::Config(_)), "{err:?}");
    }

    #[test]
    fn timeout_is_clamped_both_ways() {
        let low =
            CliSpec::from_extra(&extra(json!({ "backend": "c", "timeout_secs": 1 }))).unwrap();
        assert_eq!(low.timeout_secs, MIN_TIMEOUT_SECS);
        let high = CliSpec::from_extra(&extra(json!({ "backend": "c", "timeout_secs": 999_999 })))
            .unwrap();
        assert_eq!(high.timeout_secs, MAX_TIMEOUT_SECS);
    }

    #[test]
    fn args_keep_only_strings() {
        let spec = CliSpec::from_extra(&extra(
            json!({ "backend": "c", "args": ["--foo", 7, "--bar"] }),
        ))
        .unwrap();
        assert_eq!(spec.args, vec!["--foo".to_string(), "--bar".to_string()]);
    }

    #[test]
    fn zero_max_turns_is_dropped() {
        let spec = CliSpec::from_extra(&extra(json!({ "backend": "c", "max_turns": 0 }))).unwrap();
        assert_eq!(spec.max_turns, None);
    }

    #[test]
    fn profile_keys_default_to_managed_bebok_profile_and_append() {
        let spec = CliSpec::from_extra(&extra(json!({ "backend": "claude" }))).unwrap();
        assert_eq!(spec.profile, ProfileMode::Bebok);
        assert_eq!(spec.profile_overrides, None);
        assert_eq!(spec.system_prompt_mode, SystemPromptMode::Append);
    }

    #[test]
    fn profile_keys_parse_every_documented_value() {
        for (raw, want) in [
            ("bebok", ProfileMode::Bebok),
            ("user", ProfileMode::User),
            (" Isolated ", ProfileMode::Isolated),
        ] {
            let spec = CliSpec::from_extra(&extra(json!({ "backend": "claude", "profile": raw })))
                .unwrap();
            assert_eq!(spec.profile, want, "{raw}");
        }
        let spec = CliSpec::from_extra(&extra(json!({
            "backend": "claude",
            "system_prompt_mode": "replace",
            "profile_overrides": { "env": { "FOO": "1" } },
        })))
        .unwrap();
        assert_eq!(spec.system_prompt_mode, SystemPromptMode::Replace);
        assert_eq!(
            spec.profile_overrides,
            Some(json!({ "env": { "FOO": "1" } }))
        );
    }

    #[test]
    fn bad_profile_keys_are_config_errors() {
        for bad in [
            json!({ "backend": "c", "profile": "yolo" }),
            json!({ "backend": "c", "system_prompt_mode": "prepend" }),
            json!({ "backend": "c", "profile_overrides": ["not", "an", "object"] }),
        ] {
            let err = CliSpec::from_extra(&extra(bad.clone())).unwrap_err();
            assert!(matches!(err, CliError::Config(_)), "{bad}: {err:?}");
        }
    }

    #[test]
    fn permission_bypass_in_extra_args_is_refused() {
        for bad in [
            json!({ "backend": "c", "args": ["--dangerously-skip-permissions"] }),
            json!({ "backend": "c", "args": ["--permission-mode", "bypassPermissions"] }),
        ] {
            let err = CliSpec::from_extra(&extra(bad.clone())).unwrap_err();
            assert!(matches!(err, CliError::Config(_)), "{bad}: {err:?}");
        }
    }
}
