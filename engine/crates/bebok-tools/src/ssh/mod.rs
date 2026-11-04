//! Persistent SSH sessions (the `ssh` tool).
//!
//! The problem this solves: `ssh host "cmd"` per command pays a full TCP
//! handshake + authentication for every call and — worse — loses all shell
//! state, so `cd /var/www` in one command has no effect on the next one.
//!
//! Instead, [`SshRegistry`] keeps **one** long-lived `ssh -T` child process per
//! session. Its stdin/stdout are the remote shell's stdin/stdout, so commands
//! run in the same shell: `cd`, exported variables, shell functions and the
//! remote environment persist between calls. Every command is framed with a
//! random marker so the engine knows exactly where the output ends and what the
//! exit status and the resulting `$PWD` were:
//!
//! ```text
//! {
//! <command>
//! } 2>&1
//! printf '\n__BEBOK_EOF_<nonce>__ %s %s\n' "$?" "$PWD"
//! ```
//!
//! Authentication: a cheap `BatchMode=yes` probe decides whether the host
//! accepts key/agent auth. When it does not, the registry publishes an
//! [`SshPrompt`] (the client shows a password dialog) and feeds the answer to
//! the system `ssh` client through `SSH_ASKPASS` — the secret never reaches the
//! transcript, the model or the logs, and the temporary askpass script plus
//! password file are deleted as soon as the connection is up.
//!
//! Sessions are a process-wide singleton ([`SshRegistry::global`]) so the HTTP
//! layer can list them and deliver a password prompt while a tool call is
//! blocked waiting for the user.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub mod tools;

pub use tools::SshTool;

/// Largest command output kept for the model (the tool-output cap trims further).
const MAX_OUTPUT_BYTES: usize = 200 * 1024;
/// Largest amount of `ssh`'s own stderr kept for diagnostics.
const MAX_STDERR_BYTES: usize = 64 * 1024;
/// The non-interactive auth probe must answer quickly.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a `connect` waits for the user to type a password.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(300);
/// Default per-command timeout.
pub const DEFAULT_RUN_TIMEOUT_MS: u64 = 120_000;
/// Upper bound for the per-command timeout.
pub const MAX_RUN_TIMEOUT_MS: u64 = 600_000;
/// Live sessions kept at once (each holds an SSH connection).
const MAX_SESSIONS: usize = 8;

/// A password prompt the user has to answer in the UI.
///
/// `requestID` / `sessionID` keep the caps the rest of the wire API uses
/// (`permission.asked`, the `POST /ssh/prompt/{requestID}` path); a plain
/// camelCase rename would emit `requestId` and the client would drop the
/// event as unparseable.
#[derive(Debug, Clone, Serialize)]
pub struct SshPrompt {
    #[serde(rename = "requestID")]
    pub request_id: String,
    /// Bebok session that is trying to connect.
    #[serde(rename = "sessionID")]
    pub session_id: String,
    /// Project directory (the client routes events by it).
    pub directory: String,
    /// `user@host:port` as shown to the user.
    pub host: String,
    /// What is being asked: `"password"` (SSH login) or `"sudo"` (a
    /// command that needs root on the remote host).
    pub prompt: String,
}

/// Installed by `bebok-core` to publish the prompt on the event bus.
pub type PromptSink = Arc<dyn Fn(SshPrompt) + Send + Sync>;

/// How to reach the host.
#[derive(Debug, Clone, Default)]
pub struct ConnectOpts {
    /// `host`, `user@host`, an IP or a `~/.ssh/config` alias.
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    /// Private key path (`-i`).
    pub identity: Option<String>,
    /// Refuse hosts whose key is not already known (`accept-new` otherwise).
    pub strict_host_key: bool,
    /// Remote directory to `cd` into once connected.
    pub cwd: Option<String>,
    /// Extra arguments for `ssh` itself, e.g. `-o PreferredAuthentications=password`.
    /// A value without a leading `-` is passed as `-o <value>`, so
    /// `"PreferredAuthentications=password"` works on its own.
    pub options: Vec<String>,
}

/// One executed remote command.
#[derive(Debug, Clone)]
pub struct RunResult {
    pub output: String,
    pub exit_code: i32,
    pub cwd: String,
    pub truncated: bool,
}

/// Wire shape of one live session (`GET /ssh`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshInfo {
    pub id: String,
    pub session_id: String,
    pub directory: String,
    pub host: String,
    pub label: String,
    pub cwd: String,
    /// Remote jail root: the session may not leave this directory.
    /// Always set after `connect` (canonical `pwd -P` of the entry dir).
    pub jail: String,
    pub started_at: i64,
    pub last_used: i64,
    pub commands: u32,
    /// `"running"` or `"dead"`.
    pub status: String,
}

/// The pipes of the persistent remote shell.
struct Conn {
    stdin: ChildStdin,
    stdout: ChildStdout,
}

