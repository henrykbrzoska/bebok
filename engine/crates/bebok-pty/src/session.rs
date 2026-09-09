//! A single PTY session: spawn state, scrollback, live fan-out, resize, kill.
//!
//! See the crate docs for the concurrency model. The important property: the
//! reader thread only ever uses non-blocking `try_send`, so a slow client can
//! never stall the PTY (it is dropped and a gap is implied until reattach).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use bytes::Bytes;
use portable_pty::{Child, MasterPty, PtySize};
use tokio::sync::mpsc;
use tokio::sync::watch;

use crate::scrollback::Scrollback;
use crate::PtyError;

#[cfg(windows)]
use crate::win::JobObject;

/// Bound on the per-client live-output channel (messages).
const CLIENT_BUFFER: usize = 256;
/// Bytes the reader emits per fan-out item.
const READ_CHUNK: usize = 8192;

/// Scrollback + connected clients, guarded by a single lock so that client
/// registration and the scrollback snapshot are atomic (no replay gap).
struct PtyInner {
    scrollback: Scrollback,
    clients: HashMap<u64, mpsc::Sender<Bytes>>,
}

/// One spawned terminal session (engine-owned).
pub struct PtySession {
    id: String,
    pub cwd: Option<String>,
    pub command: String,
    pub title: Option<String>,
    input_tx: mpsc::Sender<Vec<u8>>,
    inner: Mutex<PtyInner>,
    /// Master end kept alive for `resize` (its reader/writer were cloned out).
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Option<Box<dyn Child + Send + Sync>>>,
    /// Job Object the child is assigned to (Windows tree-kill).
    #[cfg(windows)]
    job: Mutex<Option<JobObject>>,
    exited: AtomicBool,
    exit_code: Mutex<Option<u32>>,
    exit_tx: watch::Sender<Option<i32>>,
    next_client_id: AtomicU64,
}

impl PtySession {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn is_exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    pub fn exit_code(&self) -> Option<u32> {
        *self.exit_code.lock().unwrap()
    }

    /// Child process id (diagnostics / tests).
    pub fn process_id(&self) -> Option<u32> {
        self.child.lock().unwrap().as_ref().and_then(|c| c.process_id())
    }

    /// Subscribe to the exit signal (`None` while running, then the exit code).
    pub fn exit_rx(&self) -> watch::Receiver<Option<i32>> {
        self.exit_tx.subscribe()
    }

    /// Register a live-output consumer. Returns a handle that replays the
    /// scrollback first (captured atomically with registration) and then
    /// streams live bytes. Dropping the handle unregisters it.
    pub fn connect(self: &Arc<Self>) -> PtyClient {
        let (tx, rx) = mpsc::channel(CLIENT_BUFFER);
        let id = self.next_client_id.fetch_add(1, Ordering::Relaxed);
        let snapshot = {
            let mut inner = self.inner.lock().unwrap();
            inner.clients.insert(id, tx);
            inner.scrollback.bytes().to_vec()
        };
        PtyClient {
            session: Arc::clone(self),
            id,
            rx,
            scrollback: snapshot,
        }
    }

    fn unsubscribe(&self, id: u64) {
        self.inner.lock().unwrap().clients.remove(&id);
    }

    /// Send raw bytes to the child (client input).
    pub async fn send_input(&self, data: Vec<u8>) -> Result<(), PtyError> {
        if data.is_empty() {
            return Ok(());
        }
        self.input_tx
            .send(data)
            .await
            .map_err(|_| PtyError::Other("pty input channel closed".to_string()))
    }

    /// Resize the terminal (SIGWINCH on Unix, ConPTY repaint on Windows).
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), PtyError> {
        let master = self.master.lock().unwrap();
        master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// Kill the whole process tree (best effort). The child handle is kept so
    /// the reader thread can reap it (and record the exit code) once the master
    /// end reaches EOF.
    ///
    /// - Windows: the Job Object (primary, kills the whole tree) then the
    ///   `taskkill /T` fallback.
    /// - Unix: `kill(-pgid, SIGKILL)` (the child is a session leader).
    pub fn kill(&self) -> Result<(), PtyError> {
        // Windows: terminate the Job Object first - this kills every process in
        // the tree regardless of depth, which `taskkill` / direct `kill` cannot
        // guarantee from a ConPTY session.
        #[cfg(windows)]
        if let Some(job) = self.job.lock().unwrap().take() {
            crate::win::terminate(job);
        }

        let mut guard = self.child.lock().unwrap();
        if let Some(child) = guard.as_mut() {
            if let Some(pid) = child.process_id() {
                kill_process_tree(pid);
            }
            let _ = child.kill();
        }
        Ok(())
    }

    /// Called by the reader thread once the master end hits EOF (child exit).
    fn mark_exited(&self) {
        let code = {
            // Take the child out so we don't hold the lock during the blocking
            // `wait()` (which reaps the process and yields its exit code).
            let mut child = self.child.lock().unwrap().take();
            match child.as_mut() {
                Some(child) => match child.wait() {
                    Ok(status) => Some(status.exit_code()),
                    Err(_) => None,
                },
                None => None,
            }
        };
        self.exited.store(true, Ordering::Release);
        *self.exit_code.lock().unwrap() = code;
        let _ = self.exit_tx.send(code.map(|c| c as i32));
        // Wake/drop any live consumers: their receivers close.
        self.inner.lock().unwrap().clients.clear();
    }
}

