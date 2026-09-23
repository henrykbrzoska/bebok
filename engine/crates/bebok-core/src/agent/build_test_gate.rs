//! Build-test gate: blocks bash commands that look like test runners when
//! `verify.buildTest` is `Off`.
//!
//! This is a hard gate — it runs *before* the permission engine so that
//! test commands are rejected even when the user has a catch-all `allow`
//! rule for `bash(*)`.  The denial message explains the policy and
//! suggests switching to `auto` or `ask`.

use super::exec::ToolOutcome;
use crate::config::{BuildTestMode, ResolvedConfig};

/// Check whether a `bash` command looks like it runs a test suite.
///
/// The check is intentionally conservative: it looks for well-known
/// test-runner invocations after stripping prefixes (`sudo`, `nice`,
/// `env VAR=x`, etc.) and splitting on shell operators (`|`, `&&`,
/// `||`, `;`).
pub fn looks_like_test_command(command: &str) -> bool {
    let segments: Vec<&str> = split_shell_segments(command);
    segments.iter().any(|seg| segment_looks_like_test(seg))
}

/// Split a shell command on common operators (`|`, `||`, `&&`, `;`),
/// yielding the individual segments.  Empty segments are dropped.
fn split_shell_segments(command: &str) -> Vec<&str> {
    let mut result: Vec<&str> = Vec::new();
    let bytes = command.as_bytes();
    let mut start = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'|' && i + 1 < bytes.len() && bytes[i + 1] == b'|' {
            if start < i {
                result.push(&command[start..i]);
            }
            start = i + 2;
            i += 2;
        } else if bytes[i] == b'|' {
            if start < i {
                result.push(&command[start..i]);
            }
            start = i + 1;
            i += 1;
        } else if bytes[i] == b'&' && i + 1 < bytes.len() && bytes[i + 1] == b'&' {
            if start < i {
                result.push(&command[start..i]);
            }
            start = i + 2;
            i += 2;
        } else if bytes[i] == b';' {
            if start < i {
                result.push(&command[start..i]);
            }
            start = i + 1;
            i += 1;
        } else {
            i += 1;
        }
    }
    if start < bytes.len() {
        result.push(&command[start..]);
    }
    result
}

/// Does a single (already-split) segment look like a test command?
fn segment_looks_like_test(seg: &str) -> bool {
    let seg = seg.trim_start();
    if seg.is_empty() {
        return false;
    }

    // Strip well-known non-command prefixes.
    let after_prefixes = strip_prefixes(seg);

    // Strip `python -m <mod>`, `npx`, `pnpm exec`, `npm exec`.
    let after_wrappers = strip_tool_wrappers(after_prefixes);

    // Determine the first token (the runner command, possibly with a
    // leading `./` or `/path/to/`).
    let first_token = normalize_runner(after_wrappers.split_whitespace().next().unwrap_or(""));

    // Bare POSIX `test` / `[` / `[[` are NOT test runners.
    if matches!(first_token.as_str(), "test" | "[" | "[[") {
        return false;
    }

    // --- Single-level runners: the command itself IS the test runner.
    if matches!(
        first_token.as_str(),
        "pytest"
            | "py.test"
            | "tox"
            | "nox"
            | "phpunit"
            | "rspec"
            | "jest"
            | "vitest"
            | "mocha"
            | "karma"
            | "ava"
            | "tap"
            | "unittest"
    ) {
        return true;
    }

    // --- Multi-level runners: need a test-related subcommand.
    let mut tokens = after_wrappers.split_whitespace();
    let _runner = tokens.next(); // skip runner (already validated above)
    let sub = tokens.next().unwrap_or("");

    match first_token.as_str() {
        "cargo" => matches!(sub, "test" | "t"),
        "go" => sub == "test",
        "npm" | "yarn" | "pnpm" | "bun" => match sub {
            "test" => true,
            "run" => tokens.next().is_some_and(|a| a.starts_with("test")),
            _ => false,
        },
        "gradle" | "gradlew" | "mvn" => {
            matches!(
                sub,
                "test" | "check" | "verify" | "integrationTest" | "functionalTest"
            )
        }
        "make" | "gmake" => matches!(sub, "test" | "check"),
        _ => false,
    }
}

