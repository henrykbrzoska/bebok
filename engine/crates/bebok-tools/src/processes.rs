//! Background process registry (F9-14).
//!
//! `bash { background: true }` spawns long-running commands (dev servers,
//! watchers) *detached* from the tool call: own process group, stdin closed,
//! stdout + stderr appended to `<root>/.bebok/run/<id>.log`. The registry is a
//! process-wide singleton ([`ProcessRegistry::global`]) so the HTTP layer can
//! list, tail and kill those processes independently of the turn that started
//! them, and so every one of them dies with the engine.
//!
//! Per process two tasks run on the tokio runtime:
//!
//! * a **waiter** that awaits the child's exit and records
//!   `status/exit_code/ended_at`, then publishes [`ProcessEvent::Exited`];
//! * a **tail** that polls the log file every 400 ms, reads the bytes appended
//!   since the last poll and publishes them as [`ProcessEvent::Output`] chunks
//!   (at most [`MAX_CHUNK_BYTES`] each), stopping ~1 s after the exit.
//!
//! Killing is always tree-wide: `taskkill /T /F` on Windows, `SIGTERM` to the
//! process group (then `SIGKILL` after [`TERM_GRACE`]) elsewhere.
//! [`ProcessRegistry::kill_all_blocking`] is the runtime-free variant for a
//! shutdown path that may run after the tokio runtime is gone.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Tail poll period.
const TAIL_POLL: Duration = Duration::from_millis(400);
/// Largest single `ProcessEvent::Output` chunk.
pub const MAX_CHUNK_BYTES: usize = 16 * 1024;
/// Extra tail polls after the exit was observed (~1 s at [`TAIL_POLL`]).
const TAIL_POLLS_AFTER_EXIT: u32 = 3;
/// How long a `SIGTERM`-ed process group gets before `SIGKILL` (Unix).
#[cfg(not(windows))]
const TERM_GRACE: Duration = Duration::from_secs(2);
/// How long `kill` waits for the waiter to record the exit.
const KILL_WAIT: Duration = Duration::from_secs(5);
/// Broadcast capacity for [`ProcessEvent`]s.
const EVENT_CAPACITY: usize = 1024;

#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// One background process as seen by the registry (wire shape of the
/// `/processes` endpoints, snake_case).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessInfo {
    /// Registry id (uuid v4).
    pub id: String,
    /// Session that started the process.
    pub session_id: String,
    /// The shell command line as given to `bash`.
    pub command: String,
    /// Working directory the command was started in (absolute).
    pub cwd: String,
    pub pid: u32,
    /// Unix epoch milliseconds.
    pub started_at: i64,
    /// `"running"` | `"exited"`.
    pub status: String,
    /// Exit code once exited (`None` while running or when killed by a signal).
    pub exit_code: Option<i32>,
    /// Unix epoch milliseconds once exited.
    pub ended_at: Option<i64>,
    /// Absolute path of the stdout+stderr log file.
    pub log_path: String,
}

impl ProcessInfo {
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
}

/// A chunk of new log output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    pub id: String,
    pub session_id: String,
    pub chunk: String,
    /// Unix epoch milliseconds when the chunk was read.
    pub at: i64,
}

/// The process exited (or was killed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessExited {
    pub id: String,
    pub session_id: String,
    pub code: Option<i32>,
}

/// Everything the registry publishes on its broadcast channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessEvent {
    Output(ProcessOutput),
    Exited(ProcessExited),
}

/// Process-wide registry of background processes.
pub struct ProcessRegistry {
    entries: Mutex<HashMap<String, ProcessInfo>>,
    events: broadcast::Sender<ProcessEvent>,
}

static REGISTRY: OnceLock<ProcessRegistry> = OnceLock::new();

/// Unix epoch milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Relative log path (forward slashes) for a process id, as shown to the model.
pub fn relative_log_path(id: &str) -> String {
    format!(".bebok/run/{id}.log")
}

