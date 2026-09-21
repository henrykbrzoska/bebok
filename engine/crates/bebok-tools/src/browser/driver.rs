//! Per-session browser lifecycle (WP-BROWSER / F6-17, WP-BROWSER2 / F7-6).
//!
//! One Chromium process + one page per Bebok session, launched lazily by the
//! first `browser_*` call and owned by the process-wide [`BrowserDriver`].
//! Two sessions never share a page (each gets its own browser process with
//! its own `user-data-dir`), so parallel sessions cannot fight over
//! navigation state.
//!
//! Display modes (F7-6, see [`super::settings`]): the instance's `browser`
//! config section decides whether the browser is launched **headed** (a
//! visible 1280x800 OS window the user can watch) or **headless** (viewer /
//! drawer modes). CDP control is identical in both. The driver also owns the
//! live frame stream for the viewer window ([`super::frames`]): while a tool
//! call is active on a session (tracked by [`ActivityGuard`]) a streamer task
//! captures the page at most 2 fps and hands frames to the installed
//! [`FrameSink`].
//!
//! Teardown paths (all funnel into [`BrowserDriver::close`]):
//! * explicit — `bebok-core` calls [`close_session`] when a turn is aborted or
//!   a session is deleted;
//! * idle — a reaper task closes browsers unused for [`IDLE_TIMEOUT`];
//! * process exit — chromiumoxide spawns the child with `kill_on_drop`, so
//!   dropping the driver (or the whole process) takes the browser down.
//!
//! Talks CDP through the `chromiumoxide` crate (tokio runtime, no bundled
//! browser download: the executable comes from [`super::discovery`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::{
    GetNavigationHistoryParams, NavigateToHistoryEntryParams,
};
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::page::Page;
use futures::StreamExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::console::{self, ConsoleBuffer, SharedConsole};
use super::discovery;
use super::frames::{self, Frame, FrameSink};
use super::page::{AnyPage, LocalPage};
use super::remote::{RemoteClient, RemotePage};
use super::settings::{BrowserDisplay, BrowserSettings};

/// Default viewport (CSS pixels). Wide enough for desktop layouts, small
/// enough that a screenshot stays well under the model's image size limits.
pub const VIEWPORT_WIDTH: u32 = 1280;
pub const VIEWPORT_HEIGHT: u32 = 800;
/// A browser unused for this long is closed by the reaper.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How often the reaper checks for idle browsers.
const REAP_INTERVAL: Duration = Duration::from_secs(60);
/// Upper bound on launching the browser process + CDP handshake.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Upper bound on a single CDP request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// One live browser bound to a session.
struct SessionBrowser {
    browser: Browser,
    page: Page,
    handler: JoinHandle<()>,
    last_used: Instant,
    user_data_dir: PathBuf,
    /// Instance directory the session belongs to (frames carry it).
    directory: String,
    /// Launched with a visible OS window.
    headed: bool,
    /// Frame counter (monotonic per browser).
    seq: u64,
    /// Console / exception / log capture (WP-AUTOVERIFY / F8-1).
    console: SharedConsole,
    /// The capture task feeding `console`; aborted on shutdown.
    console_task: Option<JoinHandle<()>>,
}

/// Static facts about a session's live browser (for the HTTP API).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BrowserInfo {
    pub headed: bool,
    pub directory: String,
    pub url: String,
    pub title: String,
}

/// User-driven history navigation (viewer window toolbar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryAction {
    Back,
    Forward,
    Reload,
}

/// Streamer bookkeeping for one session.
#[derive(Default)]
struct Activity {
    /// Number of tool calls currently running on the session.
    active: usize,
    /// The frame streamer task, while one runs.
    streamer: Option<JoinHandle<()>>,
}

/// Process-wide registry of per-session browsers.
#[derive(Default)]
pub struct BrowserDriver {
    sessions: Mutex<HashMap<String, SessionBrowser>>,
    reaper: OnceLock<JoinHandle<()>>,
    /// `browser` config section per instance root (set by `bebok-core` on
    /// instance load/reload; consulted at launch time).
    settings: std::sync::RwLock<HashMap<PathBuf, BrowserSettings>>,
    /// Where streamed frames go (installed once by `bebok-core`).
    frame_sink: std::sync::RwLock<Option<FrameSink>>,
    /// Last time a viewer window asked for a frame, per session.
    viewers: std::sync::Mutex<HashMap<String, Instant>>,
    /// Active tool calls + streamer per session.
    activity: std::sync::Mutex<HashMap<String, Activity>>,
}

static DRIVER: OnceLock<Arc<BrowserDriver>> = OnceLock::new();

