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
use chromiumoxide::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, Viewport as ClipViewport,
};
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
    /// CSS-pixel size of the captured viewport; `0` when unknown (use the
    /// image's own size).
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
    let metrics = page_metrics(page).await;
    let mut params = ScreenshotParams::builder()
        .format(CaptureScreenshotFormat::Jpeg)
        .quality(FRAME_JPEG_QUALITY)
        .full_page(false);
    if let Some(clip) = css_pixel_clip(metrics) {
        params = params.clip(clip);
    }
    let shot = tokio::time::timeout(CAPTURE_TIMEOUT, page.screenshot(params.build()))
        .await
        .map_err(|_| "frame capture timed out".to_string())?
        .map_err(|e| format!("frame capture failed: {e}"))?;
    let url = page.url().await.ok().flatten().unwrap_or_default();
    let title = page.get_title().await.ok().flatten().unwrap_or_default();
    let (width, height) = viewport_size(metrics);
    Ok((shot, url, title, width, height))
}

/// `[innerWidth, innerHeight, devicePixelRatio]` of the page, or `None` when
/// unreadable (mid-navigation).
pub async fn page_metrics(page: &Page) -> Option<(f64, f64, f64)> {
    let res = tokio::time::timeout(
        Duration::from_secs(2),
        page.evaluate("[window.innerWidth, window.innerHeight, window.devicePixelRatio]"),
    )
    .await;
    let Ok(Ok(v)) = res else {
        return None;
    };
    match v.into_value::<Vec<f64>>() {
        Ok(d) if d.len() == 3 && d[0] > 0.0 && d[1] > 0.0 && d[2] > 0.0 => Some((d[0], d[1], d[2])),
        _ => None,
    }
}

/// Viewport size in CSS pixels (what the screenshot covers). `(0, 0)` when
/// the metrics were unreadable: the capture was then taken unscaled, and a
/// client must use the image's own dimensions instead of a guess.
pub fn viewport_size(metrics: Option<(f64, f64, f64)>) -> (u32, u32) {
    match metrics {
        Some((w, h, _)) => (w.round() as u32, h.round() as u32),
        None => (0, 0),
    }
}

/// On a HiDPI screen a headed window renders at `devicePixelRatio` > 1, so a
/// plain capture is bigger than the CSS viewport the click coordinates refer
/// to. This clip scales the capture back to 1 image pixel per CSS pixel;
/// `None` when no scaling is needed (headless, or DPR 1).
pub fn css_pixel_clip(metrics: Option<(f64, f64, f64)>) -> Option<ClipViewport> {
    let (w, h, dpr) = metrics?;
    if (dpr - 1.0).abs() < 0.01 {
        return None;
    }
    Some(ClipViewport {
        x: 0.0,
        y: 0.0,
        width: w,
        height: h,
        scale: 1.0 / dpr,
    })
}

/// Clip covering the whole document (`css_content_size`), scaled to CSS
/// pixels like [`css_pixel_clip`]; used with `captureBeyondViewport` for
/// full-page captures of a headed window.
pub fn content_clip(width: f64, height: f64, metrics: Option<(f64, f64, f64)>) -> ClipViewport {
    let dpr = metrics.map(|(_, _, d)| d).unwrap_or(1.0);
    ClipViewport {
        x: 0.0,
        y: 0.0,
        width: width.max(1.0),
        height: height.max(1.0),
        scale: 1.0 / dpr,
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
    fn css_pixel_clip_only_scales_hidpi_captures() {
        assert!(css_pixel_clip(None).is_none());
        assert!(css_pixel_clip(Some((1280.0, 800.0, 1.0))).is_none());
        let clip = css_pixel_clip(Some((1266.0, 650.0, 1.5))).unwrap();
        assert_eq!((clip.x, clip.y), (0.0, 0.0));
        assert_eq!((clip.width, clip.height), (1266.0, 650.0));
        assert!((clip.scale - 1.0 / 1.5).abs() < 1e-9);
        assert_eq!(viewport_size(Some((1266.4, 650.0, 1.5))), (1266, 650));
        assert_eq!(viewport_size(None), (0, 0));
        let full = content_clip(1266.0, 3200.0, Some((1266.0, 650.0, 2.0)));
        assert_eq!((full.width, full.height, full.scale), (1266.0, 3200.0, 0.5));
        let full = content_clip(0.0, 0.0, None);
        assert_eq!((full.width, full.height, full.scale), (1.0, 1.0, 1.0));
    }

    #[test]
    fn frame_from_encodes_base64_and_copies_metadata() {
        let f = frame_from(
            "s1",
            "C:/proj",
            7,
            true,
            (
                vec![0xFF, 0xD8, 0xFF],
                "https://x/".into(),
                "X".into(),
                1280,
                800,
            ),
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
