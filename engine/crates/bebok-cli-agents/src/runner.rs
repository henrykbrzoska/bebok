//! Process runner: spawn, line-stream, timeout, tree kill.
//!
//! A CLI provider is a child process, and a child process is the easiest way
//! for an agent to leak resources: a cancelled turn must never leave `claude`
//! running against the project. Three rules follow from that and shape this
//! module:
//!
//! 1. **The child belongs to the event stream.** Dropping the stream (an
//!    aborted turn, a dropped provider) must terminate the whole process tree,
//!    so the [`Child`] is stored inside the stream's own state — see
//!    [`ProcessGuard`]. `kill_on_drop(true)` alone is not enough on Windows
//!    because the `claude.cmd` shim spawns a `node` grandchild that survives
//!    the `cmd.exe` wrapper.
//! 2. **stderr is drained concurrently.** A CLI that fills its stderr pipe
//!    blocks forever, and the turn then looks like a hang instead of an error.
//!    The last [`STDERR_TAIL_LINES`] lines are kept for the failure message.
//! 3. **One [`CliStreamState`] for the whole stream.** Backends remember the
//!    session key there; a fresh state per line would silently drop `--resume`.
//!
//! The runner deliberately does not depend on `bebok-tools` (that crate owns
//! Bebok's own tool registry); the process-tree helpers it needs are duplicated
//! here, mirroring `bebok-tools/src/processes.rs`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use futures::stream::{BoxStream, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, watch};

use crate::backend::{
    CliBackend, CliError, CliEvent, CliProbe, CliRequest, CliStreamState, TurnScratch,
};
use crate::spec::CliSpec;

/// How many stderr lines are kept for the error message.
const STDERR_TAIL_LINES: usize = 200;
/// Events buffered between the reader task and the consumer. Bounded on
/// purpose: a stalled consumer must back-pressure the CLI, not the heap.
const EVENT_CHANNEL: usize = 64;
/// Longest argument fragment written to the debug log (prompts are huge).
const LOG_ARG_CHARS: usize = 120;
/// How long the reader waits for the process to be reaped after stdout EOF.
const REAP_GRACE: Duration = Duration::from_secs(5);
/// How long `<binary> --help` may take. On timeout the turn still runs, with
/// every probe-gated flag off.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Probe results per resolved binary path, for the life of the process. A
/// CLI upgrade is picked up on the next engine start.
static PROBE_CACHE: OnceLock<StdMutex<HashMap<PathBuf, CliProbe>>> = OnceLock::new();

/// A running CLI: the parsed event stream plus the exit handle.
///
/// The child process is owned *by the stream*: dropping `events` (an aborted
/// turn, a dropped provider stream) kills the whole process tree. A CLI that
/// outlives its turn is a zombie `claude` holding the project open.
pub struct CliRun {
    pub events: BoxStream<'static, Result<CliEvent, CliError>>,
    pub exit: tokio::task::JoinHandle<Result<Option<i32>, CliError>>,
}

/// State shared by the event stream, the exit supervisor and the watchdog.
struct ProcessShared {
    pid: u32,
    /// The child handle. It lives here — and this struct is owned by the
    /// stream's state — so the process cannot outlive the stream.
    child: Mutex<Option<Child>>,
    /// Files the CLI reads at startup (system prompt, ...). Removed when the
    /// stream is dropped, which happens only after the CLI has exited.
    _scratch: TurnScratch,
    /// The child has been waited on, so its pid may already be reused.
    finished: AtomicBool,
    /// The hard timeout elapsed and the watchdog killed the tree.
    timed_out: AtomicBool,
    /// The stream was dropped and we killed the tree ourselves.
    killed: AtomicBool,
    /// The backend reported usage, so a non-zero exit is not an error.
    saw_usage: AtomicBool,
}

/// Kill-on-drop handle held by the event stream.
struct ProcessGuard {
    shared: Arc<ProcessShared>,
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if self.shared.finished.load(Ordering::SeqCst) {
            return;
        }
        self.shared.killed.store(true, Ordering::SeqCst);
        tracing::debug!(
            pid = self.shared.pid,
            "CLI event stream dropped; killing the process tree"
        );
        kill_tree_blocking(self.shared.pid);
    }
}