/// Keeps a session's frame streamer alive while a tool call runs. Dropping it
/// ends the activity; the streamer sends one final frame and stops.
pub struct ActivityGuard {
    driver: Weak<BrowserDriver>,
    session_id: String,
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        if let Some(driver) = self.driver.upgrade() {
            driver.end_activity(&self.session_id);
        }
    }
}

impl BrowserDriver {
    /// The process-wide driver shared by every `browser_*` tool.
    pub fn global() -> Arc<BrowserDriver> {
        DRIVER
            .get_or_init(|| Arc::new(BrowserDriver::default()))
            .clone()
    }

    // ── settings / sink ─────────────────────────────────────────────────

    /// Remember the `browser` config section for an instance root. Applies
    /// to the next launch for sessions of that root (a running browser is
    /// not restarted).
    pub fn configure(&self, root: &Path, settings: BrowserSettings) {
        self.settings
            .write()
            .unwrap()
            .insert(normalize_root(root), settings);
    }

    /// The settings for `root` (defaults when never configured).
    pub fn settings_for(&self, root: &Path) -> BrowserSettings {
        self.settings
            .read()
            .unwrap()
            .get(&normalize_root(root))
            .cloned()
            .unwrap_or_default()
    }

    /// Install the frame sink (replaces a previous one).
    pub fn set_frame_sink(&self, sink: FrameSink) {
        *self.frame_sink.write().unwrap() = Some(sink);
    }

    fn sink(&self) -> Option<FrameSink> {
        self.frame_sink.read().unwrap().clone()
    }

    // ── pages ───────────────────────────────────────────────────────────

    /// The page bound to `session_id`, launching the browser on first use
    /// with the settings of `root` (the session's instance directory).
    ///
    /// Automatically selects between local Chromium and remote extension based
    /// on the `browser.remote.enabled` config setting.
    pub async fn page(self: &Arc<Self>, session_id: &str, root: &Path) -> Result<AnyPage, String> {
        self.ensure_reaper();
        let settings = self.settings_for(root);

        // Decide whether to use remote extension. Remote mode never touches
        // the local Chromium: the page lives in the user's own browser and
        // is driven through the Companion extension over HTTP.
        if settings.remote.enabled {
            // Get extension port from settings
            let port = settings.remote.extension_port.ok_or_else(|| {
                "remote browser enabled but no extension port configured; set browser.remote.extensionPort".to_string()
            })?;

            let client = RemoteClient::new(port, session_id)?;
            Ok(Arc::new(RemotePage::new(client)))
        } else {
            // Local browser: existing behavior
            let mut sessions = self.sessions.lock().await;
            if let Some(sb) = sessions.get_mut(session_id) {
                let dead = matches!(sb.browser.try_wait(), Ok(Some(_)));
                if !dead {
                    sb.last_used = Instant::now();
                    return Ok(Arc::new(LocalPage::new(sb.page.clone(), sb.headed)));
                }
                if let Some(mut sb) = sessions.remove(session_id) {
                    sb.handler.abort();
                    if let Some(task) = sb.console_task.take() {
                        task.abort();
                    }
                    let _ = sb.browser.kill().await;
                    cleanup_user_data_dir(&sb.user_data_dir);
                }
            }
            let sb = launch(session_id, root, &settings).await?;
            let page = LocalPage::new(sb.page.clone(), sb.headed);
            sessions.insert(session_id.to_string(), sb);
            Ok(Arc::new(page))
        }
    }

    /// True when `session_id` currently has a live browser.
    pub async fn has_session(&self, session_id: &str) -> bool {
        self.sessions.lock().await.contains_key(session_id)
    }

    /// Facts about the session's browser (`None` when it has none).
    pub async fn info(&self, session_id: &str) -> Option<BrowserInfo> {
        let (page, headed, directory) = {
            let sessions = self.sessions.lock().await;
            let sb = sessions.get(session_id)?;
            (sb.page.clone(), sb.headed, sb.directory.clone())
        };
        let url = page.url().await.ok().flatten().unwrap_or_default();
        let title = page.get_title().await.ok().flatten().unwrap_or_default();
        Some(BrowserInfo {
            headed,
            directory,
            url,
            title,
        })
    }

    /// The console buffer of the session's page (`None` without a browser).
    /// WP-AUTOVERIFY (F8-1): read by `browser_console`.
    pub async fn console(&self, session_id: &str) -> Option<SharedConsole> {
        self.sessions
            .lock()
            .await
            .get(session_id)
            .map(|sb| sb.console.clone())
    }