/// Normalize a runner name: strip leading `./` and path prefixes so
/// `./gradlew`, `../bin/pytest`, etc. match on the bare command name.
fn normalize_runner(token: &str) -> String {
    // `./gradlew` → `gradlew`, `/usr/bin/make` → `make`.
    let base = token.rsplit('/').next().unwrap_or(token);
    let base = base.strip_prefix("./").unwrap_or(base);
    base.to_string()
}

// ── Prefix stripping ──────────────────────────────────────────────

/// Strip leading `env`, `sudo`, `nice`, `nohup`, and `VAR=val`
/// assignments.
fn strip_prefixes(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        if let Some(rest) = s.strip_prefix("sudo ") {
            s = rest;
            continue;
        }
        if let Some(rest) = s.strip_prefix("nice ") {
            s = rest;
            continue;
        }
        if let Some(rest) = s.strip_prefix("nohup ") {
            s = rest;
            continue;
        }
        if let Some(rest) = s.strip_prefix("env ") {
            s = rest;
            continue;
        }
        // VAR=value  (identifier before `=`)
        if let Some(sp) = s.find(' ') {
            let candidate = &s[..sp];
            if let Some(eq_pos) = candidate.find('=') {
                let before_eq = &candidate[..eq_pos];
                if !before_eq.is_empty()
                    && !before_eq.starts_with('-')
                    && before_eq
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    s = &s[sp + 1..];
                    continue;
                }
            }
        }
        break;
    }
    s
}

/// Strip `python -m <mod>`, `python3 -m <mod>`, `npx`, `pnpm exec`,
/// `npm exec`.
fn strip_tool_wrappers(mut s: &str) -> &str {
    s = s.trim_start();
    if let Some(rest) = strip_one_of(s, &["python ", "python3 "]) {
        let rest_trimmed = rest.trim_start();
        if let Some(after_flag) = rest_trimmed.strip_prefix("-m ") {
            // `python -m <mod> [args...]` → return `<mod> [args...]`
            // so the caller sees the module name as the runner.
            return after_flag;
        } else {
            // `python <script>` — the script is the command.
            s = rest;
        }
    }
    if let Some(rest) = strip_one_of(s, &["npx ", "pnpm exec ", "npm exec "]) {
        s = rest;
    }
    s
}

fn strip_one_of<'a>(s: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    for p in prefixes {
        if let Some(rest) = s.strip_prefix(p) {
            return Some(rest);
        }
    }
    None
}

// ── Public gate ───────────────────────────────────────────────────

