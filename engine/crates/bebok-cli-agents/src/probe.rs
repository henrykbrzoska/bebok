//! Feature probe for an installed CLI binary.
//!
//! `--help` is the only contract a CLI offers about what it supports, so every
//! optional flag Bebok emits must be backed by something the binary printed.
//! This module turns two invocations (`<bin> --version`, `<bin> --help`) into a
//! [`CliFeatures`] value. The result is cached per binary path *and* its
//! modification time: an upgrade replaces the file, which changes the mtime, so
//! the next turn sees the new flag set without an engine restart.
//!
//! Limits, stated plainly: help lists *documented* flags only. Some working
//! flags are hidden from it (`--max-turns` in claude 2.1.325; the
//! `--append-system-prompt-file` form is only implied by the `[-file]`
//! notation). A flag the help does not show is treated as absent, which is the
//! safe direction: the caller falls back to the conservative form.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use crate::runner::capture_stdout;

/// How long each probe invocation may take. On timeout the probe is empty and
/// is not cached, so the next turn tries again.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Longest `--version` line kept; anything longer is a misbehaving binary.
const VERSION_CAP: usize = 120;

/// What one installed binary reports about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliFeatures {
    /// First line of `--version`, capped. `None` when the binary did not answer.
    pub version: Option<String>,
    /// Flags the help text lists as options (`--foo`, `--foo <value>`).
    pub flags: HashSet<String>,
    /// The full `--help` text, for value-level checks such as the
    /// `"stream-json"` choice of `--input-format`.
    pub help: String,
}

impl CliFeatures {
    /// Build from the raw outputs of the two probe invocations.
    pub fn from_outputs(version_stdout: Option<&str>, help: &str) -> Self {
        Self {
            version: version_stdout.and_then(parse_version),
            flags: parse_flags(help),
            help: help.to_string(),
        }
    }

    /// True when the help lists `flag` as an option.
    pub fn has(&self, flag: &str) -> bool {
        self.flags.contains(flag)
    }

    /// Case-sensitive substring match over the help text.
    pub fn mentions(&self, needle: &str) -> bool {
        self.help.contains(needle)
    }

    /// ASCII case-insensitive substring match over the help text. For values
    /// whose spelling the help may vary (`"stream-json"` vs `stream-json`).
    pub fn mentions_ci(&self, needle: &str) -> bool {
        self.help
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
    }

    /// The advertised flags in a stable order, for the profile lock and the
    /// settings card.
    pub fn sorted_flags(&self) -> Vec<String> {
        let mut out: Vec<String> = self.flags.iter().cloned().collect();
        out.sort();
        out
    }
}

/// Every token that looks like a long option at the start of a help line.
///
/// A token counts only when it is a bare `--name` (letters, digits, dashes):
/// `--append-system-prompt[-file],` and `--add-dir,` inside a wrapped
/// description are rejected by the character check, not by position.
pub fn parse_flags(help: &str) -> HashSet<String> {
    help.lines()
        .filter_map(|line| {
            let mut tokens = line.split_whitespace();
            let first = tokens.next()?;
            // `-r, --resume <ID>`: a short alias comes first (clap style).
            if first.starts_with('-') && !first.starts_with("--") && first.ends_with(',') {
                tokens.next()
            } else {
                Some(first)
            }
        })
        .filter(|token| is_long_flag(token))
        .map(str::to_string)
        .collect()
}