    /// Whether the session's browser has a visible window.
    pub async fn is_headed(&self, session_id: &str) -> Option<bool> {
        self.sessions
            .lock()
            .await
            .get(session_id)
            .map(|sb| sb.headed)
    }

    /// Back / forward / reload on the session's page (viewer toolbar).
    /// Returns the resulting `(url, title)`.
    pub async fn navigate_history(
        &self,
        session_id: &str,
        action: HistoryAction,
    ) -> Result<(String, String), String> {
        let page = {
            let mut sessions = self.sessions.lock().await;
            let sb = sessions
                .get_mut(session_id)
                .ok_or_else(|| "no page is open in this session yet".to_string())?;
            sb.last_used = Instant::now();
            sb.page.clone()
        };
        navigate_page_history(&page, action).await?;
        let url = page.url().await.ok().flatten().unwrap_or_default();
        let title = page.get_title().await.ok().flatten().unwrap_or_default();
        Ok((url, title))
    }

    // ── frames / viewer ─────────────────────────────────────────────────

    /// Capture one frame of the session's page right now (viewer on-demand
    /// request). Also counts as a viewer poll, keeping the stream alive.
    pub async fn capture_frame(self: &Arc<Self>, session_id: &str) -> Result<Frame, String> {
        self.mark_viewer(session_id);
        let (page, directory, headed, seq) = {
            let mut sessions = self.sessions.lock().await;
            let sb = sessions
                .get_mut(session_id)
                .ok_or_else(|| "no page is open in this session yet".to_string())?;
            sb.seq += 1;
            (sb.page.clone(), sb.directory.clone(), sb.headed, sb.seq)
        };
        let capture = frames::capture(&page).await?;
        Ok(frames::frame_from(
            session_id, &directory, seq, headed, capture,
        ))
    }

    /// Record that a viewer window is watching `session_id` (keeps the frame
    /// stream on for [`frames::VIEWER_TTL`] in non-viewer display modes) and
    /// start the streamer if a tool call is already running.
    pub fn mark_viewer(self: &Arc<Self>, session_id: &str) {
        self.viewers
            .lock()
            .unwrap()
            .insert(session_id.to_string(), Instant::now());
        let active = self
            .activity
            .lock()
            .unwrap()
            .get(session_id)
            .map(|a| a.active)
            .unwrap_or(0);
        if active > 0 {
            self.maybe_start_streamer(session_id);
        }
    }

    /// Whether a viewer window polled `session_id` within [`frames::VIEWER_TTL`].
    pub fn viewer_recent(&self, session_id: &str) -> bool {
        self.viewers
            .lock()
            .unwrap()
            .get(session_id)
            .is_some_and(|t| t.elapsed() < frames::VIEWER_TTL)
    }

    /// Whether frames should be streamed for `session_id` right now.
    fn should_stream(&self, session_id: &str, directory: &str) -> bool {
        if self.sink().is_none() {
            return false;
        }
        self.settings_for(Path::new(directory)).display == BrowserDisplay::Viewer
            || self.viewer_recent(session_id)
    }

    /// Mark the start of a tool call on `session_id`; the returned guard ends
    /// it. Starts the frame streamer when someone is watching.
    pub fn activity(self: &Arc<Self>, session_id: &str) -> ActivityGuard {
        {
            let mut activity = self.activity.lock().unwrap();
            activity.entry(session_id.to_string()).or_default().active += 1;
        }
        self.maybe_start_streamer(session_id);
        ActivityGuard {
            driver: Arc::downgrade(self),
            session_id: session_id.to_string(),
        }
    }

    /// Number of tool calls currently running on `session_id`.
    pub fn active_calls(&self, session_id: &str) -> usize {
        self.activity
            .lock()
            .unwrap()
            .get(session_id)
            .map(|a| a.active)
            .unwrap_or(0)
    }

    /// Whether a frame streamer task is currently running for `session_id`.
    pub fn is_streaming(&self, session_id: &str) -> bool {
        self.activity
            .lock()
            .unwrap()
            .get(session_id)
            .and_then(|a| a.streamer.as_ref())
            .is_some_and(|h| !h.is_finished())
    }

    fn end_activity(&self, session_id: &str) {
        let mut activity = self.activity.lock().unwrap();
        if let Some(a) = activity.get_mut(session_id) {
            a.active = a.active.saturating_sub(1);
        }
    }