impl ProcessRegistry {
    fn new() -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            entries: Mutex::new(HashMap::new()),
            events,
        }
    }

    /// The singleton.
    pub fn global() -> &'static ProcessRegistry {
        REGISTRY.get_or_init(ProcessRegistry::new)
    }

    /// Subscribe to output / exit events of every process.
    pub fn subscribe(&self) -> broadcast::Receiver<ProcessEvent> {
        self.events.subscribe()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, ProcessInfo>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Spawn `command` through the platform shell, detached, logging to
    /// `<root>/.bebok/run/<id>.log`. Returns immediately with the snapshot.
    pub async fn spawn_background(
        &self,
        session_id: &str,
        root: &Path,
        command: &str,
    ) -> io::Result<ProcessInfo> {
        let root = std::path::absolute(root)?;
        let id = uuid::Uuid::new_v4().to_string();
        let log_path = ensure_run_dir(&root)?.join(format!("{id}.log"));

        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let err = out.try_clone()?;

        let (shell, flag) = crate::bash::shell_command();
        let mut cmd = tokio::process::Command::new(&shell);
        cmd.arg(flag);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            if flag == "/C" {
                // Same quirk as the foreground `bash` tool: cmd.exe does not
                // follow the C runtime quoting rules, so pass the line raw.
                cmd.as_std_mut().raw_arg(command);
            } else {
                cmd.arg(command);
            }
            cmd.as_std_mut()
                .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::process::CommandExt;
            cmd.arg(command);
            cmd.as_std_mut().process_group(0);
        }
        cmd.current_dir(&root)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .kill_on_drop(false);

        let mut child = cmd.spawn()?;
        let pid = child.id().unwrap_or(0);
        let info = ProcessInfo {
            id: id.clone(),
            session_id: session_id.to_string(),
            command: command.to_string(),
            cwd: root.to_string_lossy().to_string(),
            pid,
            started_at: now_ms(),
            status: "running".to_string(),
            exit_code: None,
            ended_at: None,
            log_path: log_path.to_string_lossy().to_string(),
        };
        self.lock().insert(id.clone(), info.clone());

        // Waiter: record the exit, publish `Exited`.
        {
            let id = id.clone();
            let session_id = session_id.to_string();
            let events = self.events.clone();
            tokio::spawn(async move {
                let status = child.wait().await.ok();
                let code = status.and_then(|s| s.code());
                {
                    let registry = ProcessRegistry::global();
                    let mut entries = registry.lock();
                    if let Some(entry) = entries.get_mut(&id) {
                        entry.status = "exited".to_string();
                        entry.exit_code = code;
                        entry.ended_at = Some(now_ms());
                    }
                }
                let _ = events.send(ProcessEvent::Exited(ProcessExited {
                    id,
                    session_id,
                    code,
                }));
            });
        }

        // Tail: poll the log file, publish new bytes.
        {
            let id = id.clone();
            let session_id = session_id.to_string();
            let events = self.events.clone();
            tokio::spawn(tail_log(id, session_id, log_path, events));
        }

        Ok(info)
    }

    /// Processes started by one session (running and exited), oldest first.
    pub fn list(&self, session_id: &str) -> Vec<ProcessInfo> {
        let mut out: Vec<ProcessInfo> = self
            .lock()
            .values()
            .filter(|p| p.session_id == session_id)
            .cloned()
            .collect();
        out.sort_by_key(|p| p.started_at);
        out
    }

    /// Every process known to the registry, oldest first.
    pub fn list_all(&self) -> Vec<ProcessInfo> {
        let mut out: Vec<ProcessInfo> = self.lock().values().cloned().collect();
        out.sort_by_key(|p| p.started_at);
        out
    }

    pub fn get(&self, id: &str) -> Option<ProcessInfo> {
        self.lock().get(id).cloned()
    }

    /// Find a *running* process by OS pid.
    pub fn find_by_pid(&self, pid: u32) -> Option<ProcessInfo> {
        self.lock()
            .values()
            .find(|p| p.pid == pid && p.is_running())
            .cloned()
    }

    /// Kill the whole process tree of `id` and wait (bounded) for the waiter
    /// to record the exit. Killing an already-exited process is a no-op that
    /// returns its final snapshot.
    pub async fn kill(&self, id: &str) -> io::Result<ProcessInfo> {
        let info = self.get(id).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("unknown process {id}"))
        })?;
        if !info.is_running() {
            return Ok(info);
        }
        kill_tree(info.pid).await;
        // Bounded wait for the waiter task.
        let deadline = tokio::time::Instant::now() + KILL_WAIT;
        loop {
            if let Some(current) = self.get(id)
                && !current.is_running()
            {
                return Ok(current);
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(self.get(id).unwrap_or(info))
    }

    /// Kill every running process of a session. Returns the killed snapshots.
    pub async fn kill_session(&self, session_id: &str) -> Vec<ProcessInfo> {
        let mut out = Vec::new();
        for p in self.list(session_id) {
            if p.is_running()
                && let Ok(info) = self.kill(&p.id).await
            {
                out.push(info);
            }
        }
        out
    }

    /// Kill every running process (engine shutdown).
    pub async fn kill_all(&self) -> Vec<ProcessInfo> {
        let mut out = Vec::new();
        for p in self.list_all() {
            if p.is_running()
                && let Ok(info) = self.kill(&p.id).await
            {
                out.push(info);
            }
        }
        out
    }

    /// Runtime-free `kill_all` for a shutdown path where tokio may already be
    /// gone (`std::process::Command` only, no waiting on the waiter tasks).
    /// Returns the number of processes signalled.
    pub fn kill_all_blocking(&self) -> usize {
        let running: Vec<ProcessInfo> = self
            .lock()
            .values()
            .filter(|p| p.is_running())
            .cloned()
            .collect();
        for p in &running {
            kill_tree_blocking(p.pid);
        }
        running.len()
    }

    /// Read the log of `id`: the whole file, or the last `tail_bytes` bytes.
    pub fn read_log(&self, id: &str, tail_bytes: Option<usize>) -> io::Result<String> {
        self.read_log_with_size(id, tail_bytes)
            .map(|(text, _)| text)
    }

    /// [`ProcessRegistry::read_log`] plus the total log size in bytes.
    pub fn read_log_with_size(
        &self,
        id: &str,
        tail_bytes: Option<usize>,
    ) -> io::Result<(String, u64)> {
        let info = self.get(id).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("unknown process {id}"))
        })?;
        read_tail(Path::new(&info.log_path), tail_bytes)
    }
}

