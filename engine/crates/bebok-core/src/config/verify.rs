//! `verify` config section (WP-AUTOVERIFY / F8-1).
//!
//! `verify.frontend` decides whether the agent verifies its own frontend
//! work in the built-in browser without being asked:
//!
//! * `auto` (default) — the system prompt carries the verification policy
//!   (start/reuse the dev server, open the route, screenshot, read the
//!   console, fix and re-check) and the `browser_*` tools default to
//!   `Allow` (except `browser_eval`) so the loop does not stall on prompts;
//! * `ask` — the prompt tells the agent to ask once before verifying;
//!   permission defaults are unchanged (`Ask`);
//! * `off` — no policy text, no permission change.
//!
//! The section is stored like `browser`: global config with a project
//! override (`<project>/.bebok/config.json`), the later layer replacing the
//! earlier one. Malformed values fall back to the default.

use serde_json::Value;

/// `verify.frontend` policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrontendVerify {
    /// Verify autonomously (default).
    #[default]
    Auto,
    /// Ask the user once before verifying.
    Ask,
    /// Never mention or push verification.
    Off,
}

impl FrontendVerify {
    /// Parse the `verify` config section (`{ "frontend": "auto" | "ask" | "off" }`).
    /// Unknown/missing values resolve to [`FrontendVerify::Auto`].
    pub fn from_config(section: &Value) -> Self {
        section
            .get("frontend")
            .and_then(Value::as_str)
            .and_then(Self::parse)
            .unwrap_or_default()
    }

    /// Parse one policy word (case-insensitive, trimmed).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "ask" => Some(Self::Ask),
            "off" | "none" | "never" => Some(Self::Off),
            _ => None,
        }
    }

    /// The canonical config word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ask => "ask",
            Self::Off => "off",
        }
    }

    /// Whether the `browser_*` permission default should switch to `Allow`
    /// (everything but `browser_eval`).
    pub fn auto_allows_browser(self) -> bool {
        self == Self::Auto
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_is_auto() {
        assert_eq!(FrontendVerify::default(), FrontendVerify::Auto);
        assert_eq!(
            FrontendVerify::from_config(&json!({})),
            FrontendVerify::Auto
        );
        assert_eq!(
            FrontendVerify::from_config(&Value::Null),
            FrontendVerify::Auto
        );
    }

    #[test]
    fn parses_every_mode_case_insensitively() {
        assert_eq!(
            FrontendVerify::from_config(&json!({ "frontend": "ask" })),
            FrontendVerify::Ask
        );
        assert_eq!(
            FrontendVerify::from_config(&json!({ "frontend": " OFF " })),
            FrontendVerify::Off
        );
        assert_eq!(
            FrontendVerify::from_config(&json!({ "frontend": "Auto" })),
            FrontendVerify::Auto
        );
    }

    #[test]
    fn malformed_values_fall_back_to_auto() {
        assert_eq!(
            FrontendVerify::from_config(&json!({ "frontend": "sometimes" })),
            FrontendVerify::Auto
        );
        assert_eq!(
            FrontendVerify::from_config(&json!({ "frontend": 3 })),
            FrontendVerify::Auto
        );
    }

    #[test]
    fn only_auto_relaxes_browser_permissions() {
        assert!(FrontendVerify::Auto.auto_allows_browser());
        assert!(!FrontendVerify::Ask.auto_allows_browser());
        assert!(!FrontendVerify::Off.auto_allows_browser());
    }

    #[test]
    fn as_str_round_trips() {
        for mode in [
            FrontendVerify::Auto,
            FrontendVerify::Ask,
            FrontendVerify::Off,
        ] {
            assert_eq!(FrontendVerify::parse(mode.as_str()), Some(mode));
        }
    }
}