    fn maybe_start_streamer(self: &Arc<Self>, session_id: &str) {
        // The directory is only known once the browser exists; a call that
        // launches the browser starts streaming on its next tick (the guard
        // is taken before `page()`), which is fine: nothing to show yet.
        let directory = match self.sessions.try_lock() {
            Ok(sessions) => sessions.get(session_id).map(|sb| sb.directory.clone()),
            Err(_) => None,
        };
        let Some(directory) = directory else {
            return;
        };
        if !self.should_stream(session_id, &directory) {
            return;
        }
        let mut activity = self.activity.lock().unwrap();
        let entry = activity.entry(session_id.to_string()).or_default();
        if entry.streamer.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let weak = Arc::downgrade(self);
        let sid = session_id.to_string();
        entry.streamer = Some(tokio::spawn(async move {
            loop {
                let Some(driver) = weak.upgrade() else {
                    break;
                };
                let active = driver.active_calls(&sid);
                let sink = driver.sink();
                if let (Some(sink), Ok(frame)) = (sink, driver.capture_frame_quiet(&sid).await) {
                    sink(frame);
                }
                // One final frame after the last call ended, then stop.
                if active == 0 || !driver.has_session(&sid).await {
                    break;
                }
                drop(driver);
                tokio::time::sleep(frames::FRAME_INTERVAL).await;
            }
        }));
    }

    /// `capture_frame` without touching the viewer timestamp (streamer use).
    async fn capture_frame_quiet(self: &Arc<Self>, session_id: &str) -> Result<Frame, String> {
        let (page, directory, headed, seq) = {
            let mut sessions = self.sessions.lock().await;
            let sb = sessions
                .get_mut(session_id)
                .ok_or_else(|| "no page".to_string())?;
            sb.seq += 1;
            (sb.page.clone(), sb.directory.clone(), sb.headed, sb.seq)
        };
        let capture = frames::capture(&page).await?;
        Ok(frames::frame_from(
            session_id, &directory, seq, headed, capture,
        ))
    }

    // ── teardown ────────────────────────────────────────────────────────

    /// Close (and reap) the browser bound to `session_id`, if any.
    pub async fn close(&self, session_id: &str) -> bool {
        let removed = self.sessions.lock().await.remove(session_id);
        self.forget_activity(session_id);
        match removed {
            Some(sb) => {
                shutdown(sb).await;
                true
            }
            None => false,
        }
    }

    /// Close every browser (server shutdown).
    pub async fn close_all(&self) {
        let all: Vec<(String, SessionBrowser)> = self.sessions.lock().await.drain().collect();
        for (id, sb) in all {
            self.forget_activity(&id);
            shutdown(sb).await;
        }
    }

    /// Close browsers idle for longer than `max_idle`. Returns how many were closed.
    pub async fn reap_idle(&self, max_idle: Duration) -> usize {
        let now = Instant::now();
        let idle: Vec<(String, SessionBrowser)> = {
            let mut sessions = self.sessions.lock().await;
            let ids: Vec<String> = sessions
                .iter()
                .filter(|(_, sb)| now.duration_since(sb.last_used) > max_idle)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| sessions.remove(&id).map(|sb| (id, sb)))
                .collect()
        };
        let n = idle.len();
        for (id, sb) in idle {
            tracing_debug("closing idle browser");
            self.forget_activity(&id);
            shutdown(sb).await;
        }
        n
    }

    fn forget_activity(&self, session_id: &str) {
        if let Some(a) = self.activity.lock().unwrap().remove(session_id)
            && let Some(h) = a.streamer
        {
            h.abort();
        }
        self.viewers.lock().unwrap().remove(session_id);
    }

    fn ensure_reaper(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        self.reaper.get_or_init(|| {
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(REAP_INTERVAL).await;
                    let Some(driver) = me.upgrade() else {
                        break;
                    };
                    driver.reap_idle(IDLE_TIMEOUT).await;
                }
            })
        });
    }
}

/// Back / forward / reload on a raw local page; shared by
/// [`BrowserDriver::navigate_history`] and [`super::page::LocalPage`].
/// Returns `Ok` on completion (history navigation waits for the load event,
/// best effort).
pub async fn navigate_page_history(page: &Page, action: HistoryAction) -> Result<(), String> {
    match action {
        HistoryAction::Reload => {
            tokio::time::timeout(REQUEST_TIMEOUT, page.reload())
                .await
                .map_err(|_| "reload timed out".to_string())?
                .map_err(|e| format!("reload failed: {e}"))?;
        }
        HistoryAction::Back | HistoryAction::Forward => {
            let history = page
                .execute(GetNavigationHistoryParams::default())
                .await
                .map_err(|e| format!("navigation history unavailable: {e}"))?;
            let current = history.current_index;
            let target = match action {
                HistoryAction::Back => current - 1,
                _ => current + 1,
            };
            let entry = usize::try_from(target)
                .ok()
                .and_then(|i| history.entries.get(i))
                .ok_or_else(|| {
                    format!(
                        "cannot go {} from here",
                        if action == HistoryAction::Back {
                            "back"
                        } else {
                            "forward"
                        }
                    )
                })?;
            page.execute(NavigateToHistoryEntryParams::new(entry.id))
                .await
                .map_err(|e| format!("history navigation failed: {e}"))?;
            let _ = tokio::time::timeout(Duration::from_secs(10), page.wait_for_navigation()).await;
        }
    }
    Ok(())
}