/// One persistent SSH session.
pub struct SshSession {
    id: String,
    pub session_id: String,
    pub directory: String,
    /// A short, human-readable target (`user@host:port`).
    pub label: String,
    /// The `user@host` argument handed to `ssh`.
    target: String,
    /// Random per-session output marker.
    nonce: String,
    /// `sudo` password typed by the user for this session. Memory only, never
    /// persisted, dropped on close or as soon as the host rejects it.
    sudo_password: Mutex<Option<String>>,
    /// Whether the remote `sudo` works without a password (`NOPASSWD`), probed
    /// once at connect so the user is not asked for a password nobody needs.
    sudo_nopasswd: AtomicBool,
    cwd: Mutex<String>,
    /// Canonical remote jail root (`pwd -P` of the entry dir at connect).
    /// The shell is never allowed to leave it: `cwd` params outside it are
    /// rejected, `cd`/`pushd`/`popd` are wrapped to bounce back, and a
    /// post-command `$PWD` check drags an escaped shell back in.
    jail: Mutex<String>,
    started_at: i64,
    last_used: Mutex<i64>,
    commands: AtomicU32,
    /// Serializes commands: one remote shell, one command at a time.
    run_lock: AsyncMutex<()>,
    conn: AsyncMutex<Conn>,
    child: Mutex<Option<Child>>,
    /// `ssh`'s own diagnostics (banners, host-key warnings, errors).
    stderr: Arc<Mutex<String>>,
    dead: Mutex<Option<String>>,
}

impl SshSession {
    /// Registry id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Remote directory the shell is currently in (best effort).
    pub fn cwd(&self) -> String {
        self.cwd.lock().map(|c| c.clone()).unwrap_or_default()
    }

    fn mark_dead(&self, reason: &str) {
        if let Ok(mut dead) = self.dead.lock()
            && dead.is_none()
        {
            *dead = Some(reason.to_string());
        }
    }

    /// Why the session is unusable, if it is.
    pub fn dead_reason(&self) -> Option<String> {
        self.dead.lock().ok().and_then(|d| d.clone())
    }

    /// Remote jail root; empty when the session may use the whole server.
    pub fn jail(&self) -> String {
        self.jail.lock().map(|j| j.clone()).unwrap_or_default()
    }

