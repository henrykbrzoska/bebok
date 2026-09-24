//! "Build & test policy" system-prompt section.
//!
//! Controls whether the agent is told to run builds and tests itself
//! (`auto`), to ask the user first (`ask`), or to stay silent (`off`).
//!
//! Kept in its own file so the prompt assembler (`services/turn.rs`) adds
//! exactly one line and concurrent work on other assemblers stays
//! conflict-free.

use crate::config::{BuildTestMode, ResolvedConfig};

/// The section for the resolved config, or `None` when the policy is `Off`.
pub fn build_test_section(cfg: &ResolvedConfig) -> Option<String> {
    render(cfg.build_test_mode())
}

/// Render the section for a mode. `Off` returns `None`.
pub fn render(mode: BuildTestMode) -> Option<String> {
    match mode {
        BuildTestMode::Auto => Some(
            "Build & test policy (auto):\n\
             - When the user asks for an implementation, you are expected to run the relevant \
             build and test commands yourself after writing code. Set a bounded timeout for \
             package managers and builds. If a command fails, read the error and fix the cause \
             instead of guessing. Do not ask the user to run builds or tests for you."
                .to_string(),
        ),
        BuildTestMode::Ask => Some(
            "Build & test policy (ask):\n\
             - After implementing changes that would benefit from a build or test run, ask the \
             user once whether you should run them. Do not run builds or tests without \
             permission. When they agree, run the commands and report the results."
                .to_string(),
        ),
        BuildTestMode::Off => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ResolvedConfig;
    use serde_json::json;

    fn cfg(mode: &str) -> ResolvedConfig {
        ResolvedConfig::builder()
            .verify(json!({ "buildTest": mode }))
            .build()
    }

    #[test]
    fn auto_section_contains_expected_text() {
        let section = build_test_section(&cfg("auto")).unwrap();
        assert!(section.contains("Build & test policy (auto)"));
        assert!(section.contains("run the relevant build and test commands yourself"));
        assert!(section.contains("bounded timeout"));
        assert!(section.contains("fix the cause instead of guessing"));
        assert!(!section.contains("ask the user once"));
    }

    #[test]
    fn ask_section_contains_expected_text() {
        let section = build_test_section(&cfg("ask")).unwrap();
        assert!(section.contains("Build & test policy (ask)"));
        assert!(section.contains("ask the user once whether you should run them"));
        assert!(section.contains("Do not run builds or tests without permission"));
        assert!(!section.contains("run the relevant build and test commands yourself"));
    }

    #[test]
    fn off_returns_none() {
        assert!(build_test_section(&cfg("off")).is_none());
        assert!(build_test_section(&cfg("none")).is_none());
        assert!(build_test_section(&cfg("never")).is_none());
    }

    #[test]
    fn default_is_auto() {
        let section = build_test_section(&ResolvedConfig::default()).unwrap();
        assert!(section.contains("Build & test policy (auto)"));
    }

    #[test]
    fn render_round_trips() {
        for mode in [BuildTestMode::Auto, BuildTestMode::Ask, BuildTestMode::Off] {
            assert_eq!(BuildTestMode::parse(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn parse_case_insensitive() {
        assert_eq!(BuildTestMode::parse("Auto"), Some(BuildTestMode::Auto));
        assert_eq!(BuildTestMode::parse("ASK"), Some(BuildTestMode::Ask));
        assert_eq!(BuildTestMode::parse("Off"), Some(BuildTestMode::Off));
    }

    #[test]
    fn malformed_values_fall_back_to_auto() {
        assert_eq!(
            build_test_section(&cfg("sometimes")).unwrap(),
            build_test_section(&ResolvedConfig::default()).unwrap()
        );
    }

    #[test]
    fn assembled_prompt_contains_exactly_one_variant() {
        for (mode, own, other) in [
            (BuildTestMode::Auto, "(auto)", "(ask)"),
            (BuildTestMode::Ask, "(ask)", "(auto)"),
        ] {
            let section = render(mode).unwrap();
            assert!(section.contains(own), "{mode:?} section must contain {own}");
            assert!(
                section.matches(own).count() == 1,
                "{mode:?} section must contain {own} exactly once"
            );
            assert!(
                !section.contains(other),
                "{mode:?} section must not contain {other} (never hold both)"
            );
        }
        assert!(render(BuildTestMode::Off).is_none());
    }
}