/// Close the browser bound to `session_id` (called by `bebok-core` on turn
/// abort and session deletion). No-op when the session never opened one.
pub async fn close_session(session_id: &str) -> bool {
    if DRIVER.get().is_none() {
        return false;
    }
    BrowserDriver::global().close(session_id).await
}

/// Close every browser the process launched.
pub async fn close_all() {
    if DRIVER.get().is_none() {
        return;
    }
    BrowserDriver::global().close_all().await;
}

/// Remember the `browser` config section for an instance root on the global
/// driver (called by `bebok-core` on instance load/reload).
pub fn configure(root: &Path, settings: BrowserSettings) {
    BrowserDriver::global().configure(root, settings);
}

/// Install the global frame sink (called once by `bebok-core`).
pub fn set_frame_sink(sink: FrameSink) {
    BrowserDriver::global().set_frame_sink(sink);
}

fn tracing_debug(msg: &str) {
    // `bebok-tools` has no tracing dependency; keep the hook in one place so
    // it can be swapped for `tracing::debug!` without touching call sites.
    let _ = msg;
}

/// Settings are keyed by the root as the engine spells it; only trailing
/// separators are stripped so `C:\p\` and `C:\p` agree.
fn normalize_root(root: &Path) -> PathBuf {
    let s = root.to_string_lossy();
    let trimmed = s.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        root.to_path_buf()
    } else {
        PathBuf::from(trimmed)
    }
}

/// Per-session profile directory: keeps two browsers from fighting over one
/// `user-data-dir` (Chrome refuses to start a second instance on a profile).
fn user_data_dir_for(session_id: &str) -> PathBuf {
    let safe: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    std::env::temp_dir()
        .join("bebok-browser")
        .join(format!("{safe}-{}", std::process::id()))
}

fn cleanup_user_data_dir(dir: &PathBuf) {
    // Best effort; Chrome may still hold a lock for a moment on Windows.
    let _ = std::fs::remove_dir_all(dir);
}

/// The executable to launch, or a descriptive error listing what was probed.
pub fn resolve_executable() -> Result<PathBuf, String> {
    if let Some(p) = discovery::discover_binary() {
        return Ok(p);
    }
    chromiumoxide::detection::default_executable(Default::default()).map_err(|_| {
        format!(
            "no Chromium/Chrome/Edge binary found (probed: {}). Install Google Chrome, \
             Microsoft Edge or Chromium, or set BEBOK_BROWSER to the executable path.",
            discovery::probe_description()
        )
    })
}

/// One Chrome switch for the launch command line, kept as `key` + `values`
/// (no leading dashes) because chromiumoxide's `BrowserConfigBuilder::args`
/// adds the `--` itself and merges values of a repeated key (`disable-features`
/// is also set by its defaults). Passing pre-prefixed strings produced
/// `----disable-gpu`, which Chrome silently ignored (E2E B3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchArg {
    pub key: String,
    pub values: Vec<String>,
}

impl LaunchArg {
    fn key(key: &str) -> Self {
        Self {
            key: key.to_string(),
            values: Vec::new(),
        }
    }

    fn values<V: ToString>(key: &str, values: impl IntoIterator<Item = V>) -> Self {
        Self {
            key: key.to_string(),
            values: values.into_iter().map(|v| v.to_string()).collect(),
        }
    }

    /// The switch as chromiumoxide will put it on the command line.
    pub fn render(&self) -> String {
        if self.values.is_empty() {
            format!("--{}", self.key)
        } else {
            format!("--{}={}", self.key, self.values.join(","))
        }
    }
}