/// `<root>/.bebok/run` with a `.gitignore` that hides every log.
fn ensure_run_dir(root: &Path) -> io::Result<PathBuf> {
    let dir = root.join(".bebok").join("run");
    std::fs::create_dir_all(&dir)?;
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, "*\n")?;
    }
    Ok(dir)
}

/// Read a file wholly or its last `tail_bytes` bytes (lossy UTF-8).
fn read_tail(path: &Path, tail_bytes: Option<usize>) -> io::Result<(String, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let mut buf = Vec::new();
    match tail_bytes {
        Some(n) if (n as u64) < size => {
            file.seek(SeekFrom::Start(size - n as u64))?;
            file.read_to_end(&mut buf)?;
        }
        _ => {
            file.read_to_end(&mut buf)?;
        }
    }
    Ok((String::from_utf8_lossy(&buf).to_string(), size))
}

/// Length of the longest prefix of `bytes` that does not end inside an
/// incomplete UTF-8 sequence (so a chunk boundary never splits a character).
fn utf8_boundary(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(e) => match e.error_len() {
            // Invalid bytes in the middle: emit everything (lossy) rather
            // than stall on garbage.
            Some(_) => bytes.len(),
            // Incomplete sequence at the end: keep it for the next poll.
            None => e.valid_up_to(),
        },
    }
}