    /// Everything `ssh` itself printed (never the remote command's output).
    pub fn stderr_tail(&self) -> String {
        self.stderr
            .lock()
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// Snapshot for `GET /ssh`.
    pub fn info(&self) -> SshInfo {
        let dead = self.dead_reason();
        SshInfo {
            id: self.id.clone(),
            session_id: self.session_id.clone(),
            directory: self.directory.clone(),
            host: self.target.clone(),
            label: self.label.clone(),
            cwd: self.cwd(),
            jail: self.jail(),
            started_at: self.started_at,
            last_used: self.last_used.lock().map(|t| *t).unwrap_or(self.started_at),
            commands: self.commands.load(Ordering::Relaxed),
            status: if dead.is_some() { "dead" } else { "running" }.to_string(),
        }
    }

    /// Run one command in the persistent shell.
    ///
    /// `cwd` changes the directory for this command *and* for every following
    /// one (it is a real `cd` in the remote shell) — pass `None` to stay where
    /// the shell already is.
    pub async fn run(
        &self,
        command: &str,
        cwd: Option<&str>,
        timeout_ms: u64,
        sudo: bool,
        abort: &CancellationToken,
    ) -> Result<RunResult, String> {
        if let Some(reason) = self.dead_reason() {
            return Err(format!(
                "SSH session {} is closed ({reason}); open a new one with action: \"connect\"",
                self.label
            ));
        }
        let timeout_ms = timeout_ms.clamp(1_000, MAX_RUN_TIMEOUT_MS);
        let _serial = self.run_lock.lock().await;
        // `sudo` reads its password from stdin in a non-interactive shell, so
        // the password is piped in together with the command. It is asked in
        // the UI once per session and reused for the following commands.
        let wants_root = sudo || needs_sudo(command);
        // A host with `NOPASSWD` needs no password: try it that way first and
        // only ask when the server actually demands one.
        let mut assume_nopasswd = wants_root && self.sudo_nopasswd.load(Ordering::Relaxed);

        loop {
            let password = if wants_root && !assume_nopasswd {
                Some(self.sudo_password().await?)
            } else {
                None
            };
            let frame = self.frame(command, cwd, password.as_deref());
            let (mut text, exit_code, cwd_now, truncated) =
                self.exec_frame(&frame, timeout_ms, abort).await?;

            if password.is_some() && exit_code != 0 && rejects_password(&text) {
                // Forget the password so the next sudo asks the user again.
                self.forget_sudo_password();
                return Err(format!(
                    "the remote host rejected the sudo password for {}; it was cleared, so the \
                     next sudo command asks again\n\n{text}",
                    self.label
                ));
            }
            if password.is_none() && assume_nopasswd && exit_code != 0 && asks_for_password(&text) {
                // `sudo -n` was assumed to work; it does not. Ask, then retry.
                assume_nopasswd = false;
                self.sudo_nopasswd.store(false, Ordering::Relaxed);
                continue;
            }

            let stderr = self.stderr_tail();
            if exit_code != 0 && !stderr.is_empty() {
                text.push_str(&stderr_suffix(&stderr));
            }
            return Ok(RunResult {
                output: text,
                exit_code,
                cwd: cwd_now,
                truncated,
            });
        }
    }

    /// Write one framed command and read its output back. Serialized by
    /// [`SshSession::run`] through `run_lock`.
    async fn exec_frame(
        &self,
        frame: &str,
        timeout_ms: u64,
        abort: &CancellationToken,
    ) -> Result<(String, i32, String, bool), String> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut conn = self.conn.lock().await;
        if let Err(e) = conn.stdin.write_all(frame.as_bytes()).await {
            let reason = format!("cannot write to the remote shell: {e}");
            self.mark_dead(&reason);
            return Err(reason);
        }
        if let Err(e) = conn.stdin.flush().await {
            let reason = format!("cannot write to the remote shell: {e}");
            self.mark_dead(&reason);
            return Err(reason);
        }

        let mut out: Vec<u8> = Vec::new();
        let mut truncated = false;
        let mut buf = [0u8; 8192];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "command timed out after {timeout_ms} ms (the session stays open; raise \
                     `timeout` or run long work in the background on the server)"
                ));
            }
            let read = tokio::select! {
                _ = abort.cancelled() => Err("command aborted by the user".to_string()),
                res = timeout(remaining, conn.stdout.read(&mut buf)) => match res {
                    Ok(Ok(n)) => Ok(n),
                    Ok(Err(e)) => Err(format!("cannot read from the remote shell: {e}")),
                    Err(_) => Err(format!("command timed out after {timeout_ms} ms")),
                },
            };
            let n = match read {
                Ok(n) => n,
                Err(e) => {
                    if e.starts_with("cannot read") {
                        self.mark_dead(&e);
                    }
                    return Err(e);
                }
            };
            if n == 0 {
                self.mark_dead("the SSH connection closed");
                return Err(format!(
                    "the SSH connection to {} closed{}",
                    self.label,
                    stderr_suffix(&self.stderr_tail())
                ));
            }
            out.extend_from_slice(&buf[..n]);
            if let Some((end, tail)) = find_marker(&out, &self.nonce) {
                let mut text = String::from_utf8_lossy(&out[..end]).to_string();
                if text.ends_with('\n') {
                    text.pop();
                }
                let (exit_code, cwd) = parse_tail(&tail, &self.cwd());
                if let Ok(mut current) = self.cwd.lock() {
                    *current = cwd.clone();
                }
                if let Ok(mut used) = self.last_used.lock() {
                    *used = now_ms();
                }
                self.commands.fetch_add(1, Ordering::Relaxed);
                return Ok((text, exit_code, cwd, truncated));
            }
            if out.len() > MAX_OUTPUT_BYTES * 2 {
                // Keep a window; the marker always arrives at the very end.
                let drop = out.len() - MAX_OUTPUT_BYTES;
                out.drain(..drop);
                truncated = true;
            }
        }
    }

    /// The `sudo` password for this session, asking the user the first time.
    async fn sudo_password(&self) -> Result<String, String> {
        if let Ok(cached) = self.sudo_password.lock()
            && let Some(password) = cached.as_ref()
        {
            return Ok(password.clone());
        }
        let password = SshRegistry::global()
            .ask_password(&self.directory, &self.session_id, &self.label, "sudo")
            .await?;
        if let Ok(mut cached) = self.sudo_password.lock() {
            *cached = Some(password.clone());
        }
        Ok(password)
    }

    /// Drop the remembered `sudo` password (the host refused it, or the
    /// session is closing) so the next sudo command asks the user again.
    fn forget_sudo_password(&self) {
        if let Ok(mut cached) = self.sudo_password.lock() {
            *cached = None;
        }
    }

    /// The shell snippet wrapping one command (see the module docs).
    fn frame(&self, command: &str, cwd: Option<&str>, sudo_password: Option<&str>) -> String {
        build_frame(&self.nonce, &self.jail(), command, cwd, sudo_password)
    }

    /// Close the connection (idempotent).
    pub fn kill(&self) {
        self.mark_dead("closed");
        if let Ok(mut guard) = self.child.lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.start_kill();
        }
    }
}

impl Drop for SshSession {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.child.lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.start_kill();
        }
    }
}

/// Process-wide session registry.
pub struct SshRegistry {
    sessions: Mutex<HashMap<String, Arc<SshSession>>>,
    /// Pending password prompts, keyed by request id.
    prompts: Mutex<HashMap<String, oneshot::Sender<Option<String>>>>,
    sink: OnceLock<PromptSink>,
}

static REGISTRY: OnceLock<SshRegistry> = OnceLock::new();