/// Extra Chrome arguments for a launch. Headed: a real window of the nominal
/// viewport size, optionally placed where the desktop client asked, and no
/// first-run / automation chrome that would cover the page. Headless: the
/// nominal viewport is emulated so screenshots are stable.
pub fn launch_args(settings: &BrowserSettings, headless: bool) -> Vec<LaunchArg> {
    let mut args = vec![
        LaunchArg::key("disable-gpu"),
        LaunchArg::key("no-first-run"),
        LaunchArg::key("no-default-browser-check"),
    ];
    if !headless {
        args.push(LaunchArg::key("disable-infobars"));
        args.push(LaunchArg::key("disable-session-crashed-bubble"));
        args.push(LaunchArg::key("disable-sync"));
        args.push(LaunchArg::values(
            "disable-features",
            ["Translate", "MediaRouter", "msEdgeWelcomePage"],
        ));
        if let Some((x, y)) = settings.effective_window_position() {
            args.push(LaunchArg::values("window-position", [x, y]));
        }
    }
    args
}

async fn launch(
    session_id: &str,
    root: &Path,
    settings: &BrowserSettings,
) -> Result<SessionBrowser, String> {
    let executable = resolve_executable()?;
    let user_data_dir = user_data_dir_for(session_id);
    let _ = std::fs::create_dir_all(&user_data_dir);
    let headless = settings.headless();

    let mut builder = BrowserConfig::builder()
        .chrome_executable(&executable)
        .window_size(VIEWPORT_WIDTH, VIEWPORT_HEIGHT)
        .user_data_dir(&user_data_dir)
        .launch_timeout(LAUNCH_TIMEOUT)
        .request_timeout(REQUEST_TIMEOUT);
    for arg in launch_args(settings, headless) {
        let values = arg.values.iter().map(String::as_str).collect::<Vec<_>>();
        builder = builder.arg((arg.key.as_str(), values.as_slice()));
    }
    if headless {
        // `--headless=new`: the only headless mode current Chrome/Edge ship.
        // The viewport is emulated so captures are exactly 1280x800.
        builder = builder.new_headless_mode().viewport(Viewport {
            width: VIEWPORT_WIDTH,
            height: VIEWPORT_HEIGHT,
            device_scale_factor: Some(1.0),
            emulating_mobile: false,
            is_landscape: true,
            has_touch: false,
        });
    } else {
        // Headed: no viewport emulation - what the user sees in the window is
        // exactly what screenshots and viewer frames show, so coordinates
        // from a screenshot map 1:1 onto the visible page.
        builder = builder.with_head().viewport(None);
    }
    // Linux CI images and containers commonly run as root, where Chrome's
    // sandbox refuses to start; the user can opt out of the sandbox there.
    if std::env::var_os("BEBOK_BROWSER_NO_SANDBOX").is_some() {
        builder = builder.no_sandbox();
    }
    let config = builder
        .build()
        .map_err(|e| format!("browser config: {e}"))?;

    let (mut browser, mut handler) =
        match tokio::time::timeout(LAUNCH_TIMEOUT, Browser::launch(config)).await {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                cleanup_user_data_dir(&user_data_dir);
                return Err(format!("failed to launch {}: {e}", executable.display()));
            }
            Err(_) => {
                cleanup_user_data_dir(&user_data_dir);
                return Err(format!(
                    "timed out launching {} after {}s",
                    executable.display(),
                    LAUNCH_TIMEOUT.as_secs()
                ));
            }
        };
    // The handler must be polled for the whole life of the browser: it pumps
    // CDP events and resolves every command future.
    let handler_task = tokio::spawn(async move {
        while let Some(item) = handler.next().await {
            if item.is_err() {
                break;
            }
        }
    });

    // Headed: reuse the tab Chrome opened at startup instead of adding a
    // second one next to it (the user would see two tabs). Headless keeps
    // the proven `new_page` path.
    let mut page = None;
    if !headless
        && let Ok(Ok(mut pages)) =
            tokio::time::timeout(Duration::from_secs(5), browser.pages()).await
        && !pages.is_empty()
    {
        page = Some(pages.remove(0));
    }
    let page = match page {
        Some(p) => p,
        None => {
            match tokio::time::timeout(REQUEST_TIMEOUT, browser.new_page("about:blank")).await {
                Ok(Ok(p)) => p,
                Ok(Err(e)) => {
                    handler_task.abort();
                    let _ = browser.kill().await;
                    cleanup_user_data_dir(&user_data_dir);
                    return Err(format!("failed to open a page: {e}"));
                }
                Err(_) => {
                    handler_task.abort();
                    let _ = browser.kill().await;
                    cleanup_user_data_dir(&user_data_dir);
                    return Err("timed out opening the first page".to_string());
                }
            }
        }
    };

    // Console capture must be listening before the first navigation so
    // `browser_console` can report everything since that navigation. A
    // failure here is not fatal: the tool then reports an empty buffer.
    let console: SharedConsole = Arc::new(std::sync::Mutex::new(ConsoleBuffer::default()));
    let console_task = match console::attach(&page, console.clone()).await {
        Ok(task) => Some(task),
        Err(e) => {
            tracing_debug(&format!("console capture unavailable: {e}"));
            None
        }
    };

    Ok(SessionBrowser {
        browser,
        page,
        handler: handler_task,
        last_used: Instant::now(),
        user_data_dir,
        directory: root.to_string_lossy().into_owned(),
        headed: !headless,
        seq: 0,
        console,
        console_task,
    })
}

