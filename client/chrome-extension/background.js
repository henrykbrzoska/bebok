/**
 * Bebok browser extension - background script.
 *
 * Runs as an MV3 service worker in Chrome/Edge/Brave and as an event page
 * (`background.scripts`, `persistent: false`) in Firefox — the same file serves
 * both, see `manifest.firefox.json` and `scripts/build-firefox-extension.mjs`.
 * The keepalive alarm below is what brings the worker/event page back after
 * the browser suspends it.
 *
 * Owns every HTTP call to the local Bebok engine (only the worker is covered
 * by the extension's `host_permissions`). Message types served:
 *
 *   { type: 'bebok.ask', question, directory, engineUrl, screenshot? }
 *     -> { ok: true, answer, sessionID }
 *        { ok: false, error }
 *   { type: 'bebok.testConnection', engineUrl, directory, pinnedToken? }
 *     -> { ok: true, model }
 *        { ok: false, error }
 *   { type: 'bebok.searchLocal', engineUrl, directory, pinnedToken? }
 *     -> { ok: true, engineUrl }   (working URL with ?token=, ready to save)
 *        { ok: false, needsToken, error }
 *   { type: 'bebok.remoteStatus', engineUrl, directory }
 *     -> { ok: true, status }      (GET /browser/remote/status body)
 *        { ok: false, error }
 *   { type: 'bebok.discoveryStatus', directory }
 *     -> { ok: true, port, version } | { ok: false, error }
 *
 * `pinnedToken` is the user's copy of the engine's BEBOK_TOKEN env value
 * (variant 5: pinned token). When the Engine URL carries no `?token=` of its
 * own, the pinned token is appended before probing — so with a stable token
 * the search finds the engine on ANY port after a restart.
 */
'use strict';

/** Shared storage keys. */
const STORAGE_KEY_ENGINE_URL = 'engineUrl';
const STORAGE_KEY_DIRECTORY = 'directory';
const STORAGE_KEY_REMOTE_ENABLED = 'remoteEnabled';
const STORAGE_KEY_PINNED_TOKEN = 'pinnedToken';
const STORAGE_KEY_LAST_PORT = 'lastGoodPort';
// Phase 1: status/controls
const STORAGE_KEY_AUTO_DETECT_ENABLED = 'autoDetectEnabled';
const STORAGE_KEY_PORT_SCAN_RANGE = 'portScanRange';
const STORAGE_KEY_LAST_SCAN_TIME = 'lastScanTime';
const STORAGE_KEY_BEBOK_TOKEN = 'bebokToken';
const STORAGE_KEY_FIXED_PORT = 'fixedPort';
const STORAGE_KEY_USE_FIXED_PORT = 'useFixedPort';

/**
 * Engine base URL helpers.
 *
 * `path` may carry its own query (`'/config?directory=x'`): the pathname part
 * is appended to the base (keeping any `?token=` the base already has) and the
 * query is merged into the URL's search params. Assigning the whole string to
 * `url.pathname` would percent-encode the `?` into `%3F` and 404.
 */
function buildEngineUrl(base, path) {
  const trimmed = String(base || '').trim();
  const url = new URL(trimmed);
  const [pathname, search] = String(path).split('?');
  url.pathname = url.pathname.replace(/\/+$/, '') + pathname;
  if (search) {
    for (const [key, value] of new URLSearchParams(search)) {
      url.searchParams.set(key, value);
    }
  }
  return url.toString();
}

/**
 * Promise wrapper around callback-style extension APIs.
 *
 * Chrome MV3 returns a promise when the callback is omitted; Firefox's
 * `chrome` namespace is callback-based and only settles the call you hand a
 * callback to. Passing a callback every time works on both, so every awaited
 * API call in this file goes through here. Rejects with `runtime.lastError`
 * (the extension equivalent of an HTTP error).
 */
function callApi(fn, ...args) {
  return new Promise((resolve, reject) => {
    let settled = false;
    const done = (value) => {
      if (settled) return;
      settled = true;
      const err = chrome.runtime && chrome.runtime.lastError;
      if (err) reject(new Error(err.message || String(err)));
      else resolve(value);
    };
    const returned = fn.apply(null, args.concat([done]));
    if (returned && typeof returned.then === 'function') {
      returned.then(done, (err) => {
        if (settled) return;
        settled = true;
        reject(err);
      });
    }
  });
}

/** Try to parse and read JSON from a fetch response. */
async function readJsonSafe(res) {
  try {
    return await res.json();
  } catch (_) {
    return null;
  }
}

/** Throw if response is not ok. */
async function throwUnlessOk(res, msg) {
  if (!res.ok) {
    throw new Error(`${msg}: ${res.status} ${res.statusText}`);
  }
}

/**
 * POST `{id, session_id, result?|error?}` to the engine.
 * Silently swallows network errors — the engine will time out.
 */
async function submitResult(id, sessionId, payload) {
  if (!_engineUrl) return;
  try {
    const url = buildEngineUrl(_engineUrl, '/browser/remote/result');
    await fetch(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id, session_id: sessionId, ...payload }),
    });
  } catch (_) {
    // Best-effort; engine timeout is the safety net.
  }
}

