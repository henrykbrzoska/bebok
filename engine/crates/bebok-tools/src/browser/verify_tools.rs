//! The verification trio (WP-AUTOVERIFY / F8-1): `browser_console`,
//! `browser_wait`, `browser_find`.
//!
//! These exist so an agent can *check* a frontend change on its own instead
//! of eyeballing a screenshot: read what the page logged, wait for the
//! change to actually render, and address elements by reliable selectors.
//! They share the driver, the call bounding and the `structured` state
//! conventions of [`super::tools`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};

use super::args::{self, ConsoleArgs, FindArgs, WaitArgs};
use super::driver::BrowserDriver;
use super::tools::{bounded, page_for, page_state};
use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Poll interval for `browser_wait`.
const POLL_INTERVAL: Duration = Duration::from_millis(150);
/// `network_idle`: no resource started loading for this long.
const NETWORK_QUIET: Duration = Duration::from_millis(600);

// ── browser_console ─────────────────────────────────────────────────────

pub struct BrowserConsole {
    pub driver: Arc<BrowserDriver>,
}

#[async_trait]
impl Tool for BrowserConsole {
    fn name(&self) -> &str {
        "browser_console"
    }

    fn description(&self) -> &str {
        "List the browser console output of this session's page since the last navigation: console.log/info/warn/error calls, uncaught exceptions, unhandled promise rejections and browser-generated entries such as failed resource loads (404s). Use it right after browser_open / browser_click / browser_wait to check that your frontend change produced no errors — level=\"error\" for a quick pass/fail, \"warning\" to include warnings, \"all\" for everything. Returns the newest entries with level, source and location plus per-level counts."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "level": {
                    "type": "string",
                    "enum": ["all", "debug", "log", "info", "warning", "error"],
                    "description": "Minimum severity to return (default all). \"error\" returns only errors and uncaught exceptions; \"warning\" returns warnings and errors."
                },
                "max": {
                    "type": "integer",
                    "description": "Return at most this many newest entries (default 100, max 500)."
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, raw: Value) -> ToolOutput {
        let title = "browser_console".to_string();
        let ConsoleArgs { min_level, max } = match args::parse_console(&raw) {
            Ok(a) => a,
            Err(e) => return ToolOutput::new(format!("error: {e}"), title),
        };
        let driver = self.driver.clone();
        let work = async {
            let Some(console) = driver.console(&ctx.session_id).await else {
                return Err(
                    "no page is open in this session yet; call browser_open first".to_string(),
                );
            };
            let _live = driver.activity(&ctx.session_id);
            let page = page_for(&driver, &ctx).await?;
            let (entries, total, dropped, counts) = {
                let buf = console.lock().unwrap();
                (
                    buf.snapshot(min_level),
                    buf.len(),
                    buf.dropped(),
                    buf.counts(),
                )
            };
            Ok::<_, String>((entries, total, dropped, counts, page_state(&page).await))
        };
        match bounded(&ctx, &title, work).await {
            Ok((entries, total, dropped, counts, (url, page_title))) => {
                let matched = entries.len();
                let shown: Vec<_> = entries.iter().rev().take(max).rev().collect();
                let mut text = format!(
                    "Console of {url} since the last navigation: {} error(s), {} warning(s), {} other",
                    counts["errors"], counts["warnings"], counts["other"]
                );
                if dropped > 0 {
                    text.push_str(&format!(" ({dropped} older entries dropped)"));
                }
                text.push('\n');
                if let Some(level) = min_level {
                    text.push_str(&format!("filter: level >= {}\n", level.as_str()));
                }
                if shown.is_empty() {
                    text.push_str(if total == 0 {
                        "(no console output)"
                    } else {
                        "(no entries at or above the requested level)"
                    });
                } else {
                    if shown.len() < matched {
                        text.push_str(&format!(
                            "(showing the newest {} of {matched} matching entries)\n",
                            shown.len()
                        ));
                    }
                    for e in &shown {
                        text.push_str(&e.render());
                        text.push('\n');
                    }
                }
                ToolOutput::new(text.trim_end().to_string(), title).with_structured(json!({
                    "url": url,
                    "title": page_title,
                    "counts": counts,
                    "total": total,
                    "dropped": dropped,
                    "entries": shown,
                }))
            }
            Err(out) => out,
        }
    }
}

// ── browser_wait ────────────────────────────────────────────────────────

pub struct BrowserWait {
    pub driver: Arc<BrowserDriver>,
}

/// JS probe evaluated on every poll: returns `{selector, text, idle}`
/// booleans (each `true` when its condition is met or not requested).
fn wait_probe_js(a: &WaitArgs) -> String {
    let selector = a
        .selector
        .as_deref()
        .map(|s| serde_json::to_string(s).unwrap_or_default())
        .unwrap_or_else(|| "null".to_string());
    let text = a
        .text
        .as_deref()
        .map(|s| serde_json::to_string(&s.to_lowercase()).unwrap_or_default())
        .unwrap_or_else(|| "null".to_string());
    format!(
        r#"(() => {{
  const sel = {selector}; const want = {text}; const hidden = {hidden}; const idle = {idle};
  const quiet = {quiet};
  const visible = (el) => {{
    if (!el) return false;
    const st = getComputedStyle(el);
    if (st.display === 'none' || st.visibility === 'hidden' || st.opacity === '0') return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  }};
  let selOk = true, selCount = 0;
  if (sel !== null) {{
    let els = [];
    try {{ els = Array.from(document.querySelectorAll(sel)); }} catch (e) {{ return {{ error: 'invalid selector: ' + e.message }}; }}
    selCount = els.length;
    const anyVisible = els.some(visible);
    selOk = hidden ? !anyVisible : anyVisible;
  }}
  let textOk = true;
  if (want !== null) {{
    const body = document.body ? document.body.innerText : '';
    textOk = body.toLowerCase().includes(want);
  }}
  let idleOk = true, pending = 0;
  if (idle) {{
    const now = performance.now();
    const res = performance.getEntriesByType('resource');
    const last = res.length ? Math.max(...res.map(r => r.responseEnd || r.startTime)) : 0;
    pending = res.filter(r => !r.responseEnd || r.responseEnd > now).length;
    idleOk = document.readyState === 'complete' && pending === 0 && (now - last) >= quiet;
  }}
  return {{ selector: selOk, selectorCount: selCount, text: textOk, idle: idleOk, ready: document.readyState }};
}})()"#,
        hidden = a.hidden,
        idle = a.network_idle,
        quiet = NETWORK_QUIET.as_millis(),
    )
}