/// Poll the log file and publish new bytes until ~1 s after the exit.
async fn tail_log(
    id: String,
    session_id: String,
    log_path: PathBuf,
    events: broadcast::Sender<ProcessEvent>,
) {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut offset: u64 = 0;
    let mut pending: Vec<u8> = Vec::new();
    let mut polls_after_exit: u32 = 0;
    loop {
        tokio::time::sleep(TAIL_POLL).await;

        // Read everything appended since the last poll.
        if let Ok(mut file) = tokio::fs::File::open(&log_path).await {
            let size = file.metadata().await.map(|m| m.len()).unwrap_or(0);
            if size < offset {
                // Truncated/rotated by someone else: start over.
                offset = 0;
            }
            if size > offset && file.seek(std::io::SeekFrom::Start(offset)).await.is_ok() {
                let mut buf = Vec::with_capacity((size - offset) as usize);
                if let Ok(n) = file.read_to_end(&mut buf).await {
                    offset += n as u64;
                    pending.extend_from_slice(&buf);
                }
            }
        }

        // Publish in chunks of at most MAX_CHUNK_BYTES, never splitting a
        // UTF-8 character.
        while !pending.is_empty() {
            let take = pending.len().min(MAX_CHUNK_BYTES);
            let take = utf8_boundary(&pending[..take]);
            if take == 0 {
                break;
            }
            let chunk = String::from_utf8_lossy(&pending[..take]).to_string();
            pending.drain(..take);
            let _ = events.send(ProcessEvent::Output(ProcessOutput {
                id: id.clone(),
                session_id: session_id.clone(),
                chunk,
                at: now_ms(),
            }));
        }

        let exited = ProcessRegistry::global()
            .get(&id)
            .is_none_or(|p| !p.is_running());
        if exited {
            polls_after_exit += 1;
            if polls_after_exit > TAIL_POLLS_AFTER_EXIT {
                break;
            }
        }
    }
}

/// Kill the process tree rooted at `pid` (async).
async fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = tokio::process::Command::new("taskkill");
        cmd.args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd.as_std_mut().creation_flags(CREATE_NO_WINDOW);
        let _ = cmd.status().await;
    }
    #[cfg(not(windows))]
    {
        let _ = signal_group(pid, "-TERM").status().await;
        tokio::time::sleep(TERM_GRACE).await;
        if pid_alive(pid) {
            let _ = signal_group(pid, "-KILL").status().await;
        }
    }
}

