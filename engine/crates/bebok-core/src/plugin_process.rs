//! Subprocess-based plugin driver.
//!
//! A *dynamic plugin* is a standalone binary spawned from a plugin slot dir
//! (`<project>/.bebok/plugins/<name>/`). The engine communicates with it
//! via **JSON-lines over stdio**: one JSON object per line on stdin/stdout.
//!
//! Protocol (one request → one response):
//!
//! ```json
//! {"action": "status"}  →  {"ok": true, "status": "ready", ...}
//! {"action": "search", "query": "foo"}  →  {"ok": true, "results": [...]}
//! ```
//!
//! The binary is found from the manifest's `entrypoint` field (or falls back
//! to `<slot_dir>/<name> --plugin-server`). If the binary is not on disk,
//! [`PluginProcess::invoke`] returns `None` — graceful degradation, never a
//! panic.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

use crate::error::{CoreError, Result};

/// Default timeout for one plugin invocation.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Split an entrypoint command string into argv, honouring single and
/// double quotes. This matters on Windows where e.g. `%TEMP%` often
/// contains spaces (`C:\Users\Jan Kowalski\...`): a naive
/// `split_whitespace` would shred a quoted script path into several
/// bogus arguments and the child would exit immediately.
fn split_entrypoint(cmd: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut in_arg = false;
    for ch in cmd.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                } else {
                    cur.push(ch);
                }
            }
            None => match ch {
                '"' | '\'' => {
                    quote = Some(ch);
                    in_arg = true;
                }
                c if c.is_whitespace() => {
                    if in_arg {
                        parts.push(std::mem::take(&mut cur));
                        in_arg = false;
                    }
                }
                _ => {
                    cur.push(ch);
                    in_arg = true;
                }
            },
        }
    }
    if in_arg {
        parts.push(cur);
    }
    parts
}

/// Handle to a running subprocess plugin.
///
/// The process is spawned lazily on first [`invoke`](Self::invoke) and kept
/// alive for subsequent calls. Dropping the handle kills the child.
pub struct PluginProcess {
    /// Plugin id (e.g. `"bebok-index"`).
    name: String,
    /// Command to execute (may be bare name like `"sh"` for PATH resolution,
    /// or an absolute path).
    command: String,
    /// Extra arguments (from manifest `entrypoint`, parsed).
    args: Vec<String>,
    /// Slot directory (working directory for the child).
    slot_dir: PathBuf,
    /// Lazily spawned child; `None` until first invoke.
    child: Option<Child>,
}

impl PluginProcess {
    /// Build a process handle from a manifest's entrypoint + slot dir.
    ///
    /// `entrypoint` is the full command string (e.g.
    /// `"bebok-index --plugin-server"`). When `None`, the default is
    /// `<slot_dir>/<name> --plugin-server`.
    pub fn new(name: &str, slot_dir: PathBuf, entrypoint: Option<&str>) -> Self {
        let default_ep = format!("{name} --plugin-server");
        let args_str = entrypoint.unwrap_or(&default_ep);
        let mut parts: Vec<String> = split_entrypoint(args_str);
        if parts.is_empty() {
            parts.push(default_ep.clone());
        }
        let mut command = parts.remove(0);
        // Strip stray surrounding quotes (defensive: split_entrypoint
        // already strips them, but a hand-written entrypoint may quote
        // the binary itself).
        if command.len() >= 2
            && ((command.starts_with('"') && command.ends_with('"'))
                || (command.starts_with('\'') && command.ends_with('\'')))
        {
            command = command[1..command.len() - 1].to_string();
        }
        Self {
            name: name.to_string(),
            command,
            args: parts,
            slot_dir,
            child: None,
        }
    }