/// Spawn the backend's command in `req.cwd` and stream parsed events.
///
/// Fails before spawning when the binary cannot be resolved; afterwards the
/// child's own failures arrive as `Err` items on the stream (stream errors,
/// timeouts) or on the `exit` handle (non-zero status).
pub async fn spawn_cli(
    backend: Arc<dyn CliBackend>,
    spec: CliSpec,
    req: CliRequest,
) -> Result<CliRun, CliError> {
    let resolved = resolve_program(backend.as_ref(), &spec)?;
    let probe = probe_binary(&resolved).await;
    let mut scratch = TurnScratch::default();
    let (requested, args) = backend.build_command(&spec, &req, &probe, &mut scratch)?;
    let stdin_payload = backend.stdin_payload(&req, &probe);
    let program = effective_program(&requested, &resolved);
    let (program, args) = prepare_command(&program, args);
    let timeout = spec.timeout();

    let mut cmd = Command::new(&program);
    cmd.args(&args)
        .current_dir(&req.cwd)
        .stdin(if stdin_payload.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    apply_process_flags(&mut cmd);

    tracing::debug!(
        program = %program,
        argc = args.len(),
        cwd = %req.cwd.display(),
        stdin_bytes = stdin_payload.as_ref().map_or(0, Vec::len),
        "spawning CLI backend"
    );
    for (index, arg) in args.iter().enumerate() {
        tracing::debug!(index, arg = %truncate(arg, LOG_ARG_CHARS), "CLI argument");
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| CliError::Spawn(format!("{program}: {e}")))?;
    let pid = child.id().unwrap_or(0);

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CliError::Spawn("CLI stdout pipe unavailable".into()))?;
    let stderr = child.stderr.take();

    if let (Some(payload), Some(mut stdin)) = (stdin_payload, child.stdin.take()) {
        // Written off the critical path: a CLI that exits early (or never
        // reads) must not block the reader. A failed write is only a debug
        // note here — the CLI's own exit status and stderr explain the real
        // failure, and those are what the turn reports.
        tokio::spawn(async move {
            if let Err(err) = stdin.write_all(&payload).await {
                tracing::debug!(error = %err, "CLI stdin write failed");
            }
            // Dropping the pipe closes stdin. EOF is what lets `claude -p`
            // start; an open stdin makes it wait for more input.
            drop(stdin);
        });
    }

    let shared = Arc::new(ProcessShared {
        pid,
        child: Mutex::new(Some(child)),
        _scratch: scratch,
        finished: AtomicBool::new(false),
        timed_out: AtomicBool::new(false),
        killed: AtomicBool::new(false),
        saw_usage: AtomicBool::new(false),
    });

    // stderr: drained concurrently into a bounded tail. Never joined on the
    // critical path — a leaked grandchild could hold the pipe open forever.
    let stderr_tail = Arc::new(Mutex::new(LineRing::new(STDERR_TAIL_LINES)));
    let stderr_task = stderr.map(|stderr| {
        let tail = Arc::clone(&stderr_tail);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tail.lock().await.push(line);
            }
        })
    });

    let (tx, rx) = mpsc::channel::<Result<CliEvent, CliError>>(EVENT_CHANNEL);
    let (done_tx, done_rx) = watch::channel(false);

    // stdout: one line at a time, parsed with a single stream state.
    let reader_backend = Arc::clone(&backend);
    let reader_shared = Arc::clone(&shared);
    let mut reader_done = done_rx.clone();
    let reader_tx = tx.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        let mut state = CliStreamState::default();
        'read: loop {
            if reader_shared.timed_out.load(Ordering::SeqCst) {
                break;
            }
            let line = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(err) => {
                    let _ = reader_tx.send(Err(CliError::Stream(err.to_string()))).await;
                    break;
                }
            };
            let primary = match reader_backend.parse_line(&line, &mut state) {
                Ok(primary) => primary,
                Err(err) => {
                    let _ = reader_tx.send(Err(err)).await;
                    break;
                }
            };
            // One line may carry several events: the primary one first, then
            // whatever the backend queued, in order.
            let events = primary.into_iter().chain(state.pending.drain(..));
            for event in events {
                if matches!(event, CliEvent::Usage(_)) {
                    reader_shared.saw_usage.store(true, Ordering::SeqCst);
                }
                if reader_tx.send(Ok(event)).await.is_err() {
                    // The consumer went away; stop reading.
                    break 'read;
                }
            }
            // A failed run is reported last, after the usage it still billed.
            if let Some(message) = state.failure.take() {
                let _ = reader_tx.send(Err(CliError::RunFailed(message))).await;
                break;
            }
        }
        // Normally stdout EOF precedes the exit by milliseconds; waiting for
        // the reap here keeps the guard from killing an already exiting
        // process. Bounded, because a stuck grandchild can hold stdout open.
        let _ = tokio::time::timeout(REAP_GRACE, wait_done(&mut reader_done)).await;
        drop(reader_tx);
    });

    // Hard timeout watchdog: SIGTERM the tree, tell the stream, then keep the
    // sender only until the message is delivered.
    let timeout_shared = Arc::clone(&shared);
    let timeout_tx = tx.clone();
    let mut timeout_done = done_rx;
    tokio::spawn(async move {
        let expired = tokio::select! {
            _ = tokio::time::sleep(timeout) => true,
            _ = wait_done(&mut timeout_done) => false,
        };
        if !expired {
            return;
        }
        timeout_shared.timed_out.store(true, Ordering::SeqCst);
        tracing::debug!(?timeout, pid, "CLI backend timed out; killing the tree");
        kill_tree(pid).await;
        let _ = timeout_tx.send(Err(CliError::Timeout(timeout))).await;
    });

    // Exit supervisor: reaps the child, records the status and publishes the
    // stderr tail. This is the `exit` handle the provider awaits.
    let exit_shared = Arc::clone(&shared);
    let exit_tail = Arc::clone(&stderr_tail);
    let exit = tokio::spawn(async move {
        let status = {
            let mut slot = exit_shared.child.lock().await;
            match slot.as_mut() {
                Some(child) => child.wait().await,
                None => {
                    return Err(CliError::Stream(
                        "CLI child handle was already consumed".to_string(),
                    ));
                }
            }
        };
        let status = status
            .map_err(|e| CliError::Stream(format!("failed to wait for the CLI process: {e}")));
        exit_shared.finished.store(true, Ordering::SeqCst);
        let _ = done_tx.send(true);

        // The pipe closes on exit; this just makes the tail complete.
        if let Some(task) = stderr_task {
            let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
        }

        let status = status?;
        let code = status.code();
        tracing::debug!(pid, code, "CLI backend exited");

        if exit_shared.timed_out.load(Ordering::SeqCst) {
            return Err(CliError::Timeout(timeout));
        }
        if exit_shared.killed.load(Ordering::SeqCst) {
            // We terminated the tree on purpose (dropped stream); not a failure.
            return Ok(None);
        }
        if code == Some(0) || exit_shared.saw_usage.load(Ordering::SeqCst) {
            return Ok(code);
        }
        let stderr = exit_tail.lock().await.tail();
        Err(CliError::Exit {
            status: code.unwrap_or(-1),
            stderr,
        })
    });

    // The stream owns the guard, which owns the process.
    let guard = ProcessGuard {
        shared: Arc::clone(&shared),
    };
    let events = futures::stream::unfold((rx, guard), |(mut rx, guard)| async move {
        rx.recv().await.map(|item| (item, (rx, guard)))
    })
    .boxed();

    Ok(CliRun { events, exit })
}