/**
 * GET `/browser/remote/pending` with a 30 s fetch timeout (engine holds
 * the connection for up to 25 s).
 */
async function fetchPendingCommands() {
  const url = buildEngineUrl(_engineUrl, '/browser/remote/pending');
  const res = await fetch(url, { signal: AbortSignal.timeout(30_000) });
  await throwUnlessOk(res, 'GET /browser/remote/pending');
  const body = await readJsonSafe(res);
  return (body && body.commands) || [];
}

// ── Command handlers ─────────────────────────────────────────────────────

/**
 * Resolve the active tab in the last-focused window.
 * Returns `{tabId, url, title}` or throws.
 */
async function requireActiveTab() {
  const [tab] = await callApi(chrome.tabs.query, {
    active: true,
    lastFocusedWindow: true,
  });
  if (!tab || !tab.id) throw new Error('No active tab');
  return { tabId: tab.id, url: tab.url || '', title: tab.title || '' };
}

// Strip unexpected keys (e.g. bare strings) so chrome.tabs.query never throws.
function normalizeTabsParams(params) {
  if (!params || typeof params !== 'object' || Array.isArray(params)) return {};
  const out = { ...params };
  if ('query' in out && (typeof out.query !== 'object' || out.query === null || Array.isArray(out.query)))
    delete out.query;
  return out;
}