impl SshRegistry {
    /// The process-wide registry.
    pub fn global() -> &'static SshRegistry {
        REGISTRY.get_or_init(|| SshRegistry {
            sessions: Mutex::new(HashMap::new()),
            prompts: Mutex::new(HashMap::new()),
            sink: OnceLock::new(),
        })
    }

    /// Install the prompt sink (called once by `bebok-core`).
    pub fn set_prompt_sink(&self, sink: PromptSink) {
        let _ = self.sink.set(sink);
    }

    /// Live sessions, newest first.
    pub fn list(&self) -> Vec<SshInfo> {
        let sessions = self.sessions.lock().map(|s| s.clone()).unwrap_or_default();
        let mut out: Vec<SshInfo> = sessions.values().map(|s| s.info()).collect();
        out.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        out
    }

    /// One session by id.
    pub fn get(&self, id: &str) -> Option<Arc<SshSession>> {
        self.sessions.lock().ok()?.get(id).cloned()
    }

    /// Live sessions opened from one project directory.
    pub fn for_directory(&self, directory: &str) -> Vec<Arc<SshSession>> {
        let mut out: Vec<Arc<SshSession>> = self
            .sessions
            .lock()
            .map(|s| {
                s.values()
                    .filter(|s| s.directory == directory)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by_key(|s| s.started_at);
        out
    }

    /// Answer (or cancel, with `None`) a pending password prompt.
    pub fn answer_prompt(&self, request_id: &str, answer: Option<String>) -> bool {
        let sender = self
            .prompts
            .lock()
            .ok()
            .and_then(|mut p| p.remove(request_id));
        match sender {
            Some(sender) => sender.send(answer).is_ok(),
            None => false,
        }
    }

    /// Close a session and forget it.
    pub fn close(&self, id: &str) -> bool {
        let session = self.sessions.lock().ok().and_then(|mut s| s.remove(id));
        match session {
            Some(session) => {
                session.kill();
                true
            }
            None => false,
        }
    }

    /// Close every session (engine shutdown / tests).
    pub fn close_all(&self) {
        let sessions: Vec<Arc<SshSession>> = self
            .sessions
            .lock()
            .map(|mut s| s.drain().map(|(_, v)| v).collect())
            .unwrap_or_default();
        for session in sessions {
            session.kill();
        }
    }

    /// Resolve the target string (`user@host`) from [`ConnectOpts`].
    fn target_of(opts: &ConnectOpts) -> Result<String, String> {
        let host = opts.host.trim();
        if host.is_empty() {
            return Err("missing required parameter 'host'".to_string());
        }
        let user = opts
            .user
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty());
        let target = match user {
            Some(user) if !host.contains('@') => format!("{user}@{host}"),
            _ => host.to_string(),
        };
        let allowed = |c: char| {
            c.is_ascii_alphanumeric()
                || matches!(c, '@' | '.' | '_' | '-' | ':' | '[' | ']' | '%' | '~' | '/')
        };
        if !target.chars().all(allowed) {
            return Err(format!(
                "invalid host '{target}': use host, user@host, an IPv4/IPv6 address or a \
                 ~/.ssh/config alias"
            ));
        }
        Ok(target)
    }

    /// Option flags shared by the auth probe and the session itself.
    fn option_args(opts: &ConnectOpts) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "-o".into(),
            "ConnectTimeout=10".into(),
            "-o".into(),
            "LogLevel=ERROR".into(),
        ];
        if let Some(port) = opts.port {
            args.push("-p".into());
            args.push(port.to_string());
        }
        if let Some(identity) = opts
            .identity
            .as_deref()
            .map(str::trim)
            .filter(|i| !i.is_empty())
        {
            args.push("-i".into());
            args.push(expand_home(identity));
        }
        args.push("-o".into());
        args.push(if opts.strict_host_key {
            "StrictHostKeyChecking=yes".to_string()
        } else {
            "StrictHostKeyChecking=accept-new".to_string()
        });
        // Caller-supplied arguments win over the defaults above (`ssh` keeps
        // the last value of an `-o` pair), so `PreferredAuthentications=password`
        // can force password auth on a host that also accepts keys.
        for extra in &opts.options {
            let extra = extra.trim();
            if extra.is_empty() || extra.contains(['\n', '\r']) {
                continue;
            }
            if extra.starts_with('-') {
                args.push(extra.to_string());
            } else {
                args.push("-o".into());
                args.push(extra.to_string());
            }
        }
        args
    }

    /// Open a persistent session. Blocks while a password prompt is pending.
    pub async fn connect(
        &self,
        directory: &str,
        session_id: &str,
        opts: ConnectOpts,
    ) -> Result<Arc<SshSession>, String> {
        let target = Self::target_of(&opts)?;
        let options = Self::option_args(&opts);
        let label = match opts.port {
            Some(port) => format!("{target}:{port}"),
            None => target.clone(),
        };

        let live = self.sessions.lock().map(|s| s.len()).unwrap_or(0);
        if live >= MAX_SESSIONS {
            return Err(format!(
                "too many open SSH sessions ({live}); close one with action: \"close\" first"
            ));
        }
        // Reuse a healthy session for the same host instead of burning a
        // second connection (the agent may lose the id between turns).
        let existing = self.sessions.lock().ok().and_then(|s| {
            s.values()
                .find(|s| s.target == target && s.dead_reason().is_none())
                .cloned()
        });
        if let Some(existing) = existing {
            return Ok(existing);
        }

        // Phase 1: does the host accept key/agent auth? Fast and silent.
        // When the caller already forced password auth (`options`), skip the
        // probe: a key attempt could burn the host's `MaxAuthTries` budget.
        let password = if forces_password(&opts.options) {
            Some(
                self.ask_password(directory, session_id, &label, "password")
                    .await?,
            )
        } else {
            match probe_auth(&options, &target).await {
                Ok(()) => None,
                Err(reason) => {
                    if !wants_password(&reason) {
                        return Err(format!("cannot connect to {label}: {reason}"));
                    }
                    Some(
                        self.ask_password(directory, session_id, &label, "password")
                            .await?,
                    )
                }
            }
        };

        // Phase 2: the long-lived shell.
        let nonce = Uuid::new_v4().simple().to_string();
        let mut command = Command::new(ssh_bin());
        command
            .args(connection_args(&options, password.is_some(), &target))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut askpass = None;
        if let Some(password) = &password {
            let helper = Askpass::install(password)?;
            command
                .env("SSH_ASKPASS", helper.script.to_string_lossy().to_string())
                .env("SSH_ASKPASS_REQUIRE", "force")
                .env("DISPLAY", ":0");
            askpass = Some(helper);
        }

        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot start `{}`: {e}", ssh_bin()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or("cannot pipe the remote shell stdin")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("cannot pipe the remote shell stdout")?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or("cannot pipe the remote shell stderr")?;

        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&stderr_buf);
        tokio::spawn(async move {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                if let Ok(mut buf) = sink.lock() {
                    buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
                    if buf.len() > MAX_STDERR_BYTES {
                        let drop = buf.len() - MAX_STDERR_BYTES;
                        buf.drain(..drop);
                    }
                }
            }
        });

        let initial_cwd = opts
            .cwd
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string);
        let session = Arc::new(SshSession {
            id: Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            directory: directory.to_string(),
            label: label.clone(),
            target,
            nonce,
            sudo_password: Mutex::new(None),
            sudo_nopasswd: AtomicBool::new(false),
            cwd: Mutex::new(initial_cwd.clone().unwrap_or_default()),
            // Filled in from the canonical `pwd -P` right after the handshake:
            // a connect with `cwd` jails the session to that directory, a
            // connect without one leaves the whole server reachable.
            jail: Mutex::new(String::new()),
            started_at: now_ms(),
            last_used: Mutex::new(now_ms()),
            commands: AtomicU32::new(0),
            run_lock: AsyncMutex::new(()),
            conn: AsyncMutex::new(Conn { stdin, stdout }),
            child: Mutex::new(Some(child)),
            stderr: stderr_buf,
            dead: Mutex::new(None),
        });

        // Handshake: consume the banner/MOTD so the first real command's
        // output starts clean, and prove the shell answers.
        match session
            .run(
                ":",
                initial_cwd.as_deref(),
                20_000,
                false,
                &CancellationToken::new(),
            )
            .await
        {
            Ok(handshake) => {
                if initial_cwd.is_some()
                    && let Ok(mut jail) = session.jail.lock()
                {
                    *jail = handshake.cwd;
                }
            }
            Err(e) => {
                session.kill();
                return Err(format!(
                    "connected to {label} but the shell did not answer: {e}"
                ));
            }
        }
        // Does the remote sudo work without a password? Ask before the user is
        // ever bothered with a prompt for it. (`sudo -n` needs no password, so
        // this cannot itself trigger a prompt.)
        let nopasswd = matches!(
            session
                .run(
                    "sudo -n true >/dev/null 2>&1",
                    None,
                    15_000,
                    false,
                    &CancellationToken::new()
                )
                .await,
            Ok(result) if result.exit_code == 0
        );
        session.sudo_nopasswd.store(nopasswd, Ordering::Relaxed);

        // The password is used once, by the now-established connection.
        drop(askpass);

        self.sessions
            .lock()
            .map_err(|_| "SSH registry poisoned".to_string())?
            .insert(session.id.clone(), Arc::clone(&session));
        Ok(session)
    }

    /// Publish a password prompt and wait for the client's answer.
    async fn ask_password(
        &self,
        directory: &str,
        session_id: &str,
        label: &str,
        kind: &str,
    ) -> Result<String, String> {
        let Some(sink) = self.sink.get().cloned() else {
            return Err(if kind == "sudo" {
                format!(
                    "{label} needs a sudo password, but no client is connected to ask for it; \
                     run a non-sudo command or ask the user to configure NOPASSWD"
                )
            } else {
                format!(
                    "{label} asks for a password, but no client is connected to prompt for it; \
                     configure key-based auth (identity) instead"
                )
            });
        };
        let request_id = Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        self.prompts
            .lock()
            .map_err(|_| "SSH registry poisoned".to_string())?
            .insert(request_id.clone(), sender);
        sink(SshPrompt {
            request_id: request_id.clone(),
            session_id: session_id.to_string(),
            directory: directory.to_string(),
            host: label.to_string(),
            prompt: kind.to_string(),
        });
        let answer = match timeout(PROMPT_TIMEOUT, receiver).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => None,
            Err(_) => {
                if let Ok(mut prompts) = self.prompts.lock() {
                    prompts.remove(&request_id);
                }
                return Err(format!(
                    "timed out waiting for the {kind} password for {label} ({} s)",
                    PROMPT_TIMEOUT.as_secs()
                ));
            }
        };
        match answer {
            Some(answer) if !answer.is_empty() => Ok(answer),
            _ => Err(format!(
                "the {kind} password prompt for {label} was cancelled"
            )),
        }
    }
}