/// Resolve the executable: explicit config, then the backend's env override,
/// then the first binary name found on `PATH`.
fn resolve_program(backend: &dyn CliBackend, spec: &CliSpec) -> Result<PathBuf, CliError> {
    if let Some(command) = spec
        .command
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        return Ok(PathBuf::from(command));
    }
    let env_var = backend.env_override();
    if let Ok(value) = std::env::var(env_var) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    let names = backend.binary_names();
    for name in names {
        if let Some(path) = crate::discovery::which_on_path(name) {
            return Ok(path);
        }
    }
    Err(CliError::BinaryMissing {
        names: names.join(", "),
        env_var: env_var.to_string(),
    })
}

/// Ask the resolved binary what it supports: `<binary> --help`, cached per path.
///
/// Feature detection by help text is deliberately crude. It can only say a
/// flag is *advertised*; a backend therefore gates each flag it is unsure of
/// and keeps a fallback that works without it. A failed or timed-out probe
/// yields an empty [`CliProbe`], which turns every gated flag off.
async fn probe_binary(resolved: &Path) -> CliProbe {
    let cache = PROBE_CACHE.get_or_init(|| StdMutex::new(HashMap::new()));
    if let Ok(map) = cache.lock()
        && let Some(hit) = map.get(resolved)
    {
        return hit.clone();
    }

    let (program, args) = prepare_command(&resolved.to_string_lossy(), vec!["--help".to_string()]);
    let mut cmd = Command::new(&program);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    apply_process_flags(&mut cmd);

    match tokio::time::timeout(PROBE_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            let probe = CliProbe::new(String::from_utf8_lossy(&output.stdout).into_owned());
            if let Ok(mut map) = cache.lock() {
                map.insert(resolved.to_path_buf(), probe.clone());
            }
            probe
        }
        _ => {
            // Not cached: a transient failure should get another try next turn.
            tracing::debug!(
                program = %resolved.display(),
                "CLI --help probe failed; probe-gated flags stay off"
            );
            CliProbe::default()
        }
    }
}