const handlers = {
  // ── Navigation ──────────────────────────────────────────────────────

  navigate: async (params) => {
    const { tabId } = await requireActiveTab();
    const url = params.url;
    if (!url) throw new Error('navigate: missing url');

    // chrome.tabs.update navigates and returns the tab.
    const updated = await callApi(chrome.tabs.update, tabId, { url });

    // Wait for the page to finish loading.
    await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        chrome.tabs.onUpdated.removeListener(listener);
        reject(new Error('navigate: load timeout'));
      }, 15_000);
      function listener(id, info) {
        if (id === updated.id && info.status === 'complete') {
          clearTimeout(timeout);
          chrome.tabs.onUpdated.removeListener(listener);
          resolve();
        }
      }
      chrome.tabs.onUpdated.addListener(listener);
    });

    // Re-read tab metadata after load.
    const tab = await callApi(chrome.tabs.get, updated.id);
    return { url: tab.url || '', title: tab.title || '' };
  },

  // ── Screenshots ─────────────────────────────────────────────────────

  screenshot: async (params) => {
    const format = params.format === 'jpeg' ? 'jpeg' : 'png';
    const opts = { format };
    const dataUrl = await callApi(chrome.tabs.captureVisibleTab, null, opts);
    const prefix = 'data:image/' + format + ';base64,';
    const data =
      typeof dataUrl === 'string' && dataUrl.startsWith(prefix)
        ? dataUrl.slice(prefix.length)
        : '';
    const mediaType = 'image/' + format;
    return { data, media_type: mediaType };
  },

  // ── JavaScript evaluation ───────────────────────────────────────────

  evaluate: async (params) => {
    const { tabId } = await requireActiveTab();
    const js = params.js;
    if (!js) throw new Error('evaluate: missing js');

    const results = await callApi(chrome.scripting.executeScript, {
      target: { tabId },
      func: (code) => {
        try {
          return { value: eval(code) };
        } catch (err) {
          return { error: String(err) };
        }
      },
      args: [js],
    });

    if (!results || !results.length) {
      throw new Error('evaluate: no result');
    }
    const outcome = results[0].result;
    if (outcome && outcome.error) throw new Error(outcome.error);
    return outcome || { value: null };
  },

  // ── Input ───────────────────────────────────────────────────────────

  click: async (params) => {
    const { tabId, url, title } = await requireActiveTab();

    await callApi(chrome.scripting.executeScript, {
      target: { tabId },
      func: (selector, x, y) => {
        if (selector) {
          const el = document.querySelector(selector);
          if (!el) throw new Error('click: element not found: ' + selector);
          el.scrollIntoView({ block: 'center', inline: 'center' });
          el.click();
        } else if (x !== undefined && y !== undefined) {
          const el = document.elementFromPoint(x, y);
          if (!el) throw new Error('click: no element at (' + x + ', ' + y + ')');
          el.click();
        } else {
          throw new Error('click: provide selector or x,y');
        }
      },
      args: [params.selector || null, params.x, params.y],
    });

    if (params.wait_ms) {
      await new Promise((r) => setTimeout(r, params.wait_ms));
    } else {
      await new Promise((r) => setTimeout(r, 150));
    }

    return { url, title };
  },

  type: async (params) => {
    const { tabId, url, title } = await requireActiveTab();
    const selector = params.selector;
    const text = params.text || '';
    if (!selector) throw new Error('type: missing selector');

    await callApi(chrome.scripting.executeScript, {
      target: { tabId },
      func: (sel, txt, clear, submit) => {
        const el = document.querySelector(sel);
        if (!el) throw new Error('type: element not found: ' + sel);
        el.focus();
        if (clear) {
          el.value = '';
          el.dispatchEvent(new Event('input', { bubbles: true }));
          el.dispatchEvent(new Event('change', { bubbles: true }));
        }
        el.value = (el.value || '') + txt;
        el.dispatchEvent(new Event('input', { bubbles: true }));
        el.dispatchEvent(new Event('change', { bubbles: true }));
        if (submit) {
          el.dispatchEvent(
            new KeyboardEvent('keydown', {
              key: 'Enter',
              code: 'Enter',
              keyCode: 13,
              bubbles: true,
            })
          );
        }
      },
      args: [selector, text, Boolean(params.clear), Boolean(params.submit)],
    });

    await new Promise((r) => setTimeout(r, 100));
    return { url, title };
  },

  // ── Text extraction ─────────────────────────────────────────────────

  getText: async (params) => {
    const { tabId, url, title } = await requireActiveTab();

    const results = await callApi(chrome.scripting.executeScript, {
      target: { tabId },
      func: (selector, maxChars) => {
        const el = selector
          ? document.querySelector(selector)
          : document.body;
        if (!el) return { text: '', truncated: false };
        let text = el.innerText || '';
        const truncated = text.length > maxChars;
        if (truncated) text = text.slice(0, maxChars);
        return { text, truncated };
      },
      args: [params.selector || null, params.max_chars || 20000],
    });

    const r = results && results[0] ? results[0].result : { text: '', truncated: false };
    return { text: r.text, url, title, truncated: r.truncated };
  },

  getPageContext: async (params) => {
    const { tabId } = await requireActiveTab();

    const results = await callApi(chrome.scripting.executeScript, {
      target: { tabId },
      func: (maxChars) => {
        const NOISE =
          'script,style,noscript,svg,canvas,template,nav,header,footer,aside,form,iframe,[aria-hidden],[hidden]';
        const CONTAINERS =
          'article,main,[role="main"],#content,#main,.markdown-body,.content';

        const clean = (root) => {
          if (!root) return '';
          const c = root.cloneNode(true);
          c.querySelectorAll(NOISE).forEach((n) => n.remove());
          return (c.textContent || '').replace(/\s+/g, ' ').trim();
        };

        let text = '';
        for (const s of CONTAINERS.split(',')) {
          const t = clean(document.querySelector(s));
          if (t.length >= 200) {
            text = t;
            break;
          }
        }
        if (!text) text = clean(document.body);
        text = text.slice(0, maxChars);

        return {
          url: location.href,
          title: document.title,
          text,
          truncated: text.length >= maxChars,
          selection: (window.getSelection() || '').toString().trim(),
          metaDescription:
            (document.querySelector('meta[name="description"]') || {}).content || '',
        };
      },
      args: [params.max_chars || 4000],
    });

    return results && results[0] ? results[0].result : { url: '', title: '', text: '' };
  },

  // ── History ─────────────────────────────────────────────────────────

  history: async (params) => {
    const { tabId } = await requireActiveTab();
    const action = params.action;

    if (action === 'reload') {
      await callApi(chrome.tabs.reload, tabId);
    } else if (action === 'back') {
      await callApi(chrome.tabs.goBack, tabId);
    } else if (action === 'forward') {
      await callApi(chrome.tabs.goForward, tabId);
    } else {
      throw new Error('history: unknown action: ' + action);
    }

    await new Promise((r) => setTimeout(r, 500));

    const tab = await callApi(chrome.tabs.get, tabId);
    return { url: tab.url || '', title: tab.title || '' };
  },

  // ── Tabs ──────────────────────────────────────────────

  tabs: async (params) => {
    let q;
    try {
      q = normalizeTabsParams(params);
      const tabs = await callApi(chrome.tabs.query, q);
      return tabs.map((t) => ({
        id: t.id,
        title: t.title || '',
        url: t.url || '',
        active: t.active || false,
        windowId: t.windowId,
      }));
    } catch (err) {
      if (String(err?.message || err).includes('No matching signature')) {
        const tabs = await callApi(chrome.tabs.query, {});
        return tabs.map((t) => ({
          id: t.id,
          title: t.title || '',
          url: t.url || '',
          active: t.active || false,
          windowId: t.windowId,
        }));
      }
      throw err;
    }
  },

  // Activate another tab so every subsequent command (requireActiveTab)
  // targets it. Accepts `tab_id` / `tabId` / `id` as a number or numeric
  // string — ids come from the `tabs` action.
  switchTab: async (params) => {
    const raw = params.tab_id ?? params.tabId ?? params.id;
    const tabId = Number(raw);
    if (!Number.isInteger(tabId)) {
      throw new Error('switchTab: missing or invalid tab_id (get ids from tabs)');
    }

    let tab;
    try {
      // {active:true} switches to the tab; focus its window as well so the
      // user sees the switch when the tab lives in another window.
      tab = await callApi(chrome.tabs.update, tabId, { active: true });
    } catch (err) {
      const msg = String(err?.message || err);
      if (/no tab with id|invalid tab id/i.test(msg)) {
        throw new Error('switchTab: no tab with id ' + tabId + ' — list tabs first');
      }
      throw err;
    }

    if (tab && tab.windowId != null) {
      try {
        await callApi(chrome.windows.update, tab.windowId, { focused: true });
      } catch (_) {
        // Focusing the window is best-effort; the tab is active either way.
      }
    }

    // Re-read metadata: url/title are only populated after the update.
    const fresh = tab && tab.id != null ? await callApi(chrome.tabs.get, tab.id) : tab;
    return {
      id: fresh.id,
      title: fresh.title || '',
      url: fresh.url || '',
      active: fresh.active || false,
      windowId: fresh.windowId,
    };
  },

  // ── Element discovery ───────────────────────────────────────────────

  find: async (params) => {
    const { tabId } = await requireActiveTab();

    const results = await callApi(chrome.scripting.executeScript, {
      target: { tabId },
      func: (query) => {
        const interactive =
          'a,button,input,textarea,select,[role="button"],[role="link"],[onclick]';
        const elements = [...document.querySelectorAll(interactive)];
        const visible = elements.filter((el) => {
          const r = el.getBoundingClientRect();
          return (
            r.width > 0 &&
            r.height > 0 &&
            getComputedStyle(el).visibility !== 'hidden' &&
            getComputedStyle(el).display !== 'none'
          );
        });
        return visible.map((el) => {
          const r = el.getBoundingClientRect();
          const name =
            el.getAttribute('aria-label') ||
            el.textContent.trim().slice(0, 50) ||
            el.tagName;
          const tag = el.tagName.toLowerCase();
          const id = el.id ? '#' + el.id : '';
          const cls =
            el.className && typeof el.className === 'string'
              ? '.' + el.className.trim().split(/\s+/).join('.')
              : '';
          return {
            role: tag,
            name,
            selector: tag + id + cls,
            x: Math.round(r.x + r.width / 2),
            y: Math.round(r.y + r.height / 2),
          };
        }).filter(
          (e) => !query || e.name.toLowerCase().includes(query.toLowerCase())
        );
      },
      args: [params.query || null],
    });

    return { elements: results && results[0] ? results[0].result : [] };
  },

  // ── Waiting ─────────────────────────────────────────────────────────

  wait: async (params) => {
    const { tabId } = await requireActiveTab();
    const deadline = Date.now() + (params.timeout_ms || 5000);

    while (Date.now() < deadline) {
      const results = await callApi(chrome.scripting.executeScript, {
        target: { tabId },
        func: (selector, text) => {
          if (selector && !document.querySelector(selector)) return false;
          if (text && !document.body.innerText.includes(text)) return false;
          return true;
        },
        args: [params.selector || null, params.text || null],
      });
      const met = results && results[0] ? results[0].result : false;
      if (met) return { met: true };
      await new Promise((r) => setTimeout(r, 200));
    }
    return { met: false };
  },

  // ── Console (placeholder — requires chrome.debugger in phase 5) ─────

  console: async (_params) => {
    return { entries: [] };
  },
};