/// A connected client of a PTY: replays scrollback then streams live output.
pub struct PtyClient {
    session: Arc<PtySession>,
    id: u64,
    /// Live output from the moment of registration.
    pub rx: mpsc::Receiver<Bytes>,
    /// Scrollback snapshot (already played out of band by the caller).
    pub scrollback: Vec<u8>,
}

impl PtyClient {
    /// Wait for the next live output chunk (or `None` when the PTY exits).
    pub async fn recv(&mut self) -> Option<Bytes> {
        self.rx.recv().await
    }

    pub async fn send_input(&self, data: Vec<u8>) -> Result<(), PtyError> {
        self.session.send_input(data).await
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), PtyError> {
        self.session.resize(rows, cols)
    }

    pub fn is_exited(&self) -> bool {
        self.session.is_exited()
    }
}

impl Drop for PtyClient {
    fn drop(&mut self) {
        self.session.unsubscribe(self.id);
    }
}

/// Spawn the reader and writer threads for a freshly opened PTY pair.
pub(crate) fn spawn_threads(
    session: Arc<PtySession>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    input_rx: mpsc::Receiver<Vec<u8>>,
) {
    {
        let session = Arc::clone(&session);
        std::thread::Builder::new()
            .name(format!("pty-reader-{}", session.id))
            .spawn(move || reader_loop(reader, session))
            .expect("spawn pty reader thread");
    }
    {
        std::thread::Builder::new()
            .name(format!("pty-writer-{}", session.id))
            .spawn(move || writer_loop(writer, input_rx))
            .expect("spawn pty writer thread");
    }
}

fn reader_loop(mut reader: Box<dyn Read + Send>, session: Arc<PtySession>) {
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break, // EOF: the child (and its descendants) closed the pty
            Ok(n) => n,
            Err(_) => break,
        };
        let chunk = Bytes::copy_from_slice(&buf[..n]);
        let mut dropped = Vec::new();
        {
            let mut inner = session.inner.lock().unwrap();
            inner.scrollback.append(&chunk);
            for (id, tx) in inner.clients.iter() {
                // Never block: a lagging consumer is dropped (drop-with-gap).
                if tx.try_send(chunk.clone()).is_err() {
                    dropped.push(*id);
                }
            }
            for id in dropped {
                inner.clients.remove(&id);
                tracing::warn!("pty {} client {id} dropped (slow consumer)", session.id);
            }
        }
    }
    session.mark_exited();
}

fn writer_loop(mut writer: Box<dyn Write + Send>, mut rx: mpsc::Receiver<Vec<u8>>) {
    while let Some(data) = rx.blocking_recv() {
        if writer.write_all(&data).is_err() {
            break;
        }
        let _ = writer.flush();
    }
}

/// Kill an entire process tree.
///
/// - Unix: the PTY child is a session leader (portable-pty runs `setsid`), so
///   its process-group id equals its pid; `kill(-pgid, SIGKILL)` terminates the
///   whole group.
/// - Windows: `taskkill /T /F` walks the child tree. This is the fallback when
///   the Job Object could not be assigned; the primary tree-kill is the Job
///   Object terminated in [`PtySession::kill`].
fn kill_process_tree(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }

    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .status();
    }
}

/// Build the session and spin up its I/O threads.
pub(crate) fn spawn(
    id: String,
    cwd: Option<String>,
    command: String,
    title: Option<String>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    scrollback: Scrollback,
) -> Arc<PtySession> {
    let reader = master
        .try_clone_reader()
        .expect("clone pty reader");
    let writer = master.take_writer().expect("take pty writer");

    // Windows: assign the child to a Job Object (KILL_ON_JOB_CLOSE) as soon as
    // possible so a later `kill()` terminates the whole process tree.
    #[cfg(windows)]
    let job = child.as_raw_handle().and_then(crate::win::assign_process);

    let (input_tx, input_rx) = mpsc::channel(1024);
    let (exit_tx, _exit_rx) = watch::channel(None);
    let session = Arc::new(PtySession {
        id,
        cwd,
        command,
        title,
        input_tx,
        inner: Mutex::new(PtyInner {
            scrollback,
            clients: HashMap::new(),
        }),
        master: Mutex::new(master),
        child: Mutex::new(Some(child)),
        #[cfg(windows)]
        job: Mutex::new(job),
        exited: AtomicBool::new(false),
        exit_code: Mutex::new(None),
        exit_tx,
        next_client_id: AtomicU64::new(1),
    });

    spawn_threads(Arc::clone(&session), reader, writer, input_rx);
    session
}