/// `CliBackend::build_command` may return a bare program name (`claude`) that
/// is only a placeholder for "the binary you resolved"; the runner substitutes
/// the resolved path so the PATH lookup and the `.cmd` shim handling apply.
/// A backend that deliberately returns a concrete program keeps it.
fn effective_program(requested: &str, resolved: &Path) -> String {
    let requested = requested.trim();
    if is_bare_program_name(requested) {
        resolved.to_string_lossy().into_owned()
    } else {
        requested.to_string()
    }
}

/// True for a plain executable name (`claude`, `claude.exe`) as opposed to a
/// path (`/usr/bin/claude`, `C:\bin\claude.cmd`, `./claude`).
fn is_bare_program_name(program: &str) -> bool {
    !program.is_empty() && !program.contains(['/', '\\']) && !program.contains(':')
}

/// On Windows, npm installs Claude Code as a `claude.cmd` shim, which
/// `CreateProcess` cannot launch directly — it has to go through `cmd.exe`.
fn needs_cmd_wrapper(program: &str) -> bool {
    let lower = program.to_ascii_lowercase();
    lower.ends_with(".cmd") || lower.ends_with(".bat")
}

/// Rewrite `(claude.cmd, [args])` into `(cmd, ["/C", "claude.cmd", args...])`.
fn wrap_cmd(program: &str, args: Vec<String>) -> (String, Vec<String>) {
    let mut wrapped = Vec::with_capacity(args.len() + 2);
    wrapped.push("/C".to_string());
    wrapped.push(program.to_string());
    wrapped.extend(args);
    ("cmd".to_string(), wrapped)
}

/// Apply the platform-specific executable handling before spawning.
fn prepare_command(program: &str, args: Vec<String>) -> (String, Vec<String>) {
    if cfg!(windows) && needs_cmd_wrapper(program) {
        wrap_cmd(program, args)
    } else {
        (program.to_string(), args)
    }
}

/// Spawn in its own process group (Unix) or with creation flags (Windows) so
/// the whole tree can be signalled as one unit.
fn apply_process_flags(cmd: &mut Command) {
    platform::spawn_flags(cmd);
}

