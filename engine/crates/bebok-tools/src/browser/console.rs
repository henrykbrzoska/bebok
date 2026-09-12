//! Console / error capture for `browser_console` (WP-AUTOVERIFY / F8-1).
//!
//! Every session browser gets one capture task that subscribes to the page's
//! CDP event streams and appends to a bounded [`ConsoleBuffer`]:
//!
//! * `Runtime.consoleApiCalled` — `console.log/info/warn/error/...`,
//! * `Runtime.exceptionThrown` — uncaught exceptions and unhandled rejections,
//! * `Log.entryAdded` — browser-generated entries such as
//!   "Failed to load resource: 404" (network, security, deprecation, ...),
//! * `Page.frameNavigated` (main frame) — clears the buffer, so the tool
//!   reports "since the last navigation" without any bookkeeping in the
//!   tool itself.
//!
//! Runtime, Page and Log are enabled by chromiumoxide's page initialisation,
//! so subscribing is enough. The buffer keeps the newest [`MAX_ENTRIES`]
//! entries; the drop count is reported so the model knows it saw a tail.

use std::sync::{Arc, Mutex};

use chromiumoxide::cdp::browser_protocol::log::{EventEntryAdded, LogEntryLevel};
use chromiumoxide::cdp::browser_protocol::page::EventFrameNavigated;
use chromiumoxide::cdp::js_protocol::runtime::{
    ConsoleApiCalledType, EventConsoleApiCalled, EventExceptionThrown, RemoteObject,
};
use chromiumoxide::page::Page;
use futures::StreamExt;
use serde::Serialize;
use tokio::task::JoinHandle;

/// Newest entries kept per page.
pub const MAX_ENTRIES: usize = 500;
/// Longest text kept per entry (characters).
pub const MAX_TEXT_CHARS: usize = 2_000;

/// Severity of one entry (ordered: `Debug < Log < Info < Warning < Error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsoleLevel {
    Debug,
    Log,
    Info,
    Warning,
    Error,
}

impl ConsoleLevel {
    /// Parse the tool's `level` filter: the *minimum* severity to return.
    /// `all` (or absent) returns everything.
    pub fn parse_filter(raw: &str) -> Option<Option<Self>> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "all" | "*" => Some(None),
            "debug" | "verbose" => Some(Some(Self::Debug)),
            "log" => Some(Some(Self::Log)),
            "info" => Some(Some(Self::Info)),
            "warn" | "warning" | "warnings" => Some(Some(Self::Warning)),
            "error" | "errors" => Some(Some(Self::Error)),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Log => "log",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    fn from_console_type(t: &ConsoleApiCalledType) -> Self {
        match t {
            ConsoleApiCalledType::Error | ConsoleApiCalledType::Assert => Self::Error,
            ConsoleApiCalledType::Warning => Self::Warning,
            ConsoleApiCalledType::Info => Self::Info,
            ConsoleApiCalledType::Debug | ConsoleApiCalledType::Trace => Self::Debug,
            _ => Self::Log,
        }
    }

    fn from_log_level(l: &LogEntryLevel) -> Self {
        match l {
            LogEntryLevel::Error => Self::Error,
            LogEntryLevel::Warning => Self::Warning,
            LogEntryLevel::Info => Self::Info,
            LogEntryLevel::Verbose => Self::Debug,
        }
    }
}

/// Where an entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsoleSource {
    /// `console.*` call from page code.
    Console,
    /// Uncaught exception / unhandled promise rejection.
    Exception,
    /// Browser-generated log entry (network, security, deprecation, ...).
    Browser,
}

/// One captured message.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConsoleEntry {
    pub level: ConsoleLevel,
    pub source: ConsoleSource,
    pub text: String,
    /// Script/resource URL when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// 1-based line when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
    /// Page timestamp (ms since epoch) as reported by the browser.
    pub timestamp: f64,
}

impl ConsoleEntry {
    /// One-line rendering for the tool output.
    pub fn render(&self) -> String {
        let mut s = format!("[{}]", self.level.as_str());
        if self.source == ConsoleSource::Exception {
            s.push_str(" uncaught");
        } else if self.source == ConsoleSource::Browser {
            s.push_str(" browser");
        }
        s.push(' ');
        s.push_str(&self.text);
        if let Some(url) = &self.url {
            s.push_str(" (");
            s.push_str(url);
            if let Some(line) = self.line {
                s.push_str(&format!(":{line}"));
            }
            s.push(')');
        }
        s
    }
}

/// Bounded per-page buffer, cleared on main-frame navigation.
#[derive(Debug, Default)]
pub struct ConsoleBuffer {
    entries: Vec<ConsoleEntry>,
    /// Entries dropped because the buffer was full (since the last clear).
    dropped: usize,
    /// Main-frame navigations observed (diagnostics).
    navigations: u64,
}

