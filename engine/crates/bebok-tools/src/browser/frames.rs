//! Live frame stream for the Bebok browser viewer (WP-BROWSER2 / F7-6).
//!
//! While a `browser_*` tool is running (and shortly after it finishes) the
//! driver captures the page at most [`MAX_FPS`] times per second and hands
//! every capture to the process-wide [`FrameSink`]. `bebok-core` installs a
//! sink that publishes the frame as a `browser.frame` event on the global
//! SSE bus; the client's `/browser-view` window renders it.
//!
//! Frames are only produced when somebody can see them: in `viewer` display
//! mode always, otherwise only while a viewer window has asked for a frame
//! (`GET /session/{id}/browser/frame`) within [`VIEWER_TTL`]. Headed mode
//! therefore costs nothing unless the user opens the viewer window too.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::{Page, ScreenshotParams};
use serde::Serialize;

/// Upper bound on the stream rate.
pub const MAX_FPS: u32 = 2;
/// Interval between two captures at [`MAX_FPS`].
pub const FRAME_INTERVAL: Duration = Duration::from_millis(1000 / MAX_FPS as u64);
/// A viewer window that fetched a frame this recently keeps the stream on.
pub const VIEWER_TTL: Duration = Duration::from_secs(120);
/// JPEG quality of streamed frames (they are transient; keep SSE light).
pub const FRAME_JPEG_QUALITY: i64 = 60;
/// Upper bound on one capture (a navigating page can stall CDP briefly).
pub const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);

/// One captured frame of a session's browser page.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Frame {
    #[serde(rename = "sessionID")]
    pub session_id: String,
    /// Instance directory the session belongs to (the SSE envelope needs it).
    pub directory: String,
    pub url: String,
    pub title: String,
    pub media_type: String,
    /// Raw base64 (no `data:` prefix).
    pub data: String,
    pub width: u32,
    pub height: u32,
    /// Monotonic per-session counter so a client can drop stale frames.
    pub seq: u64,
    /// Whether the browser runs with a visible OS window (headed mode).
    pub headed: bool,
}

/// Receives every streamed frame (installed once by `bebok-core`).
pub type FrameSink = Arc<dyn Fn(Frame) + Send + Sync>;

/// Capture one JPEG frame of `page` plus its current url/title/size.
///
/// Returns `Err` on CDP failures (e.g. mid-navigation); the streamer skips
/// that tick, an on-demand request reports it.
pub async fn capture(page: &Page) -> Result<(Vec<u8>, String, String, u32, u32), String> {
    let params = ScreenshotParams::builder()
        .format(CaptureScreenshotFormat::Jpeg)
        .quality(FRAME_JPEG_QUALITY)
        .full_page(false)
        .build();
    let shot = tokio::time::timeout(CAPTURE_TIMEOUT, page.screenshot(params))
        .await
        .map_err(|_| "frame capture timed out".to_string())?
        .map_err(|e| format!("frame capture failed: {e}"))?;
    let url = page.url().await.ok().flatten().unwrap_or_default();
    let title = page.get_title().await.ok().flatten().unwrap_or_default();
    let (width, height) = viewport_size(page).await;
    Ok((shot, url, title, width, height))
}

/// `innerWidth`/`innerHeight` of the page (CSS pixels; what the screenshot
/// covers). Falls back to the driver's nominal viewport when unreadable.
pub async fn viewport_size(page: &Page) -> (u32, u32) {
    let fallback = (super::driver::VIEWPORT_WIDTH, super::driver::VIEWPORT_HEIGHT);
    let res = tokio::time::timeout(
        Duration::from_secs(2),
        page.evaluate("[window.innerWidth, window.innerHeight]"),
    )
    .await;
    let Ok(Ok(v)) = res else {
        return fallback;
    };
    match v.into_value::<Vec<u32>>() {
        Ok(dims) if dims.len() == 2 && dims[0] > 0 && dims[1] > 0 => (dims[0], dims[1]),
        _ => fallback,
    }
}

/// Assemble a [`Frame`] from a capture.
pub fn frame_from(
    session_id: &str,
    directory: &str,
    seq: u64,
    headed: bool,
    capture: (Vec<u8>, String, String, u32, u32),
) -> Frame {
    let (bytes, url, title, width, height) = capture;
    Frame {
        session_id: session_id.to_string(),
        directory: directory.to_string(),
        url,
        title,
        media_type: "image/jpeg".to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        width,
        height,
        seq,
        headed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_interval_matches_max_fps() {
        assert_eq!(MAX_FPS, 2);
        assert_eq!(FRAME_INTERVAL, Duration::from_millis(500));
    }

    #[test]
    fn frame_from_encodes_base64_and_copies_metadata() {
        let f = frame_from(
            "s1",
            "C:/proj",
            7,
            true,
            (vec![0xFF, 0xD8, 0xFF], "https://x/".into(), "X".into(), 1280, 800),
        );
        assert_eq!(f.session_id, "s1");
        assert_eq!(f.directory, "C:/proj");
        assert_eq!(f.seq, 7);
        assert!(f.headed);
        assert_eq!(f.media_type, "image/jpeg");
        assert_eq!(f.data, "/9j/");
        assert_eq!((f.width, f.height), (1280, 800));
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(json["sessionID"], "s1");
        assert_eq!(json["media_type"], "image/jpeg");
    }
}