// ── Command dispatch ─────────────────────────────────────────────────────

async function dispatchCommand(cmd) {
  const handler = handlers[cmd.method];
  if (!handler) {
    return { error: 'unknown method: ' + cmd.method };
  }
  try {
    const result = await handler(cmd.params || {});
    return { result };
  } catch (err) {
    return { error: String(err.message || err) };
  }
}

// ── Polling loop ─────────────────────────────────────────────────────────

async function startPollingLoop() {
  if (_pollingActive) return;
  _pollingActive = true;
  let backoff = POLL_BACKOFF_BASE_MS;

  while (_pollingActive) {
    if (!_engineUrl) {
      await new Promise((r) => setTimeout(r, 1000));
      continue;
    }
    try {
      const commands = await fetchPendingCommands();
      backoff = POLL_BACKOFF_BASE_MS;

      for (const cmd of commands) {
        try {
          const outcome = await dispatchCommand(cmd);
          await submitResult(
            cmd.id,
            cmd.session_id || '',
            outcome.error ? { error: outcome.error } : { result: outcome.result }
          );
        } catch (err) {
          await submitResult(
            cmd.id,
            cmd.session_id || '',
            { error: 'dispatch failed: ' + String(err.message || err) }
          );
        }
      }
    } catch (err) {
      console.warn('[bebok] polling error:', err.message || err);
      await new Promise((r) => setTimeout(r, backoff));
      backoff = Math.min(backoff * 2, POLL_BACKOFF_MAX_MS);
    }
  }
}

function stopPollingLoop() {
  _pollingActive = false;
}

// ── Registration + heartbeat ─────────────────────────────────────────────

async function registerExtension() {
  if (!_engineUrl || !_directory) return;
  try {
    const urlObj = new URL(buildEngineUrl(_engineUrl, '/browser/register'));
    urlObj.searchParams.set('session_id', 'extension');
    urlObj.searchParams.set('directory', _directory);
    await fetch(urlObj.toString(), { method: 'POST' });
  } catch (_) {
    // Best-effort; heartbeat will retry.
  }
}