    /// Resolve the command to an absolute path for the `exists()` check.
    /// Returns `Some(absolute_path)` when the binary can be located on disk,
    /// `None` when it should be resolved via PATH at spawn time (e.g. `"sh"`).
    fn resolved_binary(&self) -> Option<PathBuf> {
        if Path::new(&self.command).is_absolute() {
            let p = PathBuf::from(&self.command);
            if p.exists() {
                return Some(p);
            }
            // On Windows, also try with .exe suffix.
            #[cfg(windows)]
            {
                let p_exe = PathBuf::from(format!("{}.exe", self.command));
                if p_exe.exists() {
                    return Some(p_exe);
                }
            }
            return None;
        }
        // Try slot_dir first, then fall back to PATH.
        let local = self.slot_dir.join(&self.command);
        if local.exists() {
            return Some(local);
        }
        // On Windows, also try with .exe suffix in the slot dir.
        #[cfg(windows)]
        {
            let local_exe = self.slot_dir.join(format!("{}.exe", self.command));
            if local_exe.exists() {
                return Some(local_exe);
            }
        }
        // Try to find on PATH via `which` (`where` on Windows).
        let probe = if cfg!(windows) { "where" } else { "which" };
        std::process::Command::new(probe)
            .arg(&self.command)
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    // `where` on Windows may list several matches (one
                    // per line); take the first usable line instead of
                    // treating the whole stdout blob as one path.
                    let path = String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .map(str::trim)
                        .find(|l| !l.is_empty())
                        .unwrap_or("")
                        .to_string();
                    if !path.is_empty() {
                        Some(PathBuf::from(path))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
    }

    /// Ensure the child process is running. Returns `Ok(())` when the
    /// process is up (or was already running). If the command can't be
    /// located, returns an error.
    fn ensure_running(&mut self) -> Result<()> {
        // If we already have a child, check if it's still alive.
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    // Process exited — respawn below.
                    self.child = None;
                }
                Ok(None) => return Ok(()), // Still running.
                Err(_) => {
                    self.child = None;
                }
            }
        }

        // Spawn via the resolved absolute path when the binary lives in the
        // slot dir: a bare command (`bebok-index`) resolves via PATH, not via
        // `current_dir`, so spawning it bare fails with ENOENT even though
        // the binary sits right next to us. `invoke` already verified
        // `resolved_binary().is_some()`, so this is guaranteed to find it.
        let program = self
            .resolved_binary()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.command.clone());
        let child = Command::new(&program)
            .args(&self.args)
            .current_dir(&self.slot_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()) // plugin logs go elsewhere
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                CoreError::Other(format!(
                    "failed to spawn plugin '{}' (command: {}): {e}",
                    self.name, self.command,
                ))
            })?;

        self.child = Some(child);
        Ok(())
    }

    /// Send a JSON-lines request and read one JSON-lines response.
    ///
    /// Returns `Ok(None)` when the binary is missing (graceful degradation).
    pub async fn invoke(&mut self, action: &str, input: &Value) -> Result<Option<Value>> {
        if self.resolved_binary().is_none() {
            return Ok(None);
        }

        self.ensure_running()?;

        let child = self
            .child
            .as_mut()
            .ok_or_else(|| CoreError::Other(format!("plugin '{}' not running", self.name)))?;

        // Build the request envelope.
        let mut request = input.clone();
        if let Some(obj) = request.as_object_mut() {
            obj.insert("action".to_string(), Value::String(action.to_string()));
        } else {
            let mut obj = serde_json::Map::new();
            obj.insert("action".to_string(), Value::String(action.to_string()));
            obj.insert("payload".to_string(), input.clone());
            request = Value::Object(obj);
        }

        let mut line = serde_json::to_string(&request).map_err(CoreError::Json)?;
        line.push('\n');

        // Write request to stdin.
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| CoreError::Other(format!("plugin '{}' stdin unavailable", self.name)))?;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(CoreError::Io)?;
        stdin.flush().await.map_err(CoreError::Io)?;

        // Read one line from stdout (with timeout).
        let stdout = child.stdout.as_mut().ok_or_else(|| {
            CoreError::Other(format!("plugin '{}' stdout unavailable", self.name))
        })?;
        let mut reader = BufReader::new(stdout);
        let mut response_line = String::new();

        tokio::select! {
            result = reader.read_line(&mut response_line) => {
                match result {
                    Ok(0) => {
                        // EOF — plugin crashed or exited.
                        self.child = None;
                        return Err(CoreError::Other(format!(
                            "plugin '{}' closed stdout (possibly crashed)",
                            self.name
                        )));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        return Err(CoreError::Other(format!(
                            "plugin '{}' read error: {e}",
                            self.name
                        )));
                    }
                }
            }
            _ = tokio::time::sleep(DEFAULT_TIMEOUT) => {
                // Kill the stuck process by dropping it (kill_on_drop(true)).
                self.child = None;
                return Err(CoreError::Other(format!(
                    "plugin '{}' timed out after {DEFAULT_TIMEOUT:?}",
                    self.name
                )));
            }
        }

        let response_line = response_line.trim();
        if response_line.is_empty() {
            return Ok(None);
        }

        let value: Value = serde_json::from_str(response_line).map_err(|e| {
            CoreError::Other(format!("plugin '{}' returned invalid JSON: {e}", self.name))
        })?;

        Ok(Some(value))
    }

    /// Kill the subprocess (if running).
    pub fn kill(&mut self) {
        // Dropping a tokio `Child` kills the process (kill_on_drop(true)).
        self.child.take();
    }

    /// Plugin name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Command name (may be bare name or absolute path).
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Resolved binary path, if locatable on disk.
    pub fn binary_path(&self) -> Option<PathBuf> {
        self.resolved_binary()
    }
}