impl ConsoleBuffer {
    pub fn push(&mut self, mut entry: ConsoleEntry) {
        if entry.text.chars().count() > MAX_TEXT_CHARS {
            entry.text = entry.text.chars().take(MAX_TEXT_CHARS).collect();
            entry.text.push_str(" [truncated]");
        }
        if self.entries.len() >= MAX_ENTRIES {
            self.entries.remove(0);
            self.dropped += 1;
        }
        self.entries.push(entry);
    }

    /// Forget everything (a new document started).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.dropped = 0;
        self.navigations += 1;
    }

    /// Entries at or above `min` (all when `None`), oldest first.
    pub fn snapshot(&self, min: Option<ConsoleLevel>) -> Vec<ConsoleEntry> {
        self.entries
            .iter()
            .filter(|e| min.is_none_or(|m| e.level >= m))
            .cloned()
            .collect()
    }

    pub fn dropped(&self) -> usize {
        self.dropped
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn navigations(&self) -> u64 {
        self.navigations
    }

    /// Count per level (for the structured summary).
    pub fn counts(&self) -> serde_json::Value {
        let mut errors = 0;
        let mut warnings = 0;
        let mut other = 0;
        for e in &self.entries {
            match e.level {
                ConsoleLevel::Error => errors += 1,
                ConsoleLevel::Warning => warnings += 1,
                _ => other += 1,
            }
        }
        serde_json::json!({ "errors": errors, "warnings": warnings, "other": other })
    }
}

pub type SharedConsole = Arc<Mutex<ConsoleBuffer>>;