async function sendHeartbeat() {
  if (!_engineUrl) {
    maybeAutoRecover('no-url');
    return;
  }
  try {
    const urlObj = new URL(buildEngineUrl(_engineUrl, '/browser/heartbeat'));
    urlObj.searchParams.set('session_id', 'extension');
    if (_directory) urlObj.searchParams.set('directory', _directory);
    const response = await fetch(urlObj.toString(), { method: 'POST' });
    if (response.status === 401) {
      _lastBeatAt = 0;
      _hbFailures += 1;
      _netFailStreak = 0;
      updateBadge('error');
      return;
    }
    await throwUnlessOk(response, 'POST /browser/heartbeat');
    _lastBeatAt = Date.now();
    _hbFailures = 0;
    _netFailStreak = 0;
    updateBadge('');
  } catch (_) {
    _hbFailures += 1;
    _netFailStreak += 1;
    updateBadge('!');
    if (_netFailStreak >= RECOVER_AFTER_FAILS) {
      _netFailStreak = 0;
      maybeAutoRecover('unreachable');
    }
  }
}

/** `action` (MV3) with a `browserAction` (MV2-style) fallback for Firefox. */
function actionApi() {
  if (chrome.action) return chrome.action;
  if (chrome.browserAction) return chrome.browserAction;
  return null;
}

function updateBadge(text) {
  const api = actionApi();
  if (api && api.setBadgeText) {
    api.setBadgeText({ text });
  }
}

async function maybeAutoRecover(reason) {
  try {
    if (Date.now() < _autoRecoverUntil) return;
    _autoRecoverUntil = Date.now() + RECOVER_COOLDOWN_MS;
    const stored = await readStorage();
    if (!stored.remoteEnabled) return;
    if (!stored.directory) return;
    const token = _pinnedToken || stored.pinnedToken || '';
    const result = await handleSearchLocal({
      engineUrl: stored.engineUrl || _engineUrl,
      directory: stored.directory,
      pinnedToken: token,
      preferPort: stored.lastPort || undefined,
    });
    if (result && result.ok && result.engineUrl) {
      _engineUrl = result.engineUrl;
      _directory = stored.directory;
      _pinnedToken = token;
      try {
        const port = new URL(result.engineUrl).port;
        _lastGoodPort = port ? Number(port) : 0;
      } catch (_) { /* keep previous */ }
      saveKey(STORAGE_KEY_ENGINE_URL, result.engineUrl);
      if (_lastGoodPort) saveKey(STORAGE_KEY_LAST_PORT, _lastGoodPort);
      _hbFailures = 0;
      _netFailStreak = 0;
      _lastBeatAt = Date.now();
      try {
        await registerExtension();
      } catch (_) { /* heartbeat will retry */ }
      console.info(`[bebok] auto-recovered engine (${reason}): ${result.engineUrl}`);
    }
  } catch (_) {
    // Best-effort; next cooldown window retries.
  }
}

let _heartbeatInterval = null;

function startHeartbeat() {
  if (_heartbeatInterval) return;
  sendHeartbeat();
  _heartbeatInterval = setInterval(sendHeartbeat, HEARTBEAT_INTERVAL_MS);
}

function stopHeartbeat() {
  if (_heartbeatInterval) {
    clearInterval(_heartbeatInterval);
    _heartbeatInterval = null;
  }
}

// ── Keepalive (Firefox event page / MV3 service worker suspension) ────────
//
// Both Firefox event pages and Chrome service workers get suspended when the
// browser decides the extension is idle, which silently kills the heartbeat
// and the long-poll loop. A periodic alarm wakes the script again; the init
// block at the bottom then re-reads storage and resumes remote piloting.

const KEEPALIVE_ALARM = 'bebok.keepalive';

function startKeepaliveAlarm() {
  if (!chrome.alarms) return;
  try {
    chrome.alarms.create(KEEPALIVE_ALARM, { periodInMinutes: 1 });
  } catch (_) {
    // Alarms are a safety net; the in-memory loop still runs while awake.
  }
}

function stopKeepaliveAlarm() {
  if (!chrome.alarms) return;
  try {
    chrome.alarms.clear(KEEPALIVE_ALARM);
  } catch (_) {
    // ignore
  }
}

if (chrome.alarms && chrome.alarms.onAlarm) {
  chrome.alarms.onAlarm.addListener((alarm) => {
    if (!alarm || alarm.name !== KEEPALIVE_ALARM) return;
    if (!_remoteEnabled) return;
    // This tick may have started the worker from scratch; resume the loops.
    startHeartbeat();
    startPollingLoop();
  });
}

// ── Engine discovery (Phase 1) ────────────────────────────────────────────

async function probeVersion(port, token) {
  try {
    const url = new URL(`http://127.0.0.1:${port}/version`);
    if (token) url.searchParams.set('token', token);
    const res = await fetch(url.toString(), { signal: AbortSignal.timeout(1000) });
    if (!res.ok) return null;
    return await res.json();
  } catch (_) {
    return null;
  }
}

async function scanPorts(rangeStart, rangeEnd, token) {
  for (let p = rangeStart; p <= rangeEnd; p++) {
    const info = await probeVersion(p, token);
    if (info) {
      return { port: p, version: info.version || 'unknown' };
    }
  }
  return null;
}