/// The `ssh` executable (override with `BEBOK_SSH`).
pub fn ssh_bin() -> String {
    std::env::var("BEBOK_SSH")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "ssh".to_string())
}

/// Whether the caller explicitly asked for a password login
/// (`-o PreferredAuthentications=password` or `-o BatchMode=no` …).
fn forces_password(options: &[String]) -> bool {
    options.iter().any(|option| {
        let option = option.to_ascii_lowercase().replace(' ', "");
        let value = option
            .strip_prefix("-opreferredauthentications=")
            .or_else(|| option.strip_prefix("preferredauthentications="));
        match value {
            Some(value) => value.split(',').any(|method| method == "password"),
            None => false,
        }
    })
}

/// The `ssh` argv for the long-lived session shell.
///
/// Every option must come *before* `-- <target>`: anything after the target
/// is treated as the remote command, so a stray `-o PubkeyAuthentication=no`
/// there makes the remote shell run `-o ...` and abort with
/// `bash: - : invalid option`.
fn connection_args(options: &[String], password: bool, target: &str) -> Vec<String> {
    let mut args = options.to_vec();
    if password {
        // The password is the only credential this connection may use: with
        // an agent holding many keys the host aborts the handshake with
        // "Too many authentication failures" before asking for one.
        args.extend([
            "-o".to_string(),
            "PreferredAuthentications=password,keyboard-interactive".to_string(),
            "-o".to_string(),
            "PubkeyAuthentication=no".to_string(),
            "-o".to_string(),
            "GSSAPIAuthentication=no".to_string(),
        ]);
    }
    args.extend([
        "-T".to_string(),
        "-o".to_string(),
        "ServerAliveInterval=30".to_string(),
        "-o".to_string(),
        "NumberOfPasswordPrompts=1".to_string(),
        "--".to_string(),
        target.to_string(),
    ]);
    args
}

