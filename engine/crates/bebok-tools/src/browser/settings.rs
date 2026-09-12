//! Browser display settings (WP-BROWSER2 / F7-6).
//!
//! The `browser` config section decides how the agent's browser is shown:
//!
//! ```json
//! { "browser": { "display": "headed" | "viewer" | "drawer", "windowPosition": [x, y] } }
//! ```
//!
//! * `headed` (default on desktop) - Chrome/Edge is launched with a visible OS
//!   window (1280x800) so the user watches the agent directly.
//! * `viewer` - headless Chrome; the engine streams `browser.frame` events so
//!   a separate Bebok "viewer" window can mirror the page live.
//! * `drawer` - headless Chrome; only the right-drawer thumbnail (tool
//!   screenshots) is shown, frames are streamed on demand only.
//!
//! `BEBOK_BROWSER_HEADLESS=1` forces headless regardless of the setting (CI,
//! remote engines). On Linux without `DISPLAY`/`WAYLAND_DISPLAY`, and on
//! Android, a headed launch is impossible and silently degrades to headless.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How the agent's browser is presented to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BrowserDisplay {
    /// Visible browser window (non-headless Chrome/Edge).
    #[default]
    Headed,
    /// Headless + live frame stream for the Bebok viewer window.
    Viewer,
    /// Headless; drawer thumbnail only.
    Drawer,
}

impl BrowserDisplay {
    pub const ALL: [BrowserDisplay; 3] = [Self::Headed, Self::Viewer, Self::Drawer];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Headed => "headed",
            Self::Viewer => "viewer",
            Self::Drawer => "drawer",
        }
    }

    /// Parse a config string (case-insensitive, trimmed). Unknown -> `None`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "headed" | "window" | "visible" => Some(Self::Headed),
            "viewer" => Some(Self::Viewer),
            "drawer" | "panel" => Some(Self::Drawer),
            _ => None,
        }
    }
}

/// Resolved `browser` section for one instance root.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BrowserSettings {
    pub display: BrowserDisplay,
    /// Desired top-left corner of the headed window (screen pixels). Set by
    /// the desktop client to "right of the app"; `None` = let Chrome decide.
    pub window_position: Option<(i32, i32)>,
}

impl BrowserSettings {
    /// Parse the `browser` config section. Malformed values fall back to the
    /// defaults (never an error: a typo in config must not break the tools).
    pub fn from_config(value: &Value) -> Self {
        let mut out = Self::default();
        let Some(obj) = value.as_object() else {
            return out;
        };
        if let Some(display) = obj.get("display").and_then(Value::as_str) {
            if let Some(mode) = BrowserDisplay::parse(display) {
                out.display = mode;
            }
        }
        let pos = obj
            .get("windowPosition")
            .or_else(|| obj.get("window_position"));
        out.window_position = pos.and_then(parse_position);
        out
    }

    /// Whether the browser must be launched headless: the env override wins,
    /// then the display mode, then the platform's ability to show a window.
    pub fn headless(&self) -> bool {
        if env_flag("BEBOK_BROWSER_HEADLESS") {
            return true;
        }
        if self.display != BrowserDisplay::Headed {
            return true;
        }
        !can_show_window()
    }

    /// Effective window position: config, then `BEBOK_BROWSER_WINDOW_POS=x,y`.
    pub fn effective_window_position(&self) -> Option<(i32, i32)> {
        self.window_position.or_else(|| {
            std::env::var("BEBOK_BROWSER_WINDOW_POS")
                .ok()
                .and_then(|s| parse_position(&Value::String(s)))
        })
    }
}