async function handleSearchLocal(payload) {
  const { engineUrl, directory, pinnedToken, preferPort } = payload || {};
  const url = engineUrl || 'http://127.0.0.1:8787';
  const u = new URL(url);
  const port = u.port ? Number(u.port) : 8787;

  const tryOne = async (tryPort, tryToken) => {
    try {
      const u2 = new URL(url);
      u2.port = tryPort;
      const finalUrl = u2.toString();
      const res = await fetch(buildEngineUrl(finalUrl, '/config?directory=' + encodeURIComponent(directory)), {
        headers: {
          'Content-Type': 'application/json',
          'Authorization': tryToken ? 'Bearer ' + tryToken : '',
        },
        signal: AbortSignal.timeout(1000),
      });
      if (res.ok) {
        // Keep whatever `?token=` the URL already carried; only append the
        // pinned token when there is one (an empty `&token=` would shadow
        // the real token on the next request).
        let out = finalUrl;
        if (tryToken && !out.includes('token=')) {
          out += (out.includes('?') ? '&' : '?') + 'token=' + encodeURIComponent(tryToken);
        }
        return { ok: true, engineUrl: out };
      }
      if (res.status === 401) {
        return { ok: false, needsToken: true, error: 'Engine found, token needed.' };
      }
      return { ok: false, error: `HTTP ${res.status}` };
    } catch (err) {
      return { ok: false, error: String(err.message || err) };
    }
  };

  if (preferPort) {
    const res = await tryOne(preferPort, pinnedToken);
    if (res.ok) return res;
  }

  const res = await tryOne(port, pinnedToken);
  if (res.ok) return res;

  // Phase 1: scan range
  const autoDetect = await new Promise((resolve) => {
    chrome.storage.sync.get({ [STORAGE_KEY_AUTO_DETECT_ENABLED]: true }, (vals) => {
      resolve(vals[STORAGE_KEY_AUTO_DETECT_ENABLED]);
    });
  });
  if (autoDetect !== false) {
    const range = await new Promise((resolve) => {
      chrome.storage.sync.get({ [STORAGE_KEY_PORT_SCAN_RANGE]: [8780, 8790] }, (vals) => {
        resolve(vals[STORAGE_KEY_PORT_SCAN_RANGE]);
      });
    });
    const start = range[0] || 8780;
    const end = range[1] || 8790;
    for (let p = start; p <= end; p++) {
      if (p === port || p === preferPort) continue;
      const scanRes = await tryOne(p, pinnedToken);
      if (scanRes.ok) {
        saveKey(STORAGE_KEY_LAST_PORT, p);
        return scanRes;
      }
    }
  }

  return res;
}

async function handleDiscoveryStatus(payload) {
  const { directory } = payload || {};
  if (!directory) return { ok: false, error: 'Working directory is not set' };

  const stored = await readStorage();
  const token =
    new URLSearchParams(
      String(stored.engineUrl || '').split('?')[1] || ''
    ).get('token') ||
    stored.pinnedToken ||
    '';

  const range = await new Promise((resolve) => {
    chrome.storage.sync.get({ [STORAGE_KEY_PORT_SCAN_RANGE]: [8780, 8790] }, (vals) => {
      resolve(vals[STORAGE_KEY_PORT_SCAN_RANGE]);
    });
  });
  const result = await scanPorts(range[0] || 8780, range[1] || 8790, token);

  chrome.storage.sync.set({ [STORAGE_KEY_LAST_SCAN_TIME]: Date.now() });

  if (result) {
    saveKey(STORAGE_KEY_LAST_PORT, result.port);
    return { ok: true, port: result.port, version: result.version };
  }
  return { ok: false, error: 'No engine found in scan range' };
}

async function handleSetEnginePort(payload) {
  const { port } = payload || {};
  if (!port) return { ok: false, error: 'Port required' };

  await chrome.storage.sync.set({ [STORAGE_KEY_LAST_PORT]: port });
  return { ok: true };
}

// ── Shared state ─────────────────────────────────────────────────────────

let _engineUrl = '';
let _directory = '';
let _pinnedToken = '';
let _lastGoodPort = 0;
let _remoteEnabled = false;

const HEARTBEAT_INTERVAL_MS = 5_000;
const POLL_BACKOFF_BASE_MS = 1_000;
const POLL_BACKOFF_MAX_MS = 30_000;
let _netFailStreak = 0;
const RECOVER_AFTER_FAILS = 3;
const RECOVER_COOLDOWN_MS = 60_000;
let _autoRecoverUntil = 0;

let _pollingActive = false;
let _hbFailures = 0;
let _lastBeatAt = 0;

function readStorage() {
  return new Promise((resolve) => {
    if (!chrome.storage || !chrome.storage.sync) {
      resolve({ engineUrl: '', directory: '', remoteEnabled: false, pinnedToken: '' });
      return;
    }
    chrome.storage.sync.get(
      {
        [STORAGE_KEY_ENGINE_URL]: '',
        [STORAGE_KEY_DIRECTORY]: '',
        [STORAGE_KEY_REMOTE_ENABLED]: false,
        [STORAGE_KEY_PINNED_TOKEN]: '',
        [STORAGE_KEY_LAST_PORT]: 0,
      },
      (values) => {
        resolve({
          engineUrl: values[STORAGE_KEY_ENGINE_URL] || '',
          directory: values[STORAGE_KEY_DIRECTORY] || '',
          remoteEnabled: Boolean(values[STORAGE_KEY_REMOTE_ENABLED]),
          pinnedToken: values[STORAGE_KEY_PINNED_TOKEN] || '',
          lastPort: Number(values[STORAGE_KEY_LAST_PORT]) || 0,
        });
      }
    );
  });
}