/// Whether a failed key probe means "this host wants a password".
///
/// `permission denied` is the plain case; `too many authentication failures`
/// is the same thing seen through an agent that holds more keys than the
/// host's `MaxAuthTries`, so the handshake aborts before the password method
/// is ever offered.
fn wants_password(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("permission denied") || lower.contains("too many authentication failures")
}

/// Non-interactive auth check: `ssh -o BatchMode=yes host true`.
async fn probe_auth(options: &[String], target: &str) -> Result<(), String> {
    let mut command = Command::new(ssh_bin());
    command
        .args(options)
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("--")
        .arg(target)
        .arg("true")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = timeout(PROBE_TIMEOUT, command.output())
        .await
        .map_err(|_| {
            format!(
                "`ssh {target}` did not answer within {} s",
                PROBE_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| format!("cannot start `{}`: {e}", ssh_bin()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if stderr.is_empty() {
        format!(
            "`ssh {target}` failed with {}",
            output
                .status
                .code()
                .map(|c| format!("exit code {c}"))
                .unwrap_or_else(|| "a signal".to_string())
        )
    } else {
        stderr
    })
}

/// The temporary askpass helper that hands the password to `ssh` once.
struct Askpass {
    dir: PathBuf,
    script: PathBuf,
}

impl Askpass {
    fn install(password: &str) -> Result<Self, String> {
        let dir = std::env::temp_dir().join(format!("bebok-ssh-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create the askpass directory: {e}"))?;
        let secret = dir.join("answer");
        write_private(&secret, password)?;
        let shown = secret.display().to_string().replace('\'', "");

        #[cfg(windows)]
        let (script, body) = (dir.join("askpass.cmd"), format!("@type \"{shown}\"\r\n"));
        #[cfg(not(windows))]
        let (script, body) = (
            dir.join("askpass.sh"),
            format!("#!/bin/sh\ncat '{shown}'\necho\nexit 0\n"),
        );
        std::fs::write(&script, body)
            .map_err(|e| format!("cannot write the askpass helper: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("cannot make the askpass helper executable: {e}"))?;
        }
        Ok(Askpass { dir, script })
    }
}

impl Drop for Askpass {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Write a file only the current user can read.
fn write_private(path: &Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("cannot restrict {}: {e}", path.display()))?;
    }
    Ok(())
}

/// POSIX single-quote a value.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Expand a leading `~/` (the `ssh` CLI would, our own paths would not).
fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    {
        return PathBuf::from(home).join(rest).to_string_lossy().to_string();
    }
    path.to_string()
}

/// Whether a command line invokes `sudo` (so its password can be piped in).
///
/// Tokens are compared after trimming shell punctuation, so `$(sudo ...)`,
/// `&& sudo ...` and `(sudo ...)` all count.
fn needs_sudo(command: &str) -> bool {
    // A command "uses sudo" when a shell segment *starts* with it; `echo sudo`
    // or `chmod +x sudo` must not trigger a password prompt.
    command.split([';', '&', '|', '`', '\n']).any(|segment| {
        let mut tokens = segment
            .split_whitespace()
            .map(|token| {
                token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
            })
            .filter(|token| !token.is_empty());
        if tokens.next() != Some("sudo") {
            return false;
        }
        // `sudo -n` / `-v` / `-l` / `-K` never ask for a password.
        match tokens.next() {
            Some(next) if next.starts_with('-') => {
                let flag = next.trim_start_matches('-');
                !matches!(flag.chars().next(), Some('n' | 'v' | 'l' | 'K'))
            }
            _ => true,
        }
    })
}

/// Whether the output shows that `sudo` wanted a password it was not given.
fn asks_for_password(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("sudo: a password is required")
        || lower.contains("no tty present")
        || lower.contains("a terminal is required")
        || lower.contains("no askpass program")
}

/// Whether `sudo` refused the password it was given.
fn rejects_password(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    [
        "sorry, try again",
        "incorrect password",
        "no password was provided",
        "a password is required",
        "no tty present",
        "a terminal is required",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// The shell snippet wrapping one command, framed with the session marker.
///
/// `jail` (when not empty) is the directory the shell may not leave; with
/// `sudo_password` the command runs as root through `sudo -S` instead.
fn build_frame(
    nonce: &str,
    jail: &str,
    command: &str,
    cwd: Option<&str>,
    sudo_password: Option<&str>,
) -> String {
    let mut inner = String::new();
    if let Some(dir) = cwd.map(str::trim).filter(|d| !d.is_empty()) {
        inner.push_str(&format!(
            "cd -- {} 2>/dev/null || printf 'bebok: cannot cd to {}' >&2\n",
            shell_quote(dir),
            dir.replace('\'', "")
        ));
    }
    inner.push_str(command);

    let mut frame = String::new();
    match sudo_password {
        // `sudo -S` reads the password from stdin, `-p ''` kills the
        // prompt text; the password is a shell builtin argument, so it
        // never shows up in the remote process list.
        Some(password) => frame.push_str(&format!(
            "printf '%s\\n' {} | sudo -S -p '' sh -c {} 2>&1\n",
            shell_quote(password),
            shell_quote(&inner)
        )),
        None => {
            frame.push_str("{\n");
            frame.push_str(&inner);
            frame.push_str("\n} 2>&1\n");
        }
    }
    // Remember the exit status, then make sure the shell did not leave the
    // jail (`cd`, `pushd`, a sourced script, ...). With no jail the shell
    // goes wherever the user asks on the server.
    frame.push_str("__bebok_rc=$?\n");
    if !jail.is_empty() && jail != "/" {
        frame.push_str(&format!(
            "case \"$PWD/\" in {jail}/*) ;; *) cd -- {jail} 2>/dev/null || \
                 printf 'bebok: cannot return to %s\\n' {jail} >&2 ;; esac\n",
            jail = shell_quote(jail)
        ));
    }
    frame.push_str(&format!(
        "printf '\\n__BEBOK_EOF_{}__ %s %s\\n' \"$__bebok_rc\" \"$PWD\"\n",
        nonce
    ));
    frame
}

/// Locate the marker and return `(output_end, rest_of_marker_line)`.
fn find_marker(buf: &[u8], nonce: &str) -> Option<(usize, String)> {
    let needle = format!("__BEBOK_EOF_{nonce}__");
    let needle = needle.as_bytes();
    if buf.len() < needle.len() {
        return None;
    }
    let pos = buf.windows(needle.len()).position(|w| w == needle)?;
    let rest = &buf[pos + needle.len()..];
    let end = rest.iter().position(|b| *b == b'\n')?;
    Some((
        pos,
        String::from_utf8_lossy(&rest[..end]).trim().to_string(),
    ))
}

/// `"<exit code> <pwd>"` -> `(exit code, pwd)`; keeps `fallback` if unparsable.
fn parse_tail(tail: &str, fallback: &str) -> (i32, String) {
    let mut parts = tail.splitn(2, ' ');
    let code = parts
        .next()
        .and_then(|c| c.trim().parse::<i32>().ok())
        .unwrap_or(-1);
    let cwd = parts
        .next()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .unwrap_or(fallback)
        .to_string();
    (code, cwd)
}

/// `"\n\nssh: ..."` when there is something worth showing.
fn stderr_suffix(stderr: &str) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!("\n\nssh: {stderr}")
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_is_found_and_tail_parsed() {
        let nonce = "abc123";
        let mut buf = b"hello\nworld\n".to_vec();
        buf.extend_from_slice(format!("__BEBOK_EOF_{nonce}__ 0 /srv/app\n").as_bytes());
        let (end, tail) = find_marker(&buf, nonce).expect("marker");
        assert_eq!(String::from_utf8_lossy(&buf[..end]), "hello\nworld\n");
        assert_eq!(parse_tail(&tail, "/"), (0, "/srv/app".to_string()));
    }

    #[test]
    fn incomplete_marker_line_waits_for_more_bytes() {
        let nonce = "abc123";
        let buf = format!("x__BEBOK_EOF_{nonce}__ 0 /srv").into_bytes();
        assert!(find_marker(&buf, nonce).is_none());
    }

    #[test]
    fn quotes_and_resolves_targets() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        let opts = ConnectOpts {
            host: "example.com".into(),
            user: Some("deploy".into()),
            ..Default::default()
        };
        assert_eq!(SshRegistry::target_of(&opts).unwrap(), "deploy@example.com");
    }

    #[test]
    fn rejects_hosts_with_shell_metacharacters() {
        for host in ["example.com; rm -rf /", "$(id)", "host name", "host|cat"] {
            let opts = ConnectOpts {
                host: host.into(),
                ..Default::default()
            };
            assert!(SshRegistry::target_of(&opts).is_err(), "{host}");
        }
    }

    #[test]
    fn spots_password_requiring_sudo() {
        for command in [
            "sudo systemctl restart nginx",
            "cd /srv && sudo -u www-data make",
            "$(sudo whoami)",
            "sudo bash -c 'rm -rf /tmp/x'",
        ] {
            assert!(needs_sudo(command), "{command}");
        }
        for command in [
            "git status",
            "echo sudo",
            "sudo -n true",
            "sudo -l",
            "sudo -v",
            "mksudo",
        ] {
            assert!(!needs_sudo(command), "{command}");
        }
    }

    #[test]
    fn prompt_wire_shape_matches_the_client() {
        let prompt = SshPrompt {
            request_id: "req-1".into(),
            session_id: "sess-1".into(),
            directory: "/srv/app".into(),
            host: "rajner@host:2233".into(),
            prompt: "password".into(),
        };
        let json = serde_json::to_value(&prompt).unwrap();
        // The client answers `POST /ssh/prompt/{requestID}` and routes by
        // `sessionID`; camelCase (`requestId`) would silently drop the event.
        assert_eq!(json["requestID"], "req-1");
        assert_eq!(json["sessionID"], "sess-1");
        assert_eq!(json["directory"], "/srv/app");
        assert_eq!(json["prompt"], "password");
        assert!(json.get("requestId").is_none());
    }

    #[test]
    fn every_option_stays_before_the_target() {
        let options = vec!["PreferredAuthentications=password".to_string()];
        let args = connection_args(&options, true, "rajner@host");
        let separator = args.iter().position(|a| a == "--").expect("-- present");
        assert_eq!(
            separator,
            args.len() - 2,
            "only `-- <target>` may follow the options: {args:?}"
        );
        assert_eq!(args[separator + 1], "rajner@host");
        // Nothing that looks like a remote command may be left over.
        assert!(!args[..separator].contains(&"rajner@host".to_string()));
        let joined = args[..separator].join(" ");
        assert!(joined.contains("PubkeyAuthentication=no"), "{joined}");
        assert!(joined.contains("-T"), "{joined}");
    }

    #[test]
    fn detects_forced_password_auth() {
        for options in [
            vec!["PreferredAuthentications=password".to_string()],
            vec![
                "-o".to_string(),
                "PreferredAuthentications=password".to_string(),
            ],
            vec!["-oPreferredAuthentications=password,keyboard-interactive".to_string()],
        ] {
            assert!(forces_password(&options), "{options:?}");
        }
        assert!(!forces_password(&[
            "PreferredAuthentications=publickey".to_string()
        ]));
        assert!(!forces_password(&["StrictHostKeyChecking=no".to_string()]));
        assert!(!forces_password(&[]));
    }

    #[test]
    fn extra_options_reach_the_ssh_command_line() {
        let opts = ConnectOpts {
            host: "example.com".into(),
            port: Some(2233),
            options: vec![
                "PreferredAuthentications=password".into(),
                "-o".into(),
                "ProxyJump=bastion".into(),
                "  ".into(),
            ],
            ..Default::default()
        };
        let args = SshRegistry::option_args(&opts);
        let joined = args.join(" ");
        assert!(joined.contains("-p 2233"));
        assert!(
            joined.contains("-o PreferredAuthentications=password"),
            "{joined}"
        );
        assert!(joined.contains("-o ProxyJump=bastion"), "{joined}");
    }

    #[test]
    fn a_key_storm_still_means_password_auth() {
        assert!(wants_password(
            "rajner@host: Permission denied (publickey,password)."
        ));
        assert!(wants_password(
            "Received disconnect from 1.2.3.4 port 2233:2: Too many authentication failures"
        ));
        assert!(!wants_password(
            "Connection timed out during banner exchange"
        ));
        assert!(!wants_password("Host key verification failed."));
    }

    #[test]
    fn recognises_sudo_output() {
        assert!(rejects_password(
            "Sorry, try again.\nsudo: 1 incorrect password attempt"
        ));
        assert!(asks_for_password("sudo: a password is required"));
        assert!(asks_for_password(
            "sudo: no tty present and no askpass program specified"
        ));
        assert!(!rejects_password("make: *** [Makefile:2: all] Error 2"));
        assert!(!asks_for_password("ls: cannot access '/nope'"));
    }

    #[test]
    fn frames_commands_and_quotes_the_directory() {
        let frame = build_frame("n", "/srv/app", "id -u", Some("/srv/app/etc"), None);
        assert!(frame.starts_with("{\n"));
        assert!(frame.contains("cd -- '/srv/app/etc'"));
        assert!(frame.contains("__BEBOK_EOF_n__"));
        // The jail is enforced after the command ran.
        assert!(frame.contains("case \"$PWD/\" in '/srv/app'/*"));
    }

    #[test]
    fn frames_sudo_with_the_piped_password() {
        let frame = build_frame("n", "", "id -u", None, Some("s3cr3t"));
        assert!(frame.contains("printf '%s\\n' 's3cr3t' | sudo -S -p '' sh -c"));
        assert!(frame.contains("'/srv'") || frame.contains("id -u"));
        // The password never becomes its own command, and the marker still
        // reports sudo's exit code.
        assert!(frame.contains("__BEBOK_EOF_n__ %s %s"));
        assert!(!frame.contains("jail"));
    }

    #[test]
    fn frames_without_a_jail_do_not_clamp() {
        let frame = build_frame("n", "", "cd /tmp && pwd", None, None);
        assert!(!frame.contains("case \"$PWD/\""));
        assert!(frame.contains("__bebok_rc=$?"));
    }
}