fn is_long_flag(token: &str) -> bool {
    token.strip_prefix("--").is_some_and(|rest| {
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// First non-empty line of `--version` output, trimmed and capped.
pub fn parse_version(stdout: &str) -> Option<String> {
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    Some(line.chars().take(VERSION_CAP).collect())
}

type Cache = HashMap<PathBuf, (SystemTime, CliFeatures)>;

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

fn cache() -> &'static Mutex<Cache> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Probe `binary` (`--version` and `--help`), reusing the cached answer while
/// the file's modification time is unchanged.
///
/// A failed `--help` yields an empty [`CliFeatures`] and is not cached.
pub async fn probe_features(binary: &Path) -> CliFeatures {
    let mtime = std::fs::metadata(binary).and_then(|m| m.modified()).ok();
    if let Some(mtime) = mtime
        && let Ok(map) = cache().lock()
        && let Some((seen, hit)) = map.get(binary)
        && *seen == mtime
    {
        return hit.clone();
    }

    let version = capture_stdout(binary, &["--version"], PROBE_TIMEOUT).await;
    let Some(help) = capture_stdout(binary, &["--help"], PROBE_TIMEOUT).await else {
        tracing::debug!(
            binary = %binary.display(),
            "CLI --help probe failed; probe-gated flags stay off"
        );
        return CliFeatures::default();
    };

    let features = CliFeatures::from_outputs(version.as_deref(), &help);
    if let Some(mtime) = mtime
        && let Ok(mut map) = cache().lock()
    {
        map.insert(binary.to_path_buf(), (mtime, features.clone()));
    }
    features
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_alias_before_the_long_flag_is_parsed() {
        let flags =
            parse_flags("  -r, --resume [<ID>]\n      --model <M>\n  -v, --version\n  -p single\n");
        assert!(flags.contains("--resume"));
        assert!(flags.contains("--model"));
        assert!(flags.contains("--version"));
        assert_eq!(flags.len(), 3);
    }

    /// Verbatim excerpts of `claude --help` (2.1.325), including the wrapped
    /// lines that mention flags without defining them.
    const HELP_EXCERPT: &str = "\
Usage: claude [options] [command] [prompt]

Options:
  --agents <json-or-file>               JSON object defining custom agents, or
                                        with --print the path to a file that
                                        holds one (e.g. '{\"reviewer\":
                                        {\"description\": \"Reviews code\",
                                        \"prompt\": \"You are a code reviewer\"}}')
                                        via: --system-prompt[-file],
                                        --append-system-prompt[-file], --add-dir
                                        (CLAUDE.md dirs), --mcp-config,
                                        --settings, --agents, --plugin-dir.
  --input-format <format>               Input format (only works with --print):
                                        \"text\" (default), or \"stream-json\"
                                        (choices: \"text\", \"stream-json\")
  --include-partial-messages            Include partial message chunks as they
                                        arrive (only works with --print and
                                        --output-format=stream-json)
  --mcp-config <configs...>             Load MCP servers from JSON files or
                                        strings
  --plugin-dir <path>                   Load a plugin from a directory or .zip
  --setting-sources <sources>           Comma-separated list of setting sources
  --settings <file-or-json>             Path to a settings JSON file or a JSON
  --strict-mcp-config                   Only use MCP servers from --mcp-config,
  --system-prompt <prompt>              System prompt to use for the session
  --allowedTools <tools...>             Comma or space-separated list
  --allowed-tools <tools...>            Same list, kebab-case spelling
";

    #[test]
    fn long_flags_are_parsed_from_option_definitions_only() {
        let flags = parse_flags(HELP_EXCERPT);
        for expected in [
            "--agents",
            "--input-format",
            "--include-partial-messages",
            "--mcp-config",
            "--plugin-dir",
            "--setting-sources",
            "--settings",
            "--strict-mcp-config",
            "--system-prompt",
            "--allowedTools",
            "--allowed-tools",
        ] {
            assert!(flags.contains(expected), "{expected} missing: {flags:?}");
        }
        // Mentioned mid-line or with bracket notation: not an option of its own.
        assert!(!flags.contains("--append-system-prompt[-file],"));
        assert!(
            !flags.contains("--add-dir"),
            "wrapped mention must not count"
        );
        assert!(!flags.contains("--append-system-prompt-file"));
    }

    #[test]
    fn stream_json_is_matched_regardless_of_quoting_and_case() {
        let features = CliFeatures::from_outputs(None, HELP_EXCERPT);
        assert!(features.has("--input-format"));
        assert!(features.mentions_ci("stream-json"));
        assert!(features.mentions_ci("STREAM-JSON"));
        // The quotes are a help-formatting detail, not part of the contract:
        // an unquoted spelling must match the same way.
        let unquoted = CliFeatures::from_outputs(None, "  --input-format <f>  stream-json\n");
        assert!(unquoted.mentions_ci("stream-json"));
        assert!(!unquoted.mentions_ci("stream-xml"));
    }

    #[test]
    fn version_is_the_first_non_empty_line_capped() {
        assert_eq!(
            parse_version("\n  2.1.325 (Claude Code)  \nsecond\n").as_deref(),
            Some("2.1.325 (Claude Code)")
        );
        assert_eq!(parse_version("  \n "), None);
        let long = "v".repeat(VERSION_CAP + 40);
        assert_eq!(parse_version(&long).unwrap().chars().count(), VERSION_CAP);
    }

    #[test]
    fn features_report_flags_in_sorted_order() {
        let features = CliFeatures::from_outputs(Some("1.0.0"), "  --zeta\n  --alpha <x>\n");
        assert_eq!(features.sorted_flags(), vec!["--alpha", "--zeta"]);
        assert_eq!(features.version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn empty_help_advertises_nothing() {
        let features = CliFeatures::from_outputs(None, "");
        assert!(features.flags.is_empty());
        assert!(!features.has("--settings"));
        assert!(features.version.is_none());
    }

    #[tokio::test]
    async fn missing_binary_probes_to_defaults_and_is_not_cached() {
        let path = Path::new("/nonexistent/bebok-probe-features");
        let features = probe_features(path).await;
        assert_eq!(features, CliFeatures::default());
        assert!(cache().lock().unwrap().get(path).is_none());
    }
}