/// The high-level gate: returns `Some(Denied)` when the tool call should
/// be blocked by the build-test policy, `None` otherwise.
pub fn check_build_test_policy(
    config: &ResolvedConfig,
    tool_name: &str,
    input: &serde_json::Value,
) -> Option<ToolOutcome> {
    if config.build_test_mode() != BuildTestMode::Off {
        return None;
    }
    if tool_name != "bash" {
        return None;
    }
    let command = input.get("command").and_then(|v| v.as_str())?;
    if looks_like_test_command(command) {
        Some(ToolOutcome::Denied(
            "build-test policy is off: test commands are blocked. \
             Set `verify.buildTest` to \"auto\" or \"ask\" to allow tests.",
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ResolvedConfig;

    fn cfg(mode: &str) -> ResolvedConfig {
        ResolvedConfig::builder()
            .verify(serde_json::json!({ "buildTest": mode }))
            .build()
    }

    // ── looks_like_test_command: positive ─────────────────────────────

    #[test]
    fn cargo_test() {
        assert!(looks_like_test_command("cargo test"));
        assert!(looks_like_test_command("cargo t"));
        assert!(looks_like_test_command(
            "CARGO_INCREMENTAL=1 cargo test -- --nocapture"
        ));
    }

    #[test]
    fn go_test() {
        assert!(looks_like_test_command("go test ./..."));
        assert!(looks_like_test_command("go test -v -run TestFoo"));
    }

    #[test]
    fn npm_test_variants() {
        assert!(looks_like_test_command("npm test"));
        assert!(looks_like_test_command("yarn test"));
        assert!(looks_like_test_command("pnpm test"));
        assert!(looks_like_test_command("bun test"));
        assert!(looks_like_test_command("npm run test"));
        assert!(looks_like_test_command("npm run test:unit"));
        assert!(looks_like_test_command("yarn run test:coverage"));
    }

    #[test]
    fn python_test_runners() {
        assert!(looks_like_test_command("pytest"));
        assert!(looks_like_test_command("pytest tests/"));
        assert!(looks_like_test_command("py.test"));
        assert!(looks_like_test_command("tox"));
        assert!(looks_like_test_command("nox"));
        assert!(looks_like_test_command("python -m pytest"));
        assert!(looks_like_test_command("python3 -m pytest tests/"));
        assert!(looks_like_test_command("unittest"));
    }

    #[test]
    fn php_ruby_test_runners() {
        assert!(looks_like_test_command("phpunit"));
        assert!(looks_like_test_command("rspec"));
    }

    #[test]
    fn js_test_frameworks() {
        assert!(looks_like_test_command("jest"));
        assert!(looks_like_test_command("vitest"));
        assert!(looks_like_test_command("mocha"));
        assert!(looks_like_test_command("karma"));
        assert!(looks_like_test_command("ava"));
        assert!(looks_like_test_command("tap"));
        assert!(looks_like_test_command("npx jest"));
        assert!(looks_like_test_command("npx vitest run"));
    }

    #[test]
    fn java_build_tools() {
        assert!(looks_like_test_command("gradle test"));
        assert!(looks_like_test_command("./gradlew test"));
        assert!(looks_like_test_command("mvn test"));
        assert!(looks_like_test_command("mvn verify"));
    }

    #[test]
    fn make_test() {
        assert!(looks_like_test_command("make test"));
        assert!(looks_like_test_command("make check"));
        assert!(looks_like_test_command("gmake test"));
    }

    #[test]
    fn piped_test_command() {
        assert!(looks_like_test_command("cargo test 2>&1 | head -20"));
        assert!(looks_like_test_command("echo prep && cargo test"));
        assert!(looks_like_test_command("cargo test || echo failed"));
    }

    // ── looks_like_test_command: negative ─────────────────────────────

    #[test]
    fn cargo_non_test_subcommands() {
        assert!(!looks_like_test_command("cargo build"));
        assert!(!looks_like_test_command("cargo run"));
        assert!(!looks_like_test_command("cargo clippy"));
        assert!(!looks_like_test_command("cargo fmt"));
        assert!(!looks_like_test_command(
            "cargo build --release --target x86_64-unknown-linux-gnu"
        ));
    }

    #[test]
    fn go_non_test() {
        assert!(!looks_like_test_command("go build"));
        assert!(!looks_like_test_command("go run main.go"));
    }

    #[test]
    fn npm_non_test() {
        assert!(!looks_like_test_command("npm install"));
        assert!(!looks_like_test_command("npm start"));
        assert!(!looks_like_test_command("yarn install"));
        assert!(!looks_like_test_command("pnpm install"));
    }

    #[test]
    fn non_test_commands() {
        assert!(!looks_like_test_command("echo hello"));
        assert!(!looks_like_test_command("ls -la"));
        assert!(!looks_like_test_command("pwd"));
        assert!(!looks_like_test_command("cat file.txt"));
        assert!(!looks_like_test_command("sudo echo hi"));
        assert!(!looks_like_test_command("VAR=1 cargo build"));
        assert!(!looks_like_test_command("nohup cargo run &"));
        assert!(!looks_like_test_command("git status"));
        assert!(!looks_like_test_command("cargo fmt -- --check"));
        assert!(!looks_like_test_command("pip install pytest"));
    }

    #[test]
    fn posix_test_builtin_not_blocked() {
        assert!(!looks_like_test_command("test -f file.txt"));
        assert!(!looks_like_test_command("[ -f file.txt ]"));
        assert!(!looks_like_test_command("[[ -d dir ]]"));
    }

    // ── looks_like_test_command: real-world ───────────────────────────

    #[test]
    fn real_world_commands() {
        assert!(looks_like_test_command(
            "BEBOK_NO_AUTH=1 cargo test --workspace"
        ));
        assert!(looks_like_test_command("sudo npm test"));
        assert!(looks_like_test_command(
            "env RUST_LOG=debug cargo test -p bebok-core"
        ));
        assert!(!looks_like_test_command(
            "cargo build --release --target x86_64-unknown-linux-gnu"
        ));
        assert!(!looks_like_test_command("git status"));
        assert!(!looks_like_test_command("cargo fmt -- --check"));
    }

    // ── check_build_test_policy ───────────────────────────────────────

    #[test]
    fn off_blocks_bash_cargo_test() {
        let input = serde_json::json!({ "command": "cargo test" });
        let result = check_build_test_policy(&cfg("off"), "bash", &input);
        assert!(result.is_some());
        assert!(matches!(result.unwrap(), ToolOutcome::Denied(_)));
    }

    #[test]
    fn off_blocks_npm_test() {
        let input = serde_json::json!({ "command": "npm test" });
        assert!(check_build_test_policy(&cfg("off"), "bash", &input).is_some());
    }

    #[test]
    fn off_blocks_pytest() {
        let input = serde_json::json!({ "command": "pytest" });
        assert!(check_build_test_policy(&cfg("off"), "bash", &input).is_some());
    }

    #[test]
    fn off_blocks_go_test() {
        let input = serde_json::json!({ "command": "go test ./..." });
        assert!(check_build_test_policy(&cfg("off"), "bash", &input).is_some());
    }

    #[test]
    fn off_blocks_make_test() {
        let input = serde_json::json!({ "command": "make test" });
        assert!(check_build_test_policy(&cfg("off"), "bash", &input).is_some());
    }

    #[test]
    fn off_blocks_npx_jest() {
        let input = serde_json::json!({ "command": "npx jest" });
        assert!(check_build_test_policy(&cfg("off"), "bash", &input).is_some());
    }

    #[test]
    fn auto_passes_all() {
        let input = serde_json::json!({ "command": "cargo test" });
        assert!(check_build_test_policy(&cfg("auto"), "bash", &input).is_none());
    }

    #[test]
    fn ask_passes_all() {
        let input = serde_json::json!({ "command": "cargo test" });
        assert!(check_build_test_policy(&cfg("ask"), "bash", &input).is_none());
    }

    #[test]
    fn non_bash_tool_passes() {
        let input = serde_json::json!({ "command": "cargo test" });
        assert!(check_build_test_policy(&cfg("off"), "read_file", &input).is_none());
        assert!(check_build_test_policy(&cfg("off"), "exec", &input).is_none());
    }

    #[test]
    fn non_test_command_passes_even_when_off() {
        let input = serde_json::json!({ "command": "cargo build" });
        assert!(check_build_test_policy(&cfg("off"), "bash", &input).is_none());
        let input = serde_json::json!({ "command": "echo hello" });
        assert!(check_build_test_policy(&cfg("off"), "bash", &input).is_none());
    }

    #[test]
    fn missing_command_field_passes() {
        let input = serde_json::json!({});
        assert!(check_build_test_policy(&cfg("off"), "bash", &input).is_none());
    }

    #[test]
    fn denied_message_suggests_policy_change() {
        let input = serde_json::json!({ "command": "cargo test" });
        let outcome = check_build_test_policy(&cfg("off"), "bash", &input).unwrap();
        if let ToolOutcome::Denied(msg) = outcome {
            assert!(msg.contains("auto"), "should suggest auto: {msg}");
            assert!(msg.contains("ask"), "should suggest ask: {msg}");
        } else {
            panic!("expected Denied");
        }
    }
}