async fn shutdown(mut sb: SessionBrowser) {
    // Graceful close first (lets Chrome flush its profile), then make sure
    // the process is gone so nothing is orphaned.
    let _ = tokio::time::timeout(Duration::from_secs(5), sb.browser.close()).await;
    if !matches!(sb.browser.try_wait(), Ok(Some(_))) {
        let _ = sb.browser.kill().await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(5), sb.browser.wait()).await;
    sb.handler.abort();
    if let Some(task) = sb.console_task.take() {
        task.abort();
    }
    cleanup_user_data_dir(&sb.user_data_dir);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::settings::BrowserRemote;

    #[test]
    fn user_data_dir_is_per_session_and_filesystem_safe() {
        let a = user_data_dir_for("11111111-2222-3333-4444-555555555555");
        let b = user_data_dir_for("weird/id:with*chars");
        assert_ne!(a, b);
        let leaf = b.file_name().unwrap().to_string_lossy().to_string();
        assert!(!leaf.contains('/') && !leaf.contains(':') && !leaf.contains('*'));
        assert!(a.starts_with(std::env::temp_dir().join("bebok-browser")));
    }

    #[tokio::test]
    async fn close_is_a_noop_for_unknown_sessions() {
        let driver = Arc::new(BrowserDriver::default());
        assert!(!driver.close("nope").await);
        assert!(!driver.has_session("nope").await);
        assert_eq!(driver.reap_idle(Duration::ZERO).await, 0);
        assert!(driver.info("nope").await.is_none());
        assert_eq!(driver.is_headed("nope").await, None);
    }

    #[tokio::test]
    async fn close_session_without_a_driver_is_a_noop() {
        // Never initialises the global driver as a side effect.
        assert!(!close_session("never-opened").await);
    }

    #[test]
    fn settings_are_per_root_and_default_when_unset() {
        let driver = BrowserDriver::default();
        let root = Path::new("C:/projects/a");
        assert_eq!(driver.settings_for(root), BrowserSettings::default());
        driver.configure(
            root,
            BrowserSettings {
                display: BrowserDisplay::Viewer,
                window_position: Some((1, 2)),
                ..BrowserSettings::default()
            },
        );
        assert_eq!(driver.settings_for(root).display, BrowserDisplay::Viewer);
        // Trailing separators do not create a second key.
        assert_eq!(
            driver.settings_for(Path::new("C:/projects/a/")).display,
            BrowserDisplay::Viewer
        );
        assert_eq!(
            driver.settings_for(Path::new("C:/projects/b")).display,
            BrowserDisplay::Headed
        );
    }

    #[test]
    fn launch_args_differ_between_headed_and_headless() {
        let settings = BrowserSettings {
            display: BrowserDisplay::Headed,
            window_position: Some((1300, 40)),
            ..BrowserSettings::default()
        };
        let headed: Vec<String> = launch_args(&settings, false)
            .iter()
            .map(LaunchArg::render)
            .collect();
        assert!(headed.iter().any(|a| a == "--window-position=1300,40"));
        assert!(headed.iter().any(|a| a == "--disable-infobars"));
        assert!(headed.iter().any(|a| a == "--no-first-run"));
        assert!(
            headed
                .iter()
                .any(|a| a == "--disable-features=Translate,MediaRouter,msEdgeWelcomePage")
        );
        let headless: Vec<String> = launch_args(&settings, true)
            .iter()
            .map(LaunchArg::render)
            .collect();
        assert!(!headless.iter().any(|a| a.starts_with("--window-position")));
        assert!(!headless.iter().any(|a| a == "--disable-infobars"));
        assert!(headless.iter().any(|a| a == "--no-first-run"));
        // No position configured and no env hint: Chrome decides.
        let none = launch_args(&BrowserSettings::default(), false);
        if std::env::var_os("BEBOK_BROWSER_WINDOW_POS").is_none() {
            assert!(!none.iter().any(|a| a.key == "window-position"));
        }
    }

    /// E2E B3: chromiumoxide prefixes every key with `--` itself, so the keys
    /// we hand it must carry no dashes, and the rendered switch exactly one
    /// `--` (the observed bug was `----disable-gpu`, ignored by Chrome).
    #[test]
    fn launch_args_are_prefixed_exactly_once() {
        let settings = BrowserSettings {
            display: BrowserDisplay::Headed,
            window_position: Some((10, 20)),
            ..BrowserSettings::default()
        };
        for headless in [false, true] {
            for arg in launch_args(&settings, headless) {
                assert!(
                    !arg.key.starts_with('-'),
                    "key must not carry its own dashes: {}",
                    arg.key
                );
                assert!(
                    !arg.key.contains('='),
                    "values go in `values`, not the key: {}",
                    arg.key
                );
                let rendered = arg.render();
                assert!(rendered.starts_with("--"), "{rendered}");
                assert!(!rendered.starts_with("---"), "double prefix: {rendered}");
                assert_eq!(rendered.matches("--").count(), 1, "{rendered}");
            }
        }
    }

    #[tokio::test]
    async fn activity_guard_counts_and_releases() {
        let driver = Arc::new(BrowserDriver::default());
        assert_eq!(driver.active_calls("s"), 0);
        let g1 = driver.activity("s");
        let g2 = driver.activity("s");
        assert_eq!(driver.active_calls("s"), 2);
        // No browser for "s": nothing to stream.
        assert!(!driver.is_streaming("s"));
        drop(g1);
        assert_eq!(driver.active_calls("s"), 1);
        drop(g2);
        assert_eq!(driver.active_calls("s"), 0);
        // Underflow-safe.
        driver.end_activity("s");
        assert_eq!(driver.active_calls("s"), 0);
    }

    #[tokio::test]
    async fn viewer_marks_expire_and_close_forgets_them() {
        let driver = Arc::new(BrowserDriver::default());
        assert!(!driver.viewer_recent("s"));
        driver.mark_viewer("s");
        assert!(driver.viewer_recent("s"));
        driver.close("s").await;
        assert!(!driver.viewer_recent("s"));
        // Capturing without a browser is an error, not a panic.
        let err = driver.capture_frame("s").await.unwrap_err();
        assert!(err.contains("no page is open"), "{err}");
        let err = driver
            .navigate_history("s", HistoryAction::Back)
            .await
            .unwrap_err();
        assert!(err.contains("no page is open"), "{err}");
    }

    #[test]
    fn should_stream_requires_a_sink_and_a_watcher() {
        let driver = Arc::new(BrowserDriver::default());
        let root = "C:/projects/x";
        driver.configure(
            Path::new(root),
            BrowserSettings {
                display: BrowserDisplay::Viewer,
                window_position: None,
                ..BrowserSettings::default()
            },
        );
        // Viewer mode but no sink installed: nothing to deliver to.
        assert!(!driver.should_stream("s", root));
        driver.set_frame_sink(Arc::new(|_f| {}));
        assert!(driver.should_stream("s", root));
        // Headed/drawer: only while a viewer window polls.
        driver.configure(Path::new(root), BrowserSettings::default());
        assert!(!driver.should_stream("s", root));
        driver.mark_viewer("s");
        assert!(driver.should_stream("s", root));
    }

    #[test]
    fn remote_disabled_returns_local_fallback() {
        // Remote disabled (default): settings_for returns LocalPage
        let driver = BrowserDriver::default();
        let root = Path::new("C:/projects/a");
        let settings = driver.settings_for(root);
        assert!(!settings.remote.enabled);
    }

    #[test]
    fn remote_enabled_without_port_returns_error() {
        // Remote enabled but no port configured: should error
        let driver = BrowserDriver::default();
        let root = Path::new("C:/projects/b");
        driver.configure(
            root,
            BrowserSettings {
                display: BrowserDisplay::Headed,
                window_position: None,
                remote: BrowserRemote {
                    enabled: true,
                    extension_port: None,
                    tab_id: None,
                },
            },
        );
        let settings = driver.settings_for(root);
        assert!(settings.remote.enabled);
        assert!(settings.remote.extension_port.is_none());
    }

    #[test]
    fn remote_enabled_with_port_configured() {
        // Remote enabled with port configured
        let driver = BrowserDriver::default();
        let root = Path::new("C:/projects/c");
        driver.configure(
            root,
            BrowserSettings {
                display: BrowserDisplay::Headed,
                window_position: None,
                remote: BrowserRemote {
                    enabled: true,
                    extension_port: Some(4317),
                    tab_id: None,
                },
            },
        );
        let settings = driver.settings_for(root);
        assert!(settings.remote.enabled);
        assert_eq!(settings.remote.extension_port, Some(4317));
    }
}