/// Wait until the exit supervisor publishes `done` (or drops the sender).
async fn wait_done(rx: &mut watch::Receiver<bool>) {
    loop {
        let done = *rx.borrow_and_update();
        if done {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// Kill the process tree rooted at `pid` (async, for the timeout watchdog).
async fn kill_tree(pid: u32) {
    platform::kill_tree(pid).await;
}

/// Kill the process tree rooted at `pid` without a tokio runtime (drop path).
fn kill_tree_blocking(pid: u32) {
    platform::kill_tree_blocking(pid);
}

/// Platform-specific process control.
///
/// The two platforms share no item here on purpose. A `use` or `const` declared
/// once and referenced only from inside a `#[cfg(...)]` branch is dead code for
/// the other target, and `-D warnings` turns that into a build failure on the
/// platform nobody built. Keeping each branch a self-contained module makes
/// that failure mode structurally impossible; `scripts/check-cfg-hygiene.mjs`
/// enforces the rule for the rest of the tree.
#[cfg(windows)]
mod platform {
    use std::process::Stdio;

    /// Own process group for the wrapper, and no console window flashing up.
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn creation_flags() -> u32 {
        CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
    }

    /// `taskkill /T` is the only reliable tree kill: `claude.cmd` is a `cmd.exe`
    /// shim around a `node` grandchild, and terminating the shim orphans it.
    pub(super) async fn kill_tree(pid: u32) {
        use std::os::windows::process::CommandExt;
        let mut cmd = tokio::process::Command::new("taskkill");
        cmd.args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd.as_std_mut().creation_flags(CREATE_NO_WINDOW);
        let _ = cmd.status().await;
    }

    pub(super) fn kill_tree_blocking(pid: u32) {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }

    pub(super) fn spawn_flags(cmd: &mut tokio::process::Command) {
        use std::os::windows::process::CommandExt;
        cmd.as_std_mut().creation_flags(creation_flags());
    }
}

#[cfg(not(any(windows, unix)))]
compile_error!(
    "bebok-cli-agents needs a process-control impl for this platform: add one next to the windows/unix `platform` modules in runner.rs"
);

#[cfg(unix)]
mod platform {
    use std::time::{Duration, Instant};

    /// How long a SIGTERM'd process tree may linger before it gets SIGKILL.
    const TERM_GRACE: Duration = Duration::from_secs(3);
    /// How often the liveness of a dying tree is re-checked.
    const POLL: Duration = Duration::from_millis(50);

    /// Signal the whole process group led by `pid` (the child was spawned with
    /// `process_group(0)`, so its pgid is its pid). A direct syscall rather than
    /// the `kill` binary: procps parses `kill -TERM -1234` as a signal number and
    /// silently signals nothing. Never signals pgid 0 or 1.
    fn signal_group(pid: u32, signal: libc::c_int) {
        if pid <= 1 {
            return;
        }
        // SAFETY: plain kill(2) on a pgid we spawned; the kernel validates it.
        unsafe {
            libc::kill(-(pid as libc::pid_t), signal);
        }
    }

    fn pid_alive(pid: u32) -> bool {
        if pid <= 1 {
            return false;
        }
        // Signal 0 delivers nothing and succeeds while the process exists.
        // SAFETY: kill(2) with signal 0 has no side effects.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    pub(super) async fn kill_tree(pid: u32) {
        signal_group(pid, libc::SIGTERM);
        let deadline = Instant::now() + TERM_GRACE;
        while Instant::now() < deadline {
            if !pid_alive(pid) {
                return;
            }
            tokio::time::sleep(POLL).await;
        }
        signal_group(pid, libc::SIGKILL);
    }

    pub(super) fn kill_tree_blocking(pid: u32) {
        signal_group(pid, libc::SIGTERM);
        let deadline = Instant::now() + TERM_GRACE;
        while Instant::now() < deadline {
            if !pid_alive(pid) {
                return;
            }
            std::thread::sleep(POLL);
        }
        signal_group(pid, libc::SIGKILL);
    }

    pub(super) fn spawn_flags(cmd: &mut tokio::process::Command) {
        use std::os::unix::process::CommandExt;
        // The child becomes the leader of a new group; `kill(-pid)` then
        // reaches every descendant, including the `node` grandchild.
        cmd.as_std_mut().process_group(0);
    }
}

/// Shorten a string for the debug log without splitting a UTF-8 character.
fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let clipped: String = text.chars().take(max_chars).collect();
    format!("{clipped}…")
}

/// Bounded tail of a line stream: keeps the last `cap` lines, oldest dropped.
#[derive(Debug)]
struct LineRing {
    lines: VecDeque<String>,
    cap: usize,
}

impl LineRing {
    fn new(cap: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(cap.min(256)),
            cap,
        }
    }

    fn push(&mut self, line: String) {
        if self.cap == 0 {
            return;
        }
        while self.lines.len() >= self.cap {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    fn tail(&self) -> String {
        self.lines
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_shim_detection_is_case_insensitive() {
        assert!(needs_cmd_wrapper("claude.cmd"));
        assert!(needs_cmd_wrapper("CLAUDE.CMD"));
        assert!(needs_cmd_wrapper(
            r"C:\Users\me\AppData\Roaming\npm\claude.bat"
        ));
        assert!(!needs_cmd_wrapper("claude"));
        assert!(!needs_cmd_wrapper("claude.exe"));
        assert!(!needs_cmd_wrapper("/usr/bin/claude"));
    }

    #[test]
    fn wrap_cmd_inserts_interpreter_and_keeps_args() {
        let (program, args) = wrap_cmd(
            r"C:\npm\claude.cmd",
            vec!["--print".to_string(), "hello world".to_string()],
        );
        assert_eq!(program, "cmd");
        assert_eq!(
            args,
            vec![
                "/C".to_string(),
                r"C:\npm\claude.cmd".to_string(),
                "--print".to_string(),
                "hello world".to_string(),
            ]
        );
    }

    #[test]
    fn wrap_cmd_without_user_args() {
        let (program, args) = wrap_cmd("claude.bat", Vec::new());
        assert_eq!(program, "cmd");
        assert_eq!(args, vec!["/C".to_string(), "claude.bat".to_string()]);
    }

    #[test]
    fn prepare_command_leaves_plain_programs_alone_off_windows() {
        let (program, args) = prepare_command("claude", vec!["-p".to_string()]);
        if cfg!(windows) {
            assert_eq!(program, "claude");
        } else {
            // A `.cmd` shim is only wrapped on Windows.
            assert_eq!(program, "claude");
        }
        assert_eq!(args, vec!["-p".to_string()]);
    }

    #[test]
    fn bare_program_names_are_recognised() {
        assert!(is_bare_program_name("claude"));
        assert!(is_bare_program_name("claude.exe"));
        assert!(!is_bare_program_name(""));
        assert!(!is_bare_program_name("/usr/bin/claude"));
        assert!(!is_bare_program_name(r"C:\bin\claude.cmd"));
        assert!(!is_bare_program_name("./claude"));
    }

    #[test]
    fn resolved_path_replaces_a_bare_program_name() {
        let resolved = Path::new("/usr/local/bin/claude");
        assert_eq!(
            effective_program("claude", resolved),
            "/usr/local/bin/claude"
        );
        // A concrete program from the backend wins.
        assert_eq!(effective_program("/opt/claude", resolved), "/opt/claude");
        assert_eq!(effective_program("cmd", Path::new("cmd")), "cmd");
    }

    #[test]
    fn line_ring_keeps_only_the_tail() {
        let mut ring = LineRing::new(3);
        for line in ["1", "2", "3", "4", "5"] {
            ring.push(line.to_string());
        }
        assert_eq!(ring.tail(), "3\n4\n5");
    }

    #[test]
    fn line_ring_is_empty_before_input_and_under_capacity() {
        let mut ring = LineRing::new(3);
        assert_eq!(ring.tail(), "");
        ring.push("only".to_string());
        assert_eq!(ring.tail(), "only");
    }

    #[test]
    fn line_ring_with_zero_capacity_discards_everything() {
        let mut ring = LineRing::new(0);
        ring.push("dropped".to_string());
        assert_eq!(ring.tail(), "");
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("claude", 10), "claude");
        assert_eq!(truncate("zażółć", 2), "za…");
        assert_eq!(truncate("", 0), "");
    }

    #[tokio::test]
    async fn missing_binary_probes_to_an_empty_help_text() {
        let probe = probe_binary(Path::new("/nonexistent/bebok-probe-target")).await;
        assert!(!probe.mentions("--input-format"));
    }

    /// A backend that runs a shell snippet. Each stdout line becomes a text
    /// event, except `fail`, which exercises the pending/failure path.
    #[cfg(unix)]
    struct ShellBackend {
        script: &'static str,
        stdin: Option<Vec<u8>>,
    }

    #[cfg(unix)]
    impl CliBackend for ShellBackend {
        fn id(&self) -> &'static str {
            "shell-test"
        }
        fn label(&self) -> &'static str {
            "Shell test"
        }
        fn capabilities(&self) -> crate::backend::CliCapabilities {
            crate::backend::CliCapabilities::default()
        }
        fn default_models(&self) -> Vec<String> {
            Vec::new()
        }
        fn binary_names(&self) -> &'static [&'static str] {
            &["sh"]
        }
        fn env_override(&self) -> &'static str {
            "BEBOK_SHELL_TEST_BIN"
        }
        fn build_command(
            &self,
            _spec: &CliSpec,
            _req: &CliRequest,
            _probe: &CliProbe,
            _scratch: &mut TurnScratch,
        ) -> Result<(String, Vec<String>), CliError> {
            Ok(("sh".into(), vec!["-c".into(), self.script.into()]))
        }
        fn stdin_payload(&self, _req: &CliRequest, _probe: &CliProbe) -> Option<Vec<u8>> {
            self.stdin.clone()
        }
        fn parse_line(
            &self,
            line: &str,
            st: &mut CliStreamState,
        ) -> Result<Option<CliEvent>, CliError> {
            if line == "fail" {
                st.pending.push_back(CliEvent::Thinking("note".into()));
                st.failure = Some("boom".into());
                return Ok(Some(CliEvent::Text("before".into())));
            }
            Ok(Some(CliEvent::Text(line.to_string())))
        }
    }

    #[cfg(unix)]
    fn shell_spec() -> CliSpec {
        CliSpec {
            backend: "shell-test".into(),
            command: Some("/bin/sh".into()),
            args: Vec::new(),
            mode: crate::backend::CliMode::Agent,
            allow_write: false,
            timeout_secs: 30,
            max_turns: None,
        }
    }

    #[cfg(unix)]
    fn shell_request() -> CliRequest {
        CliRequest {
            prompt: String::new(),
            blocks: Vec::new(),
            system: None,
            model: None,
            cwd: std::env::temp_dir(),
            resume_key: None,
            write: false,
            max_turns: None,
            timeout: Duration::from_secs(30),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdin_payload_reaches_the_child_and_then_eof() {
        let backend = Arc::new(ShellBackend {
            script: "cat",
            stdin: Some(b"one\ntwo\n".to_vec()),
        });
        let run = spawn_cli(backend, shell_spec(), shell_request())
            .await
            .expect("spawns");
        // `cat` only exits once stdin is closed, so this also proves EOF.
        let events = tokio::time::timeout(Duration::from_secs(20), run.events.collect::<Vec<_>>())
            .await
            .expect("cat exits on EOF");
        let texts: Vec<String> = events
            .into_iter()
            .map(|event| match event {
                Ok(CliEvent::Text(text)) => text,
                other => panic!("unexpected event {other:?}"),
            })
            .collect();
        assert_eq!(texts, vec!["one".to_string(), "two".to_string()]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_stdin_write_does_not_hide_the_exit_status() {
        // The CLI exits without reading a payload larger than the pipe buffer,
        // so the write fails with EPIPE. The exit status must still win.
        let backend = Arc::new(ShellBackend {
            script: "exit 3",
            stdin: Some(vec![b'x'; 1 << 20]),
        });
        let run = spawn_cli(backend, shell_spec(), shell_request())
            .await
            .expect("spawns");
        let events: Vec<_> = run.events.collect().await;
        assert!(events.is_empty(), "{events:?}");
        match run.exit.await.expect("supervisor joined") {
            Err(CliError::Exit { status: 3, .. }) => {}
            other => panic!("expected exit status 3, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn queued_events_come_before_the_run_failure() {
        let backend = Arc::new(ShellBackend {
            script: "printf 'fail\\n'",
            stdin: None,
        });
        let run = spawn_cli(backend, shell_spec(), shell_request())
            .await
            .expect("spawns");
        let events: Vec<_> = run.events.collect().await;
        assert_eq!(events.len(), 3, "{events:?}");
        assert!(matches!(&events[0], Ok(CliEvent::Text(t)) if t == "before"));
        assert!(matches!(&events[1], Ok(CliEvent::Thinking(t)) if t == "note"));
        assert!(matches!(&events[2], Err(CliError::RunFailed(m)) if m == "boom"));
    }
}