/// Load a dynamic plugin from a slot directory.
///
/// Reads `bebok-plugin.json` manifest, resolves the entrypoint, and returns
/// a [`DynamicPlugin`] ready for registration on [`PluginHost`](crate::PluginHost).
///
/// Returns `Err` when:
/// - The manifest is missing or invalid.
/// - The slot directory does not exist.
pub fn load_dynamic_plugin(slot_dir: &Path) -> crate::error::Result<DynamicPlugin> {
    if !slot_dir.is_dir() {
        return Err(crate::error::CoreError::BadRequest(format!(
            "plugin slot directory does not exist: {}",
            slot_dir.display()
        )));
    }
    let manifest = crate::plugin_registry::read_manifest(slot_dir)?;
    let name = manifest.name;
    // Choose platform-specific entrypoint: entrypoint_windows on Windows,
    // entrypoint_unix on Unix, else generic entrypoint.
    let entrypoint = if cfg!(windows) {
        manifest.entrypoint_windows.or(manifest.entrypoint)
    } else {
        manifest.entrypoint_unix.or(manifest.entrypoint)
    };
    Ok(DynamicPlugin::new(
        &name,
        slot_dir.to_path_buf(),
        entrypoint,
    ))
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

#[async_trait::async_trait]
impl crate::plugin::BebokPlugin for DynamicPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    async fn on_action(
        &self,
        action: &str,
        input: &serde_json::Value,
    ) -> Option<serde_json::Value> {
        match self.invoke_action(action, input).await {
            Ok(Some(resp)) => Some(resp),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(
                    "dynamic plugin '{}' action '{}' failed: {e}",
                    self.name,
                    action
                );
                None
            }
        }
    }
}

/// A thin wrapper that implements [`BebokPlugin`] and delegates
/// [`on_action`](crate::BebokPlugin::on_action) to a [`PluginProcess`].
pub struct DynamicPlugin {
    name: String,
    process: tokio::sync::Mutex<PluginProcess>,
    slot_dir: PathBuf,
    entrypoint: Option<String>,
}

impl DynamicPlugin {
    pub fn new(name: &str, slot_dir: PathBuf, entrypoint: Option<String>) -> Self {
        let process = PluginProcess::new(name, slot_dir.clone(), entrypoint.as_deref());
        Self {
            name: name.to_string(),
            process: tokio::sync::Mutex::new(process),
            slot_dir,
            entrypoint,
        }
    }

    /// Invoke an action on the subprocess plugin.
    pub async fn invoke_action(&self, action: &str, input: &Value) -> Result<Option<Value>> {
        let mut proc = self.process.lock().await;
        proc.invoke(action, input).await
    }

    /// Kill the subprocess.
    pub async fn kill(&self) {
        let mut proc = self.process.lock().await;
        proc.kill();
    }

    /// Slot directory.
    pub fn slot_dir(&self) -> &Path {
        &self.slot_dir
    }