#[cfg(not(windows))]
fn signal_group(pid: u32, signal: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("kill");
    cmd.arg(signal)
        .arg(format!("-{pid}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

#[cfg(not(windows))]
fn pid_alive(pid: u32) -> bool {
    // `kill -0` succeeds while the process exists (zombies included; the
    // waiter reaps ours, so a lingering zombie only costs one extra SIGKILL).
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Kill the process tree rooted at `pid` without a tokio runtime.
fn kill_tree_blocking(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(format!("-{pid}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        std::thread::sleep(TERM_GRACE);
        if pid_alive(pid) {
            let _ = std::process::Command::new("kill")
                .arg("-KILL")
                .arg(format!("-{pid}"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("bebok-proc-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn short_command() -> &'static str {
        "echo hi"
    }

    fn long_command() -> &'static str {
        if cfg!(windows) {
            "ping -n 30 127.0.0.1"
        } else {
            "sleep 30"
        }
    }

    async fn wait_exited(id: &str) -> ProcessInfo {
        let registry = ProcessRegistry::global();
        for _ in 0..200 {
            if let Some(p) = registry.get(id)
                && !p.is_running()
            {
                return p;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("process {id} did not exit in time");
    }

    #[tokio::test]
    async fn short_command_exits_and_logs() {
        let root = temp_root("short");
        let registry = ProcessRegistry::global();
        let mut rx = registry.subscribe();
        let info = registry
            .spawn_background("sess-short", &root, short_command())
            .await
            .unwrap();
        assert_eq!(info.status, "running");
        assert!(info.pid > 0);
        assert_eq!(info.session_id, "sess-short");
        assert!(Path::new(&info.log_path).is_absolute());
        assert!(root.join(".bebok/run/.gitignore").exists());
        assert_eq!(
            std::fs::read_to_string(root.join(".bebok/run/.gitignore")).unwrap(),
            "*\n"
        );

        let exited = wait_exited(&info.id).await;
        assert_eq!(exited.exit_code, Some(0));
        assert!(exited.ended_at.is_some());

        // The tail keeps polling ~1 s after the exit, so the echo shows up.
        let mut saw_output = false;
        let mut saw_exit = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while (!saw_output || !saw_exit) && tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
                Ok(Ok(ProcessEvent::Output(o))) if o.id == info.id => {
                    assert_eq!(o.session_id, "sess-short");
                    if o.chunk.contains("hi") {
                        saw_output = true;
                    }
                }
                Ok(Ok(ProcessEvent::Exited(e))) if e.id == info.id => {
                    assert_eq!(e.code, Some(0));
                    saw_exit = true;
                }
                Ok(Ok(_)) => {}
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
                _ => break,
            }
        }
        assert!(saw_output, "no output event with the echo");
        assert!(saw_exit, "no exited event");

        let log = registry.read_log(&info.id, None).unwrap();
        assert!(log.contains("hi"), "{log}");
        assert!(registry.list("sess-short").iter().any(|p| p.id == info.id));
        assert!(registry.list_all().iter().any(|p| p.id == info.id));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn long_command_is_killed() {
        let root = temp_root("kill");
        let registry = ProcessRegistry::global();
        let info = registry
            .spawn_background("sess-kill", &root, long_command())
            .await
            .unwrap();
        assert!(registry.find_by_pid(info.pid).is_some());
        let killed = registry.kill(&info.id).await.unwrap();
        assert_eq!(killed.status, "exited", "{killed:?}");
        assert!(killed.ended_at.is_some());
        // Idempotent.
        let again = registry.kill(&info.id).await.unwrap();
        assert_eq!(again, killed);
        assert!(registry.find_by_pid(info.pid).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn kill_session_only_touches_that_session() {
        let root = temp_root("session");
        let registry = ProcessRegistry::global();
        let a = registry
            .spawn_background("sess-a", &root, long_command())
            .await
            .unwrap();
        let b = registry
            .spawn_background("sess-b", &root, long_command())
            .await
            .unwrap();
        let killed = registry.kill_session("sess-a").await;
        assert_eq!(killed.len(), 1);
        assert_eq!(killed[0].id, a.id);
        assert!(!registry.get(&a.id).unwrap().is_running());
        assert!(registry.get(&b.id).unwrap().is_running());
        registry.kill(&b.id).await.unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn read_log_tail_returns_last_bytes() {
        let root = temp_root("tail");
        let registry = ProcessRegistry::global();
        let info = registry
            .spawn_background("sess-tail", &root, short_command())
            .await
            .unwrap();
        wait_exited(&info.id).await;
        // Write a known payload behind the process (it has exited).
        std::fs::write(&info.log_path, "0123456789abcdef").unwrap();
        let (text, size) = registry.read_log_with_size(&info.id, Some(6)).unwrap();
        assert_eq!(text, "abcdef");
        assert_eq!(size, 16);
        assert_eq!(
            registry.read_log(&info.id, Some(100)).unwrap(),
            "0123456789abcdef"
        );
        assert_eq!(
            registry.read_log(&info.id, None).unwrap(),
            "0123456789abcdef"
        );
        assert!(registry.read_log("nope", None).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn utf8_boundary_keeps_incomplete_tail() {
        let s = "héllo";
        let bytes = s.as_bytes();
        // Cut inside the two-byte `é`.
        assert_eq!(utf8_boundary(&bytes[..2]), 1);
        assert_eq!(utf8_boundary(bytes), bytes.len());
        assert_eq!(utf8_boundary(b"\xff\xfe"), 2);
    }

    #[test]
    fn relative_log_path_is_forward_slashed() {
        assert_eq!(relative_log_path("abc"), ".bebok/run/abc.log");
    }
}