/// `[x, y]`, `{ "x": .., "y": .. }` or `"x,y"`.
fn parse_position(v: &Value) -> Option<(i32, i32)> {
    let to_i32 = |n: &Value| n.as_i64().map(|i| i.clamp(-16_384, 16_384) as i32);
    match v {
        Value::Array(items) if items.len() == 2 => Some((to_i32(&items[0])?, to_i32(&items[1])?)),
        Value::Object(o) => Some((to_i32(o.get("x")?)?, to_i32(o.get("y")?)?)),
        Value::String(s) => {
            let mut it = s.split(',').map(|p| p.trim().parse::<i64>().ok());
            let x = it.next().flatten()?;
            let y = it.next().flatten()?;
            if it.next().is_some() {
                return None;
            }
            Some((
                x.clamp(-16_384, 16_384) as i32,
                y.clamp(-16_384, 16_384) as i32,
            ))
        }
        _ => None,
    }
}

fn env_flag(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            !(v.is_empty() || v == "0" || v == "false" || v == "no" || v == "off")
        }
        Err(_) => false,
    }
}

/// Whether this process can open a visible browser window at all.
fn can_show_window() -> bool {
    if cfg!(target_os = "android") {
        return false;
    }
    if cfg!(any(target_os = "windows", target_os = "macos")) {
        return true;
    }
    // Linux/BSD: a display server must be reachable.
    std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_is_headed_without_position() {
        let s = BrowserSettings::from_config(&Value::Null);
        assert_eq!(s.display, BrowserDisplay::Headed);
        assert_eq!(s.window_position, None);
        assert_eq!(BrowserSettings::default(), s);
    }

    #[test]
    fn parses_display_case_insensitively_and_ignores_garbage() {
        for (raw, want) in [
            ("headed", BrowserDisplay::Headed),
            ("VIEWER", BrowserDisplay::Viewer),
            (" drawer ", BrowserDisplay::Drawer),
            ("window", BrowserDisplay::Headed),
        ] {
            assert_eq!(
                BrowserSettings::from_config(&json!({ "display": raw })).display,
                want,
                "{raw}"
            );
        }
        assert_eq!(
            BrowserSettings::from_config(&json!({ "display": "hologram" })).display,
            BrowserDisplay::Headed
        );
        assert_eq!(
            BrowserSettings::from_config(&json!({ "display": 7 })).display,
            BrowserDisplay::Headed
        );
        assert_eq!(BrowserDisplay::parse("nope"), None);
    }

    #[test]
    fn display_round_trips_through_serde_and_as_str() {
        for mode in BrowserDisplay::ALL {
            let s = serde_json::to_string(&mode).unwrap();
            assert_eq!(s, format!("\"{}\"", mode.as_str()));
            let back: BrowserDisplay = serde_json::from_str(&s).unwrap();
            assert_eq!(back, mode);
        }
    }

    #[test]
    fn window_position_accepts_array_object_and_string() {
        assert_eq!(
            BrowserSettings::from_config(&json!({ "windowPosition": [1300, 20] })).window_position,
            Some((1300, 20))
        );
        assert_eq!(
            BrowserSettings::from_config(&json!({ "window_position": { "x": -5, "y": 0 } }))
                .window_position,
            Some((-5, 0))
        );
        assert_eq!(
            BrowserSettings::from_config(&json!({ "windowPosition": "10, 20" })).window_position,
            Some((10, 20))
        );
        assert_eq!(
            BrowserSettings::from_config(&json!({ "windowPosition": "10,20,30" })).window_position,
            None
        );
        assert_eq!(
            BrowserSettings::from_config(&json!({ "windowPosition": [1] })).window_position,
            None
        );
        // Absurd values are clamped, not rejected.
        assert_eq!(
            BrowserSettings::from_config(&json!({ "windowPosition": [999_999, -999_999] }))
                .window_position,
            Some((16_384, -16_384))
        );
    }

    #[test]
    fn non_headed_modes_are_always_headless() {
        for mode in [BrowserDisplay::Viewer, BrowserDisplay::Drawer] {
            let s = BrowserSettings {
                display: mode,
                window_position: None,
            };
            assert!(s.headless(), "{mode:?}");
        }
    }

    #[test]
    fn env_flag_semantics() {
        // Only checks the parser; never mutates process env (tests run in parallel).
        assert!(!env_flag("BEBOK_TEST_FLAG_THAT_DOES_NOT_EXIST_12345"));
    }
}