    /// Entrypoint command, if any.
    pub fn entrypoint(&self) -> Option<&str> {
        self.entrypoint.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Binary that echoes back `{"ok":true,"action":"<action>"}` for any
    /// request — a perfect subprocess stub for integration tests.
    ///
    /// On Windows the stub is a PowerShell script driven via `powershell`
    /// (ships with Windows itself); on Unix a POSIX shell script driven via
    /// `sh` (neither `sh` nor `awk` exist on a stock Windows runner).
    fn echo_stub_path() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bebok-plugin-proc-stub-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(windows)]
        {
            let script = dir.join("echo-stub.ps1");
            std::fs::write(
                &script,
                "while (($line = [Console]::In.ReadLine()) -ne $null) {\r\n  if ($line -match '\"action\"\\s*:\\s*\"([^\"]+)\"') { $action = $Matches[1] } else { $action = \"unknown\" }\r\n  '{\"ok\":true,\"action\":\"' + $action + '\"}'\r\n}\r\n",
            )
            .unwrap();
        }
        #[cfg(not(windows))]
        {
            let script = dir.join("echo-stub.sh");
            std::fs::write(
                &script,
                r#"#!/bin/sh
while IFS= read -r line; do
  action=$(echo "$line" | awk -F'"action"' '{split($2,a,"\""); print a[2]}')
  [ -z "$action" ] && action="unknown"
  printf '{"ok":true,"action":"%s"}\n' "$action"
done
"#,
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        dir
    }

    /// Shell command launching the echo stub: `sh <script>` on Unix,
    /// `powershell -NoProfile -ExecutionPolicy Bypass -File "<script>"` on
    /// Windows (quoted: `%TEMP%` often contains spaces). Prefers `pwsh`
    /// (PowerShell 7) when installed, falls back to inbox `powershell`.
    #[cfg(windows)]
    fn ps_command() -> &'static str {
        if std::process::Command::new("where")
            .arg("pwsh")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            "pwsh"
        } else {
            "powershell"
        }
    }
    #[cfg(windows)]
    fn echo_stub_command(script: &std::path::Path) -> String {
        format!(
            "{} -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"{}\" --plugin-server",
            ps_command(),
            script.display()
        )
    }
    #[cfg(not(windows))]
    fn echo_stub_command(script: &std::path::Path) -> String {
        format!("sh {} --plugin-server", script.display())
    }

    #[tokio::test]
    async fn process_invoke_roundtrip() {
        let dir = echo_stub_path();
        #[cfg(windows)]
        let script = dir.join("echo-stub.ps1");
        #[cfg(not(windows))]
        let script = dir.join("echo-stub.sh");

        // Use `<shell> <script>` as the binary so the stub runs via shell.
        let mut proc = PluginProcess::new("test", dir.clone(), Some(&echo_stub_command(&script)));

        let result = proc
            .invoke("status", &serde_json::json!({"project": "/tmp"}))
            .await
            .unwrap();
        let resp = result.expect("expected response");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["action"], "status");

        let result = proc
            .invoke("search", &serde_json::json!({"query": "fn main"}))
            .await
            .unwrap();
        let resp = result.expect("expected response");
        assert_eq!(resp["action"], "search");

        proc.kill();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn entrypoint_split_honours_quotes() {
        assert_eq!(
            split_entrypoint("sh foo.sh --plugin-server"),
            vec!["sh", "foo.sh", "--plugin-server"]
        );
        assert_eq!(
            split_entrypoint(
                "powershell -File \"C:\\Users\\Jan Kowalski\\stub.ps1\" --plugin-server"
            ),
            vec![
                "powershell",
                "-File",
                "C:\\Users\\Jan Kowalski\\stub.ps1",
                "--plugin-server"
            ]
        );
    }

    #[test]
    fn missing_binary_returns_none() {
        let dir = PathBuf::from("/nonexistent/plugin/dir");
        let proc = PluginProcess::new("ghost", dir, None);

        // invoke is async but the binary check is sync — we can verify
        // the command is set up correctly.
        assert_eq!(proc.command(), "ghost");
    }

    #[tokio::test]
    async fn invoke_on_missing_binary_returns_none() {
        let dir = PathBuf::from(format!("/tmp/bebok-missing-{}", uuid::Uuid::new_v4()));
        let mut proc = PluginProcess::new("ghost", dir, None);
        let result = proc.invoke("status", &serde_json::json!({})).await.unwrap();
        assert!(result.is_none(), "missing binary must return None");
    }
}
