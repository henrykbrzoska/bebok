//! Per-session headless browser lifecycle (WP-BROWSER / F6-17).
//!
//! One headless Chromium process + one page per Bebok session, launched
//! lazily by the first `browser_*` call and owned by the process-wide
//! [`BrowserDriver`]. Two sessions never share a page (each gets its own
//! browser process with its own `user-data-dir`), so parallel sessions cannot
//! fight over navigation state.
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
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::page::Page;
use futures::StreamExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::discovery;

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
}

/// Process-wide registry of per-session browsers.
#[derive(Default)]
pub struct BrowserDriver {
    sessions: Mutex<HashMap<String, SessionBrowser>>,
    reaper: OnceLock<JoinHandle<()>>,
}

static DRIVER: OnceLock<Arc<BrowserDriver>> = OnceLock::new();

impl BrowserDriver {
    /// The process-wide driver shared by every `browser_*` tool.
    pub fn global() -> Arc<BrowserDriver> {
        DRIVER.get_or_init(|| Arc::new(BrowserDriver::default())).clone()
    }

    /// The page bound to `session_id`, launching the browser on first use.
    pub async fn page(self: &Arc<Self>, session_id: &str) -> Result<Page, String> {
        self.ensure_reaper();
        let mut sessions = self.sessions.lock().await;
        if let Some(sb) = sessions.get_mut(session_id) {
            // A crashed/exited browser must not be handed out again.
            let dead = matches!(sb.browser.try_wait(), Ok(Some(_)));
            if !dead {
                sb.last_used = Instant::now();
                return Ok(sb.page.clone());
            }
            if let Some(mut sb) = sessions.remove(session_id) {
                sb.handler.abort();
                let _ = sb.browser.kill().await;
                cleanup_user_data_dir(&sb.user_data_dir);
            }
        }
        let sb = launch(session_id).await?;
        let page = sb.page.clone();
        sessions.insert(session_id.to_string(), sb);
        Ok(page)
    }

    /// True when `session_id` currently has a live browser.
    pub async fn has_session(&self, session_id: &str) -> bool {
        self.sessions.lock().await.contains_key(session_id)
    }

    /// Close (and reap) the browser bound to `session_id`, if any.
    pub async fn close(&self, session_id: &str) -> bool {
        let removed = self.sessions.lock().await.remove(session_id);
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
        let all: Vec<SessionBrowser> = self.sessions.lock().await.drain().map(|(_, v)| v).collect();
        for sb in all {
            shutdown(sb).await;
        }
    }

    /// Close browsers idle for longer than `max_idle`. Returns how many were closed.
    pub async fn reap_idle(&self, max_idle: Duration) -> usize {
        let now = Instant::now();
        let idle: Vec<SessionBrowser> = {
            let mut sessions = self.sessions.lock().await;
            let ids: Vec<String> = sessions
                .iter()
                .filter(|(_, sb)| now.duration_since(sb.last_used) > max_idle)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter().filter_map(|id| sessions.remove(&id)).collect()
        };
        let n = idle.len();
        for sb in idle {
            tracing_debug("closing idle browser");
            shutdown(sb).await;
        }
        n
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

fn tracing_debug(msg: &str) {
    // `bebok-tools` has no tracing dependency; keep the hook in one place so
    // it can be swapped for `tracing::debug!` without touching call sites.
    let _ = msg;
}

/// Per-session profile directory: keeps two browsers from fighting over one
/// `user-data-dir` (Chrome refuses to start a second instance on a profile).
fn user_data_dir_for(session_id: &str) -> PathBuf {
    let safe: String = session_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
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

async fn launch(session_id: &str) -> Result<SessionBrowser, String> {
    let executable = resolve_executable()?;
    let user_data_dir = user_data_dir_for(session_id);
    let _ = std::fs::create_dir_all(&user_data_dir);

    let mut builder = BrowserConfig::builder()
        .chrome_executable(&executable)
        // `--headless=new`: the only headless mode current Chrome/Edge ship.
        .new_headless_mode()
        .window_size(VIEWPORT_WIDTH, VIEWPORT_HEIGHT)
        .viewport(Viewport {
            width: VIEWPORT_WIDTH,
            height: VIEWPORT_HEIGHT,
            device_scale_factor: Some(1.0),
            emulating_mobile: false,
            is_landscape: true,
            has_touch: false,
        })
        .user_data_dir(&user_data_dir)
        .launch_timeout(LAUNCH_TIMEOUT)
        .request_timeout(REQUEST_TIMEOUT)
        .arg("--disable-gpu")
        .arg("--no-first-run")
        .arg("--no-default-browser-check");
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

    let page = match tokio::time::timeout(REQUEST_TIMEOUT, browser.new_page("about:blank")).await
    {
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
    };

    Ok(SessionBrowser {
        browser,
        page,
        handler: handler_task,
        last_used: Instant::now(),
        user_data_dir,
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
    cleanup_user_data_dir(&sb.user_data_dir);
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[tokio::test]
    async fn close_session_without_a_driver_is_a_noop() {
        // Never initialises the global driver as a side effect.
        assert!(!close_session("never-opened").await);
    }
}