fn describe_conditions(a: &WaitArgs) -> String {
    let mut parts = Vec::new();
    if let Some(s) = &a.selector {
        parts.push(if a.hidden {
            format!("'{s}' hidden")
        } else {
            format!("'{s}' visible")
        });
    }
    if let Some(t) = &a.text {
        parts.push(format!("text \"{t}\""));
    }
    if a.network_idle {
        parts.push("network idle".to_string());
    }
    parts.join(" and ")
}

#[async_trait]
impl Tool for BrowserWait {
    fn name(&self) -> &str {
        "browser_wait"
    }

    fn description(&self) -> &str {
        "Wait until this session's page satisfies a condition: a CSS selector matches a visible element (or is hidden with hidden=true), a text appears in the visible page text (case-insensitive), and/or the network has gone idle (page loaded, no resource loading for ~0.6s). Use it after browser_open / browser_click before screenshotting or reading so client-rendered UI (Angular/React/Vue) has actually rendered; on timeout it reports which condition was still unmet (and how many elements matched the selector) so you can adapt."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string", "description": "CSS selector that must match a visible element (e.g. '.summary-card', '[data-testid=save]')." },
                "hidden": { "type": "boolean", "description": "With selector: wait until NO visible element matches (spinner gone). Default false." },
                "text": { "type": "string", "description": "Text that must appear somewhere in the page's visible text (case-insensitive substring)." },
                "network_idle": { "type": "boolean", "description": "Also wait for document ready + no resource loading for ~0.6s (default false)." },
                "timeout_ms": { "type": "integer", "description": "Give up after this many milliseconds (default 10000, max 60000)." }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, raw: Value) -> ToolOutput {
        let title = "browser_wait".to_string();
        let a = match args::parse_wait(&raw) {
            Ok(a) => a,
            Err(e) => return ToolOutput::new(format!("error: {e}"), title),
        };
        let what = describe_conditions(&a);
        let title = format!("browser_wait {what}");
        let driver = self.driver.clone();
        let work = async {
            if !driver.has_session(&ctx.session_id).await {
                return Err(
                    "no page is open in this session yet; call browser_open first".to_string(),
                );
            }
            let _live = driver.activity(&ctx.session_id);
            let page = page_for(&driver, &ctx).await?;
            let js = wait_probe_js(&a);
            let started = Instant::now();
            let deadline = started + Duration::from_millis(a.timeout_ms);
            loop {
                let probe = match page.evaluate(js.as_str()).await {
                    Ok(v) => {
                        if let Some(err) = v.get("error").and_then(Value::as_str) {
                            return Err(err.to_string());
                        }
                        let ok = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
                        if ok("selector") && ok("text") && ok("idle") {
                            let (url, page_title) = page_state(&page).await;
                            return Ok((true, started.elapsed(), v, url, page_title));
                        }
                        v
                    }
                    // Mid-navigation evaluations fail transiently; keep polling.
                    Err(e) => json!({ "evaluate_error": e.to_string() }),
                };
                if Instant::now() >= deadline {
                    let (url, page_title) = page_state(&page).await;
                    return Ok((false, started.elapsed(), probe, url, page_title));
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        };
        match bounded(&ctx, &title, work).await {
            Ok((met, elapsed, probe, url, page_title)) => {
                let ms = elapsed.as_millis();
                let text = if met {
                    format!("Condition met after {ms} ms: {what}\nnow at {url}")
                } else {
                    let mut unmet = Vec::new();
                    let flag = |k: &str| probe.get(k).and_then(Value::as_bool).unwrap_or(false);
                    if a.selector.is_some() && !flag("selector") {
                        let n = probe
                            .get("selectorCount")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        unmet.push(format!(
                            "selector '{}' {} ({n} element(s) match, {})",
                            a.selector.as_deref().unwrap_or(""),
                            if a.hidden {
                                "still visible"
                            } else {
                                "not visible"
                            },
                            if a.hidden {
                                "at least one is visible"
                            } else {
                                "none visible"
                            }
                        ));
                    }
                    if a.text.is_some() && !flag("text") {
                        unmet.push(format!(
                            "text \"{}\" not found in the page text",
                            a.text.as_deref().unwrap_or("")
                        ));
                    }
                    if a.network_idle && !flag("idle") {
                        unmet.push(format!(
                            "network not idle (readyState={})",
                            probe.get("ready").and_then(Value::as_str).unwrap_or("?")
                        ));
                    }
                    if let Some(e) = probe.get("evaluate_error").and_then(Value::as_str) {
                        unmet.push(format!("last probe failed: {e}"));
                    }
                    format!(
                        "Timed out after {ms} ms waiting for {what}\nunmet: {}\nnow at {url}. Try browser_find to see what is on the page, or browser_console for errors.",
                        unmet.join("; ")
                    )
                };
                ToolOutput::new(text, title).with_structured(json!({
                    "url": url,
                    "title": page_title,
                    "met": met,
                    "elapsed_ms": ms,
                    "probe": probe,
                }))
            }
            Err(out) => out,
        }
    }
}

// ── browser_find ────────────────────────────────────────────────────────

pub struct BrowserFind {
    pub driver: Arc<BrowserDriver>,
}

/// JS that lists visible interactive elements with a unique-ish selector.
/// Returns `[{role, name, selector, tag, x, y, w, h, value, href}]`.
fn find_js(a: &FindArgs) -> String {
    let query = a
        .query
        .as_deref()
        .map(|s| serde_json::to_string(&s.to_lowercase()).unwrap_or_default())
        .unwrap_or_else(|| "null".to_string());
    let within = a
        .within
        .as_deref()
        .map(|s| serde_json::to_string(s).unwrap_or_default())
        .unwrap_or_else(|| "null".to_string());
    format!(
        r#"(() => {{
  const query = {query}; const within = {within}; const max = {max};
  let root = document;
  if (within !== null) {{
    try {{ root = document.querySelector(within); }} catch (e) {{ return {{ error: 'invalid within selector: ' + e.message }}; }}
    if (!root) return {{ error: 'no element matches within selector ' + within }};
  }}
  const SEL = 'a[href],button,input:not([type=hidden]),select,textarea,summary,[role=button],[role=link],[role=tab],[role=menuitem],[role=checkbox],[role=radio],[role=switch],[role=option],[role=textbox],[contenteditable=""],[contenteditable=true],[onclick],[tabindex]:not([tabindex="-1"])';
  const visible = (el) => {{
    const st = getComputedStyle(el);
    if (st.display === 'none' || st.visibility === 'hidden') return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  }};
  const cssEscape = (s) => (window.CSS && CSS.escape) ? CSS.escape(s) : s.replace(/([^\w-])/g, '\\$1');
  const unique = (sel) => {{ try {{ return document.querySelectorAll(sel).length === 1; }} catch (e) {{ return false; }} }};
  const selectorFor = (el) => {{
    if (el.id && unique('#' + cssEscape(el.id))) return '#' + cssEscape(el.id);
    for (const attr of ['data-testid', 'data-test', 'data-cy', 'name', 'aria-label']) {{
      const v = el.getAttribute(attr);
      if (v) {{ const s = el.tagName.toLowerCase() + '[' + attr + '="' + v.replace(/"/g, '\\"') + '"]'; if (unique(s)) return s; }}
    }}
    // Structural path: tag:nth-of-type chain up to the nearest id'd ancestor (or body).
    const parts = [];
    let cur = el;
    while (cur && cur.nodeType === 1 && cur !== document.body) {{
      let part = cur.tagName.toLowerCase();
      if (cur.id && unique('#' + cssEscape(cur.id))) {{ parts.unshift('#' + cssEscape(cur.id)); break; }}
      const parent = cur.parentElement;
      if (parent) {{
        const sib = Array.from(parent.children).filter(c => c.tagName === cur.tagName);
        if (sib.length > 1) part += ':nth-of-type(' + (sib.indexOf(cur) + 1) + ')';
      }}
      parts.unshift(part);
      cur = parent;
    }}
    return parts.join(' > ');
  }};
  const roleOf = (el) => {{
    const r = el.getAttribute('role'); if (r) return r;
    const t = el.tagName.toLowerCase();
    if (t === 'a') return 'link';
    if (t === 'button' || t === 'summary') return 'button';
    if (t === 'select') return 'combobox';
    if (t === 'textarea') return 'textbox';
    if (t === 'input') {{
      const ty = (el.getAttribute('type') || 'text').toLowerCase();
      if (ty === 'checkbox' || ty === 'radio') return ty;
      if (ty === 'submit' || ty === 'button' || ty === 'reset') return 'button';
      return 'textbox';
    }}
    if (el.isContentEditable) return 'textbox';
    return 'generic';
  }};
  const nameOf = (el) => {{
    const aria = el.getAttribute('aria-label'); if (aria) return aria.trim();
    const lab = el.getAttribute('aria-labelledby');
    if (lab) {{ const t = lab.split(/\s+/).map(id => (document.getElementById(id) || {{}}).textContent || '').join(' ').trim(); if (t) return t; }}
    if (el.labels && el.labels.length) {{ const t = Array.from(el.labels).map(l => l.textContent).join(' ').trim(); if (t) return t; }}
    const own = (el.innerText || el.textContent || '').trim().replace(/\s+/g, ' ');
    if (own) return own;
    for (const attr of ['placeholder', 'title', 'alt', 'value']) {{ const v = el.getAttribute(attr); if (v) return v.trim(); }}
    const img = el.querySelector && el.querySelector('img[alt]'); if (img) return img.getAttribute('alt').trim();
    return '';
  }};
  const out = [];
  let total = 0;
  for (const el of root.querySelectorAll(SEL)) {{
    if (!visible(el)) continue;
    const item = {{ role: roleOf(el), name: nameOf(el).slice(0, 120), selector: selectorFor(el), tag: el.tagName.toLowerCase() }};
    const r = el.getBoundingClientRect();
    item.x = Math.round(r.left + r.width / 2); item.y = Math.round(r.top + r.height / 2);
    item.w = Math.round(r.width); item.h = Math.round(r.height);
    if ('value' in el && typeof el.value === 'string' && el.tagName !== 'BUTTON') item.value = el.value.slice(0, 80);
    if (el.tagName === 'A' && el.href) item.href = el.href.slice(0, 200);
    if (el.disabled) item.disabled = true;
    if (el.checked === true) item.checked = true;
    if (query !== null) {{
      const hay = (item.role + ' ' + item.name + ' ' + item.selector + ' ' + (item.value || '') + ' ' + (item.href || '')).toLowerCase();
      if (!hay.includes(query)) continue;
    }}
    total += 1;
    if (out.length < max) out.push(item);
  }}
  return {{ items: out, total: total }};
}})()"#,
        max = a.max,
    )
}

fn render_item(i: usize, item: &Value) -> String {
    let s = |k: &str| item.get(k).and_then(Value::as_str).unwrap_or("");
    let mut line = format!(
        "{}. {} \"{}\" -> {}",
        i + 1,
        s("role"),
        s("name"),
        s("selector")
    );
    let mut extras = Vec::new();
    if let (Some(x), Some(y)) = (
        item.get("x").and_then(Value::as_i64),
        item.get("y").and_then(Value::as_i64),
    ) {
        extras.push(format!("at {x},{y}"));
    }
    if let Some(v) = item.get("value").and_then(Value::as_str)
        && !v.is_empty()
    {
        extras.push(format!("value=\"{v}\""));
    }
    if item.get("checked").and_then(Value::as_bool) == Some(true) {
        extras.push("checked".to_string());
    }
    if item.get("disabled").and_then(Value::as_bool) == Some(true) {
        extras.push("disabled".to_string());
    }
    if let Some(h) = item.get("href").and_then(Value::as_str)
        && !h.is_empty()
    {
        extras.push(format!("href={h}"));
    }
    if !extras.is_empty() {
        line.push_str(" (");
        line.push_str(&extras.join(", "));
        line.push(')');
    }
    line
}

#[async_trait]
impl Tool for BrowserFind {
    fn name(&self) -> &str {
        "browser_find"
    }

    fn description(&self) -> &str {
        "List the visible interactive elements of this session's page (links, buttons, inputs, selects, tabs, checkboxes, ...) as an accessibility-style list: role, accessible name, a CSS selector that uniquely targets the element, its centre coordinates and current value. Use it to discover what to click or type into and to get selectors that browser_click / browser_type / browser_wait accept reliably — instead of guessing selectors from a screenshot. Filter with query (case-insensitive substring of role/name/selector) or scope with within (a CSS selector)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Only elements whose role, name, selector, value or href contains this text (case-insensitive), e.g. 'save' or 'inventory'." },
                "within": { "type": "string", "description": "CSS selector of a container; only elements inside its first match are listed." },
                "max": { "type": "integer", "description": "List at most this many elements (default 50, max 200)." }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, raw: Value) -> ToolOutput {
        let title = "browser_find".to_string();
        let a = match args::parse_find(&raw) {
            Ok(a) => a,
            Err(e) => return ToolOutput::new(format!("error: {e}"), title),
        };
        let title = match &a.query {
            Some(q) => format!("browser_find {q}"),
            None => title,
        };
        let driver = self.driver.clone();
        let work = async {
            if !driver.has_session(&ctx.session_id).await {
                return Err(
                    "no page is open in this session yet; call browser_open first".to_string(),
                );
            }
            let _live = driver.activity(&ctx.session_id);
            let page = page_for(&driver, &ctx).await?;
            let v = page
                .evaluate(find_js(&a).as_str())
                .await
                .map_err(|e| format!("listing elements failed: {e}"))?;
            if let Some(err) = v.get("error").and_then(Value::as_str) {
                return Err(err.to_string());
            }
            let items = v
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let total = v.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
            Ok::<_, String>((items, total, page_state(&page).await))
        };
        match bounded(&ctx, &title, work).await {
            Ok((items, total, (url, page_title))) => {
                let mut text = format!(
                    "{} interactive element(s) on {url}{}{}",
                    total,
                    a.query
                        .as_deref()
                        .map(|q| format!(" matching \"{q}\""))
                        .unwrap_or_default(),
                    a.within
                        .as_deref()
                        .map(|w| format!(" within '{w}'"))
                        .unwrap_or_default(),
                );
                if items.len() < total {
                    text.push_str(&format!(" (showing the first {})", items.len()));
                }
                text.push('\n');
                if items.is_empty() {
                    text.push_str("(none)");
                } else {
                    for (i, item) in items.iter().enumerate() {
                        text.push_str(&render_item(i, item));
                        text.push('\n');
                    }
                }
                ToolOutput::new(text.trim_end().to_string(), title).with_structured(json!({
                    "url": url,
                    "title": page_title,
                    "total": total,
                    "items": items,
                }))
            }
            Err(out) => out,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::tool_ctx;
    use tokio_util::sync::CancellationToken;

    fn ctx() -> ToolCtx {
        tool_ctx(
            std::env::temp_dir(),
            "test-session".to_string(),
            CancellationToken::new(),
        )
    }

    fn driver() -> Arc<BrowserDriver> {
        Arc::new(BrowserDriver::default())
    }

    #[test]
    fn names_and_read_only_classification() {
        let d = driver();
        let console = BrowserConsole { driver: d.clone() };
        let wait = BrowserWait { driver: d.clone() };
        let find = BrowserFind { driver: d };
        assert_eq!(console.name(), "browser_console");
        assert_eq!(wait.name(), "browser_wait");
        assert_eq!(find.name(), "browser_find");
        assert!(console.is_read_only());
        assert!(wait.is_read_only());
        assert!(find.is_read_only());
        // Discoverable descriptions: say when to use them.
        assert!(console.description().contains("since the last navigation"));
        assert!(wait.description().contains("before screenshotting"));
        assert!(find.description().contains("browser_click"));
    }

    #[test]
    fn schemas_expose_the_documented_parameters() {
        let d = driver();
        let console = BrowserConsole { driver: d.clone() }.parameters_schema();
        assert_eq!(
            console["properties"]["level"]["enum"],
            json!(["all", "debug", "log", "info", "warning", "error"])
        );
        let wait = BrowserWait { driver: d.clone() }.parameters_schema();
        for key in ["selector", "hidden", "text", "network_idle", "timeout_ms"] {
            assert!(wait["properties"].get(key).is_some(), "{key}");
        }
        let find = BrowserFind { driver: d }.parameters_schema();
        for key in ["query", "within", "max"] {
            assert!(find["properties"].get(key).is_some(), "{key}");
        }
    }

    /// Argument errors never touch the browser and never launch one.
    #[tokio::test]
    async fn invalid_args_are_reported_without_launching() {
        let d = driver();
        let out = BrowserConsole { driver: d.clone() }
            .execute(ctx(), json!({ "level": "loud" }))
            .await;
        assert!(
            out.text.starts_with("error: unsupported level"),
            "{}",
            out.text
        );
        let out = BrowserWait { driver: d.clone() }
            .execute(ctx(), json!({}))
            .await;
        assert!(
            out.text.starts_with("error: nothing to wait for"),
            "{}",
            out.text
        );
        let out = BrowserFind { driver: d.clone() }
            .execute(ctx(), json!({ "within": "a\u{0}b" }))
            .await;
        assert!(out.text.contains("control characters"), "{}", out.text);
        assert!(!d.has_session("test-session").await);
    }

    /// Without an open page every tool says so instead of launching a browser.
    #[tokio::test]
    async fn tools_require_an_open_page() {
        let d = driver();
        let out = BrowserConsole { driver: d.clone() }
            .execute(ctx(), json!({}))
            .await;
        assert!(out.text.contains("call browser_open first"), "{}", out.text);
        let out = BrowserWait { driver: d.clone() }
            .execute(ctx(), json!({ "text": "x" }))
            .await;
        assert!(out.text.contains("call browser_open first"), "{}", out.text);
        let out = BrowserFind { driver: d.clone() }
            .execute(ctx(), json!({}))
            .await;
        assert!(out.text.contains("call browser_open first"), "{}", out.text);
        assert!(!d.has_session("test-session").await);
    }

    #[test]
    fn wait_probe_embeds_conditions_as_json() {
        let a = args::parse_wait(
            &json!({ "selector": ".a\"b", "text": "Low Stock", "network_idle": true }),
        )
        .unwrap();
        let js = wait_probe_js(&a);
        assert!(js.contains(r#"const sel = ".a\"b";"#), "{js}");
        assert!(js.contains(r#"const want = "low stock";"#), "{js}");
        assert!(js.contains("const idle = true"), "{js}");
        let a = args::parse_wait(&json!({ "text": "x" })).unwrap();
        let js = wait_probe_js(&a);
        assert!(js.contains("const sel = null;"), "{js}");
        assert!(js.contains("const hidden = false"), "{js}");
    }

    #[test]
    fn describe_conditions_reads_naturally() {
        let a = args::parse_wait(
            &json!({ "selector": "#s", "hidden": true, "text": "Done", "network_idle": true }),
        )
        .unwrap();
        assert_eq!(
            describe_conditions(&a),
            "'#s' hidden and text \"Done\" and network idle"
        );
        let a = args::parse_wait(&json!({ "selector": ".x" })).unwrap();
        assert_eq!(describe_conditions(&a), "'.x' visible");
    }

    #[test]
    fn find_js_embeds_query_scope_and_max() {
        let a = args::parse_find(&json!({ "query": "Save", "within": "form", "max": 7 })).unwrap();
        let js = find_js(&a);
        assert!(js.contains(r#"const query = "save";"#), "{js}");
        assert!(js.contains(r#"const within = "form";"#), "{js}");
        assert!(js.contains("const max = 7;"), "{js}");
        let a = args::parse_find(&json!({})).unwrap();
        let js = find_js(&a);
        assert!(js.contains("const query = null;"));
        assert!(js.contains("const within = null;"));
    }

    #[test]
    fn render_item_shows_role_name_selector_and_extras() {
        let item = json!({
            "role": "button", "name": "Save", "selector": "#save", "x": 10, "y": 20,
            "value": "", "disabled": true
        });
        assert_eq!(
            render_item(0, &item),
            "1. button \"Save\" -> #save (at 10,20, disabled)"
        );
        let item = json!({
            "role": "link", "name": "Inventory", "selector": "nav > a:nth-of-type(2)",
            "href": "http://localhost:4200/inventory"
        });
        assert_eq!(
            render_item(4, &item),
            "5. link \"Inventory\" -> nav > a:nth-of-type(2) (href=http://localhost:4200/inventory)"
        );
    }
}