function saveKey(key, value) {
  try {
    if (chrome.storage && chrome.storage.sync) {
      chrome.storage.sync.set({ [key]: value }, () => undefined);
    }
  } catch (_) {
    // ignore
  }
}

// ── Message dispatch (popup + options → worker) ──────────────────────────

/** Normalise an exception to a message the Options page can match on. */
function errorMessage(err) {
  if (err && (err.name === 'TimeoutError' || err.name === 'AbortError')) {
    return 'Failed to fetch (timeout)';
  }
  return String((err && err.message) || err || 'unknown error');
}

/** Keep the base URL's own `?token=`, else fall back to the pinned token. */
function withToken(base, pinnedToken) {
  try {
    const url = new URL(base);
    if (!url.searchParams.get('token') && pinnedToken) {
      url.searchParams.set('token', pinnedToken);
    }
    return url.toString();
  } catch (_) {
    return base;
  }
}

/** `fetch` an engine endpoint and parse JSON (throws on non-2xx). */
async function engineFetch(base, path, init, pinnedToken) {
  const url = withToken(buildEngineUrl(base, path), pinnedToken || '');
  const res = await fetch(url, {
    signal: AbortSignal.timeout(20_000),
    ...(init || {}),
  });
  if (!res.ok) {
    const err = new Error(`HTTP ${res.status} ${res.statusText}`.trim());
    err.status = res.status;
    throw err;
  }
  return res.json().catch(() => null);
}

/** `bebok.testConnection` -> `{ ok, model }`. */
async function handleTestConnection(payload) {
  const { engineUrl, directory, pinnedToken } = payload || {};
  const base = String(engineUrl || '').trim() || 'http://127.0.0.1:8787';
  try {
    const body = await engineFetch(
      base,
      '/config?directory=' + encodeURIComponent(directory || ''),
      {},
      pinnedToken || ''
    );
    const model = (body && body.config && body.config.model) || '';
    return { ok: true, model };
  } catch (err) {
    return { ok: false, error: errorMessage(err) };
  }
}

/** `bebok.remoteStatus` -> `{ ok, status }` (live /browser/remote/status). */
async function handleRemoteStatus(payload) {
  const { engineUrl, pinnedToken } = payload || {};
  const stored = await readStorage();
  const base =
    String(engineUrl || '').trim() ||
    String(stored.engineUrl || '').trim() ||
    'http://127.0.0.1:8787';
  const token = pinnedToken || stored.pinnedToken || '';
  try {
    const status = await engineFetch(
      base,
      '/browser/remote/status?session_id=extension',
      {},
      token
    );
    return { ok: true, status };
  } catch (err) {
    return { ok: false, error: errorMessage(err) };
  }
}

/**
 * `bebok.ask` -> `{ ok, answer, sessionID }`.
 *
 * Ask path per README: `POST /session` -> `POST /session/{id}/prompt`
 * (+ optional `captureVisibleTab` PNG in `images[]`) -> poll
 * `GET /session/{id}` until `running === false` -> last assistant `text`
 * parts from `GET /session/{id}/message`.
 */
async function handleAsk(payload) {
  const { question, directory, engineUrl, screenshot } = payload || {};
  const stored = await readStorage();
  const base =
    String(engineUrl || '').trim() ||
    String(stored.engineUrl || '').trim() ||
    'http://127.0.0.1:8787';
  const dir = String(directory || '').trim() || String(stored.directory || '').trim();
  const token = stored.pinnedToken || '';
  if (!question || !String(question).trim()) {
    return { ok: false, error: 'Empty question.' };
  }
  if (!dir) {
    return { ok: false, error: 'Working directory is not set (Options).' };
  }

  try {
    const created = await engineFetch(
      base,
      '/session',
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ directory: dir }),
      },
      token
    );
    const sessionID = created && created.sessionID;
    if (!sessionID) {
      return { ok: false, error: 'Engine did not return a sessionID.' };
    }

    const body = { message: String(question) };
    if (screenshot) {
      try {
        const dataUrl = await callApi(chrome.tabs.captureVisibleTab, null, { format: 'png' });
        const data = String(dataUrl || '').split(',')[1];
        if (data) body.images = [{ media_type: 'image/png', data }];
      } catch (_) {
        // Screenshot is optional — the prompt still goes out.
      }
    }

    await engineFetch(
      base,
      `/session/${sessionID}/prompt`,
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      },
      token
    );

    const deadline = Date.now() + 180_000;
    let running = true;
    while (running && Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, 700));
      const meta = await engineFetch(base, `/session/${sessionID}`, {}, token);
      running = Boolean(meta && meta.running);
    }
    if (running) {
      return {
        ok: false,
        sessionID,
        error:
          'Timed out after 180 s — a permission ask may be waiting in the Bebok GUI.',
      };
    }

    const transcript = await engineFetch(
      base,
      `/session/${sessionID}/message`,
      {},
      token
    );
    const messages = (transcript && transcript.messages) || [];
    let answer = '';
    for (let i = messages.length - 1; i >= 0; i--) {
      const message = messages[i];
      if (!message || message.role !== 'assistant') continue;
      const text = (message.parts || [])
        .filter((part) => part && part.type === 'text')
        .map((part) => part.text)
        .join('');
      if (text.trim()) {
        answer = text;
        break;
      }
    }
    return { ok: true, answer, sessionID };
  } catch (err) {
    return { ok: false, error: errorMessage(err) };
  }
}