/// Render a console argument the way DevTools would (strings bare, objects
/// via their description/preview, primitives as JSON).
fn render_arg(obj: &RemoteObject) -> String {
    if let Some(v) = &obj.value {
        return match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
    }
    if let Some(u) = &obj.unserializable_value {
        return u.inner().to_string();
    }
    if let Some(d) = &obj.description {
        return d.clone();
    }
    obj.class_name
        .clone()
        .unwrap_or_else(|| format!("[{:?}]", obj.r#type))
}

fn console_entry(ev: &EventConsoleApiCalled) -> Option<ConsoleEntry> {
    // Grouping/timing/count calls carry no message worth reporting.
    if matches!(
        ev.r#type,
        ConsoleApiCalledType::Clear
            | ConsoleApiCalledType::StartGroup
            | ConsoleApiCalledType::StartGroupCollapsed
            | ConsoleApiCalledType::EndGroup
            | ConsoleApiCalledType::Profile
            | ConsoleApiCalledType::ProfileEnd
    ) {
        return None;
    }
    let text = ev.args.iter().map(render_arg).collect::<Vec<_>>().join(" ");
    let frame = ev
        .stack_trace
        .as_ref()
        .and_then(|st| st.call_frames.first());
    Some(ConsoleEntry {
        level: ConsoleLevel::from_console_type(&ev.r#type),
        source: ConsoleSource::Console,
        text,
        url: frame.map(|f| f.url.clone()).filter(|u| !u.is_empty()),
        line: frame.map(|f| f.line_number + 1),
        timestamp: *ev.timestamp.inner(),
    })
}

fn exception_entry(ev: &EventExceptionThrown) -> ConsoleEntry {
    let d = &ev.exception_details;
    let text = d
        .exception
        .as_ref()
        .and_then(|o| o.description.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| d.text.clone());
    let url = d.url.clone().or_else(|| {
        d.stack_trace
            .as_ref()
            .and_then(|st| st.call_frames.first())
            .map(|f| f.url.clone())
    });
    ConsoleEntry {
        level: ConsoleLevel::Error,
        source: ConsoleSource::Exception,
        text,
        url: url.filter(|u| !u.is_empty()),
        line: Some(d.line_number + 1),
        timestamp: *ev.timestamp.inner(),
    }
}

fn log_entry(ev: &EventEntryAdded) -> ConsoleEntry {
    let e = &ev.entry;
    ConsoleEntry {
        level: ConsoleLevel::from_log_level(&e.level),
        source: ConsoleSource::Browser,
        text: e.text.clone(),
        url: e.url.clone().filter(|u| !u.is_empty()),
        line: e.line_number.map(|l| l + 1),
        timestamp: *e.timestamp.inner(),
    }
}

/// Subscribe to the page's console/exception/log/navigation events and feed
/// `buffer` until the page (or the returned task) goes away.
pub async fn attach(page: &Page, buffer: SharedConsole) -> Result<JoinHandle<()>, String> {
    let mut console = page
        .event_listener::<EventConsoleApiCalled>()
        .await
        .map_err(|e| format!("console listener: {e}"))?;
    let mut exceptions = page
        .event_listener::<EventExceptionThrown>()
        .await
        .map_err(|e| format!("exception listener: {e}"))?;
    let mut logs = page
        .event_listener::<EventEntryAdded>()
        .await
        .map_err(|e| format!("log listener: {e}"))?;
    let mut navigations = page
        .event_listener::<EventFrameNavigated>()
        .await
        .map_err(|e| format!("navigation listener: {e}"))?;
    Ok(tokio::spawn(async move {
        loop {
            tokio::select! {
                ev = console.next() => match ev {
                    Some(ev) => {
                        if let Some(entry) = console_entry(&ev) {
                            buffer.lock().unwrap().push(entry);
                        }
                    }
                    None => break,
                },
                ev = exceptions.next() => match ev {
                    Some(ev) => buffer.lock().unwrap().push(exception_entry(&ev)),
                    None => break,
                },
                ev = logs.next() => match ev {
                    Some(ev) => buffer.lock().unwrap().push(log_entry(&ev)),
                    None => break,
                },
                ev = navigations.next() => match ev {
                    Some(ev) => {
                        if ev.frame.parent_id.is_none() {
                            buffer.lock().unwrap().clear();
                        }
                    }
                    None => break,
                },
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(level: ConsoleLevel, text: &str) -> ConsoleEntry {
        ConsoleEntry {
            level,
            source: ConsoleSource::Console,
            text: text.to_string(),
            url: None,
            line: None,
            timestamp: 0.0,
        }
    }

    #[test]
    fn level_filter_parses_aliases_and_rejects_junk() {
        assert_eq!(ConsoleLevel::parse_filter("all"), Some(None));
        assert_eq!(ConsoleLevel::parse_filter(""), Some(None));
        assert_eq!(
            ConsoleLevel::parse_filter("error"),
            Some(Some(ConsoleLevel::Error))
        );
        assert_eq!(
            ConsoleLevel::parse_filter("WARN"),
            Some(Some(ConsoleLevel::Warning))
        );
        assert_eq!(
            ConsoleLevel::parse_filter("verbose"),
            Some(Some(ConsoleLevel::Debug))
        );
        assert_eq!(ConsoleLevel::parse_filter("loud"), None);
    }

    #[test]
    fn levels_are_ordered_by_severity() {
        assert!(ConsoleLevel::Error > ConsoleLevel::Warning);
        assert!(ConsoleLevel::Warning > ConsoleLevel::Info);
        assert!(ConsoleLevel::Info > ConsoleLevel::Log);
        assert!(ConsoleLevel::Log > ConsoleLevel::Debug);
    }

    #[test]
    fn snapshot_filters_by_minimum_level() {
        let mut b = ConsoleBuffer::default();
        b.push(entry(ConsoleLevel::Log, "hello"));
        b.push(entry(ConsoleLevel::Warning, "careful"));
        b.push(entry(ConsoleLevel::Error, "boom"));
        assert_eq!(b.snapshot(None).len(), 3);
        let warn = b.snapshot(Some(ConsoleLevel::Warning));
        assert_eq!(warn.len(), 2);
        assert_eq!(warn[0].text, "careful");
        let err = b.snapshot(Some(ConsoleLevel::Error));
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].text, "boom");
        assert_eq!(b.counts()["errors"], 1);
        assert_eq!(b.counts()["warnings"], 1);
        assert_eq!(b.counts()["other"], 1);
    }

    #[test]
    fn buffer_is_bounded_and_counts_drops() {
        let mut b = ConsoleBuffer::default();
        for i in 0..(MAX_ENTRIES + 7) {
            b.push(entry(ConsoleLevel::Log, &i.to_string()));
        }
        assert_eq!(b.len(), MAX_ENTRIES);
        assert_eq!(b.dropped(), 7);
        // Oldest were dropped, newest kept.
        assert_eq!(b.snapshot(None)[0].text, "7");
    }

    #[test]
    fn clear_resets_everything_and_counts_navigations() {
        let mut b = ConsoleBuffer::default();
        b.push(entry(ConsoleLevel::Error, "x"));
        b.clear();
        assert!(b.is_empty());
        assert_eq!(b.dropped(), 0);
        assert_eq!(b.navigations(), 1);
    }

    #[test]
    fn long_texts_are_truncated() {
        let mut b = ConsoleBuffer::default();
        b.push(entry(ConsoleLevel::Log, &"x".repeat(MAX_TEXT_CHARS + 50)));
        let e = &b.snapshot(None)[0];
        assert!(e.text.ends_with("[truncated]"));
        assert!(e.text.chars().count() < MAX_TEXT_CHARS + 20);
    }

    #[test]
    fn render_marks_source_and_location() {
        let mut e = entry(ConsoleLevel::Error, "TypeError: x is not a function");
        e.source = ConsoleSource::Exception;
        e.url = Some("http://localhost:4200/main.js".to_string());
        e.line = Some(12);
        assert_eq!(
            e.render(),
            "[error] uncaught TypeError: x is not a function (http://localhost:4200/main.js:12)"
        );
        let mut e = entry(ConsoleLevel::Error, "Failed to load resource: 404");
        e.source = ConsoleSource::Browser;
        assert_eq!(e.render(), "[error] browser Failed to load resource: 404");
        assert_eq!(entry(ConsoleLevel::Log, "hi").render(), "[log] hi");
    }
}