const messageHandlers = {
  'bebok.ask': handleAsk,
  'bebok.testConnection': handleTestConnection,
  'bebok.searchLocal': handleSearchLocal,
  'bebok.remoteStatus': handleRemoteStatus,
  'bebok.discoveryStatus': handleDiscoveryStatus,
  'bebok.setEnginePort': handleSetEnginePort,
};

if (chrome.runtime && chrome.runtime.onMessage) {
  chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
    const handler = message && messageHandlers[message.type];
    if (!handler) return undefined;
    Promise.resolve()
      .then(() => handler(message))
      .then(sendResponse)
      .catch((err) => sendResponse({ ok: false, error: errorMessage(err) }));
    return true; // keep the channel open for the async response
  });
}

// ── Context menu: right-click selection → popup prefill ──────────────────

function installContextMenu() {
  if (!chrome.contextMenus) return;
  try {
    chrome.contextMenus.removeAll(() => {
      chrome.contextMenus.create({
        id: 'bebok-ask-selection',
        title: 'Ask Bebok about selection',
        contexts: ['selection'],
      });
    });
  } catch (_) {
    // Menu creation is best-effort.
  }
}

if (chrome.runtime && chrome.runtime.onInstalled) {
  chrome.runtime.onInstalled.addListener(installContextMenu);
}
if (chrome.runtime && chrome.runtime.onStartup) {
  chrome.runtime.onStartup.addListener(installContextMenu);
}

if (chrome.contextMenus && chrome.contextMenus.onClicked) {
  chrome.contextMenus.onClicked.addListener((info, tab) => {
    if (info.menuItemId !== 'bebok-ask-selection') return;
    const pending = {
      text: info.selectionText || '',
      url: (tab && tab.url) || '',
      title: (tab && tab.title) || '',
    };
    if (chrome.storage) {
      // storage.session (Chrome 102+/Firefox 115+) with a local fallback.
      const area = chrome.storage.session || chrome.storage.local;
      if (area) {
        area.set({ pendingSelection: pending }, () => {
          void chrome.runtime.lastError;
        });
      }
    }
    try {
      const api = actionApi();
      if (api && api.openPopup) {
        const opened = api.openPopup();
        if (opened && typeof opened.catch === 'function') opened.catch(() => {});
      }
    } catch (_) {
      // Popup may refuse to open without a gesture — the prefill is stored.
    }
  });
}

// ── Lifecycle: respond to storage changes ────────────────────────────────

function applyRemotePilotingState(remoteEnabled) {
  _remoteEnabled = Boolean(remoteEnabled);
  if (remoteEnabled) {
    registerExtension();
    startHeartbeat();
    startPollingLoop();
    startKeepaliveAlarm();
  } else {
    stopHeartbeat();
    stopPollingLoop();
    stopKeepaliveAlarm();
  }
}

if (chrome.storage && chrome.storage.onChanged) {
  chrome.storage.onChanged.addListener((changes, area) => {
    if (area !== 'sync') return;
    if (STORAGE_KEY_ENGINE_URL in changes) {
      _engineUrl = changes[STORAGE_KEY_ENGINE_URL].newValue || '';
    }
    if (STORAGE_KEY_DIRECTORY in changes) {
      _directory = changes[STORAGE_KEY_DIRECTORY].newValue || '';
    }
    if (STORAGE_KEY_PINNED_TOKEN in changes) {
      _pinnedToken = changes[STORAGE_KEY_PINNED_TOKEN].newValue || '';
    }
    if (STORAGE_KEY_REMOTE_ENABLED in changes) {
      applyRemotePilotingState(Boolean(changes[STORAGE_KEY_REMOTE_ENABLED].newValue));
    }
  });
}

// ── Bootstrap: read storage and start if enabled ─────────────────────────

(async function initRemotePiloting() {
  try {
    const { engineUrl, directory, remoteEnabled, pinnedToken, lastPort } = await readStorage();
    _engineUrl = engineUrl;
    _directory = directory;
    _pinnedToken = pinnedToken || '';
    if (lastPort) _lastGoodPort = lastPort;
    if (remoteEnabled) {
      applyRemotePilotingState(true);
      maybeAutoRecover('startup');
    }
  } catch (_) {
    // Non-fatal; remote piloting stays disabled.
  }
})();