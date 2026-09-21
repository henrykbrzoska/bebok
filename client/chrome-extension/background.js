/**
 * Bebok Chrome extension - background service worker.
 *
 * Owns every HTTP call to the local Bebok engine (only the worker is covered
 * by the extension's `host_permissions`). Two message types are served:
 *
 *   { type: 'bebok.ask', question, directory, engineUrl, screenshot? }
 *     -> { ok: true, answer, sessionID }
 *        { ok: false, error }
 *   { type: 'bebok.testConnection', engineUrl, directory }
 *     -> { ok: true, model }
 *        { ok: false, error }
 *
 * Additionally runs the **remote piloting loop** (phase 2): the extension
 * registers with the engine, sends heartbeats, and long-polls for commands
 * (`navigate`, `screenshot`, `evaluate`, …) dispatched by the engine on
 * behalf of the agent.  Results are posted back so the engine can return
 * them to the tool caller.
 *
 * Also installs the "Ask Bebok about selection" context-menu entry, which
 * forwards the selected text to the popup through `chrome.storage.session`.
 *
 * Engine contract (see `engine/crates/bebok-server/src/routes/session.rs`):
 *   POST /session { directory, agent } -> { sessionID }
 *   POST /session/{id}/prompt { message, agent, images? } -> 202 { status }
 *   GET  /session/{id} -> { running, ... } (poll until running === false)
 *   GET  /session/{id}/message -> { messages: [{ role, parts }] }
 * Parts are `{ type: 'text'|'thinking'|'tool'|'usage'|'status'|'image', ... }`
 * with snake_case variants (`session::Part` in bebok-core).
 *
 * Auth: the pasted BEBOK_READY URL already carries `?token=`; for same-origin
 * `fetch` the query param is accepted as a fallback (see `auth.rs`), so the
 * worker keeps the URL untouched and sends it verbatim.
 */
'use strict';

// ═══════════════════════════════════════════════════════════════════════════
// Ask Bebok (popup)
// ═══════════════════════════════════════════════════════════════════════════

/** Message served for popup "Ask". */
const ASK_MESSAGE = 'bebok.ask';
/** Message served for options "Test connection". */
const TEST_MESSAGE = 'bebok.testConnection';
/** Key used to hand the context-menu selection to the popup. */
const PENDING_SELECTION_KEY = 'pendingSelection';

/** Delay between `GET /session/{id}` running-polls (ms). */
const POLL_INTERVAL_MS = 800;
/** Upper bound for waiting for a turn to finish (ms). */
const POLL_TIMEOUT_MS = 5 * 60 * 1000;

/**
 * Parse an engine URL and join a path onto its pathname, preserving the
 * existing query (token) and optionally adding `directory`.
 *
 * Examples:
 *   'http://127.0.0.1:8787?token=abc' + '/session'
 *   -> 'http://127.0.0.1:8787/session?token=abc'
 *   same + '/config' + 'E:\bebok'
 *   -> 'http://127.0.0.1:8787/config?token=abc&directory=E%3A%5Cbebok'
 */
function buildEngineUrl(engineUrl, path, directory) {
  const trimmed = String(engineUrl || '').trim();
  let url;
  try {
    url = new URL(trimmed);
  } catch (_) {
    throw new Error(`Invalid engine URL: ${trimmed}`);
  }
  url.pathname = url.pathname.replace(/\/+$/, '') + path;
  if (directory !== undefined && directory !== null && String(directory).trim()) {
    url.searchParams.set('directory', String(directory));
  }
  return url.toString();
}

async function readJsonSafe(response) {
  try {
    return await response.json();
  } catch (_error) {
    return null;
  }
}

/** Throw an `Error` with the engine's message when the status is not 2xx. */
async function throwUnlessOk(response, action) {
  if (response.ok) return;
  const body = await readJsonSafe(response);
  const detail =
    (body && (body.error || body.message)) ||
    `${response.status} ${response.statusText}`;
  throw new Error(`${action} failed: ${detail}`);
}

/**
 * Capture the visible tab as a PNG data payload for `PromptBody.images[]`.
 * Returns `[]` when screenshots are off or the capture fails (e.g. on
 * `chrome://` pages) - the question still goes through without it.
 */
async function captureScreenshot(enabled) {
  if (!enabled) return [];
  try {
    const dataUrl = await chrome.tabs.captureVisibleTab(undefined, {
      format: 'png',
    });
    const prefix = 'data:image/png;base64,';
    if (typeof dataUrl === 'string' && dataUrl.startsWith(prefix)) {
      return [
        {
          media_type: 'image/png',
          data: dataUrl.slice(prefix.length),
          name: 'screenshot.png',
        },
      ];
    }
  } catch (error) {
    console.warn('[bebok] screenshot skipped:', error?.message || error);
  }
  return [];
}

/** `POST /session` -> fresh session id for `directory`. */
async function createSession(engineUrl, directory) {
  const response = await fetch(buildEngineUrl(engineUrl, '/session'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ directory, agent: 'ask' }),
  });
  await throwUnlessOk(response, 'POST /session');
  const body = await readJsonSafe(response);
  if (!body || !body.sessionID) {
    throw new Error('POST /session returned no sessionID');
  }
  return body.sessionID;
}

/** `POST /session/{id}/prompt` -> start the turn (202 when busy -> Error). */
async function sendPrompt(engineUrl, sessionID, message, images) {
  const response = await fetch(
    buildEngineUrl(engineUrl, '/session/' + sessionID + '/prompt'),
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ message, agent: 'ask', images }),
    }
  );
  await throwUnlessOk(response, 'POST /session/{id}/prompt');
}

/** Wait until `GET /session/{id}` reports `running === false`. */
async function waitForTurn(engineUrl, sessionID) {
  const base = buildEngineUrl(engineUrl, `/session/${sessionID}`);
  const deadline = Date.now() + POLL_TIMEOUT_MS;
  for (;;) {
    const response = await fetch(base);
    await throwUnlessOk(response, 'GET /session/{id}');
    const meta = await readJsonSafe(response);
    if (!meta || meta.running !== true) return;
    if (Date.now() > deadline) {
      throw new Error('Timed out waiting for the answer');
    }
    await new Promise((resolve) => setTimeout(resolve, POLL_INTERVAL_MS));
  }
}

/**
 * Pull the full transcript and return the last assistant message's text
 * parts (`thinking`/`tool`/`usage`/`status` parts are skipped).
 */
async function readAnswer(engineUrl, sessionID) {
  const response = await fetch(
    buildEngineUrl(engineUrl, `/session/${sessionID}/message`)
  );
  await throwUnlessOk(response, 'GET /session/{id}/message');
  const body = await readJsonSafe(response);
  const messages = (body && body.messages) || [];
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const message = messages[i];
    if (!message || message.role !== 'assistant' || !Array.isArray(message.parts)) {
      continue;
    }
    const text = message.parts
      .filter((part) => part && part.type === 'text' && part.text)
      .map((part) => part.text)
      .join('\n\n')
      .trim();
    if (text) return text;
  }
  return '';
}

async function handleAsk(payload) {
  const { question, directory, engineUrl, screenshot } = payload || {};
  if (!question || !String(question).trim()) {
    return { ok: false, error: 'Empty question' };
  }
  if (!directory) return { ok: false, error: 'Working directory is not set' };
  if (!engineUrl) return { ok: false, error: 'Engine URL is not set' };
  try {
    const images = await captureScreenshot(Boolean(screenshot));
    const sessionID = await createSession(engineUrl, directory);
    await sendPrompt(engineUrl, sessionID, String(question), images);
    await waitForTurn(engineUrl, sessionID);
    const answer = await readAnswer(engineUrl, sessionID);
    return { ok: true, answer, sessionID };
  } catch (error) {
    return { ok: false, error: String(error?.message || error) };
  }
}

/**
 * `GET /config?directory=` is the cheapest authenticated endpoint; a 200
 * proves the URL, token and directory are all accepted.
 */
async function handleTestConnection(payload) {
  const { engineUrl, directory } = payload || {};
  if (!engineUrl) return { ok: false, error: 'Engine URL is not set' };
  if (!directory) return { ok: false, error: 'Working directory is not set' };
  try {
    const response = await fetch(buildEngineUrl(engineUrl, '/config', directory));
    await throwUnlessOk(response, 'GET /config');
    const body = await readJsonSafe(response);
    const model =
      (body && body.config && (body.config.model || body.config.effective_model)) || '';
    return { ok: true, model: String(model) };
  } catch (error) {
    return { ok: false, error: String(error?.message || error) };
  }
}

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (!message || typeof message.type !== 'string') return false;
  if (message.type === ASK_MESSAGE) {
    handleAsk(message).then(sendResponse);
    return true; // async reply
  }
  if (message.type === TEST_MESSAGE) {
    handleTestConnection(message).then(sendResponse);
    return true; // async reply
  }
  return false;
});

/**
 * Context menu: stash the selection where the popup can pick it up, then open
 * the popup. (A service worker cannot fill the popup's textarea directly, so
 * the handoff goes through `chrome.storage.session`.)
 */
chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({
    id: 'bebok-ask-selection',
    title: 'Ask Bebok about selection',
    contexts: ['selection'],
  });
});

chrome.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId !== 'bebok-ask-selection') return;
  const payload = {
    text: info.selectionText || '',
    url: (tab && tab.url) || info.pageUrl || '',
    at: Date.now(),
  };
  chrome.storage.session.set({ [PENDING_SELECTION_KEY]: payload }, () => {
    if (chrome.action && chrome.action.openPopup) {
      chrome.action.openPopup().catch(() => undefined);
    }
  });
});

// ═══════════════════════════════════════════════════════════════════════════
// Remote piloting (phase 2)
//
// Pull-based protocol:
//   1. Register  POST /browser/register  (on startup / enable)
//   2. Heartbeat POST /browser/heartbeat  (every 5 s)
//   3. Poll      GET  /browser/remote/pending  (long-poll, ~25 s timeout)
//   4. Dispatch  (local handlers on active tab)
//   5. Post back POST /browser/remote/result  {id, session_id, result|error}
//
// Engine docs: docs/chrome-extension-remote-browser.md § Faza 2
// Engine queue: engine/crates/bebok-server/src/routes/browser_remote.rs
// ═══════════════════════════════════════════════════════════════════════════

/** Delay between heartbeats (ms). */
const HEARTBEAT_INTERVAL_MS = 5_000;
/** Base delay for the polling loop back-off after an error (ms). */
const POLL_BACKOFF_BASE_MS = 1_000;
/** Maximum back-off delay (ms). */
const POLL_BACKOFF_MAX_MS = 30_000;

/** Chrome storage keys shared with options.js. */
const STORAGE_KEY_ENGINE_URL = 'engineUrl';
const STORAGE_KEY_DIRECTORY = 'directory';
const STORAGE_KEY_REMOTE_ENABLED = 'remoteEnabled';

// ── Shared state ─────────────────────────────────────────────────────────

/** Resolved engine base URL (populated from chrome.storage.sync). */
let _engineUrl = '';
/** Resolved working directory (populated from chrome.storage.sync). */
let _directory = '';

/** Set to `true` while the polling loop is running. */
let _pollingActive = false;

// ── Helpers ──────────────────────────────────────────────────────────────

/**
 * Read engineUrl / directory / remoteEnabled from chrome.storage.sync.
 * Returns `{engineUrl, directory, remoteEnabled}`.
 */
function readStorage() {
  return new Promise((resolve) => {
    if (!chrome.storage || !chrome.storage.sync) {
      resolve({ engineUrl: '', directory: '', remoteEnabled: false });
      return;
    }
    chrome.storage.sync.get(
      {
        [STORAGE_KEY_ENGINE_URL]: '',
        [STORAGE_KEY_DIRECTORY]: '',
        [STORAGE_KEY_REMOTE_ENABLED]: false,
      },
      (values) => {
        resolve({
          engineUrl: values[STORAGE_KEY_ENGINE_URL] || '',
          directory: values[STORAGE_KEY_DIRECTORY] || '',
          remoteEnabled: Boolean(values[STORAGE_KEY_REMOTE_ENABLED]),
        });
      }
    );
  });
}

/** Capture the currently visible tab area as a base64-encoded PNG. */
async function captureTabImage() {
  try {
    const dataUrl = await chrome.tabs.captureVisibleTab(null, {
      format: 'png',
    });
    const prefix = 'data:image/png;base64,';
    if (typeof dataUrl === 'string' && dataUrl.startsWith(prefix)) {
      return { data: dataUrl.slice(prefix.length), media_type: 'image/png' };
    }
  } catch (_) {
    // chrome:// pages, restricted origins — not fatal.
  }
  return { data: '', media_type: 'image/png' };
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
  const [tab] = await chrome.tabs.query({
    active: true,
    lastFocusedWindow: true,
  });
  if (!tab || !tab.id) throw new Error('No active tab');
  return { tabId: tab.id, url: tab.url || '', title: tab.title || '' };
}

const handlers = {
  // ── Navigation ──────────────────────────────────────────────────────

  navigate: async (params) => {
    const { tabId } = await requireActiveTab();
    const url = params.url;
    if (!url) throw new Error('navigate: missing url');

    // chrome.tabs.update navigates and returns the tab.
    const updated = await chrome.tabs.update(tabId, { url });

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
    const tab = await chrome.tabs.get(updated.id);
    return { url: tab.url || '', title: tab.title || '' };
  },

  // ── Screenshots ─────────────────────────────────────────────────────

  screenshot: async (params) => {
    const format = params.format === 'jpeg' ? 'jpeg' : 'png';
    const opts = { format };
    // captureVisibleTab uses the visible viewport; full_page is not natively
    // supported without CDP (phase 5).  The engine can stitch if needed.
    const dataUrl = await chrome.tabs.captureVisibleTab(null, opts);
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

    const results = await chrome.scripting.executeScript({
      target: { tabId },
      func: (code) => {
        try {
          // eslint-disable-next-line no-eval
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

    await chrome.scripting.executeScript({
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

    // Brief settle to let event handlers fire.
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

    await chrome.scripting.executeScript({
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

    const results = await chrome.scripting.executeScript({
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

    const results = await chrome.scripting.executeScript({
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
      await chrome.tabs.reload(tabId);
    } else if (action === 'back') {
      await chrome.tabs.goBack(tabId);
    } else if (action === 'forward') {
      await chrome.tabs.goForward(tabId);
    } else {
      throw new Error('history: unknown action: ' + action);
    }

    // Wait for navigation to settle.
    await new Promise((r) => setTimeout(r, 500));

    // Re-read tab after navigation.
    const tab = await chrome.tabs.get(tabId);
    return { url: tab.url || '', title: tab.title || '' };
  },

  // ── Element discovery ───────────────────────────────────────────────

  find: async (params) => {
    const { tabId } = await requireActiveTab();

    const results = await chrome.scripting.executeScript({
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
      const results = await chrome.scripting.executeScript({
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

/**
 * Dispatch a single command to the appropriate handler.
 * Returns `{result: ...}` or `{error: "..."}`.
 */
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

/**
 * Long-poll loop: fetch pending commands, dispatch each, post results.
 * Runs while `remoteEnabled` is `true` in chrome.storage.sync.
 */
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
      backoff = POLL_BACKOFF_BASE_MS; // reset on success

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

/**
 * POST /browser/register with the session ID derived from the stored
 * directory (a deterministic hash of the directory is the session key in
 * the engine).  Uses `session_id = 'extension'` as the extension's own
 * identifier — the engine maps it per-directory.
 */
async function registerExtension() {
  if (!_engineUrl || !_directory) return;
  try {
    const url = buildEngineUrl(
      _engineUrl,
      '/browser/register',
      _directory
    );
    // The engine expects session_id — use the directory as the session key
    // since the extension serves the active tab of that project.
    const urlObj = new URL(url);
    urlObj.searchParams.set('session_id', 'extension');
    await fetch(urlObj.toString(), { method: 'POST' });
  } catch (_) {
    // Best-effort; heartbeat will retry.
  }
}

async function sendHeartbeat() {
  if (!_engineUrl) return;
  try {
    const url = buildEngineUrl(_engineUrl, '/browser/heartbeat');
    const urlObj = new URL(url);
    urlObj.searchParams.set('session_id', 'extension');
    await fetch(urlObj.toString(), { method: 'POST' });
  } catch (_) {
    // Best-effort.
  }
}

let _heartbeatInterval = null;

function startHeartbeat() {
  if (_heartbeatInterval) return;
  sendHeartbeat(); // immediate first beat
  _heartbeatInterval = setInterval(sendHeartbeat, HEARTBEAT_INTERVAL_MS);
}

function stopHeartbeat() {
  if (_heartbeatInterval) {
    clearInterval(_heartbeatInterval);
    _heartbeatInterval = null;
  }
}

// ── Lifecycle: respond to storage changes ────────────────────────────────

function applyRemotePilotingState(remoteEnabled) {
  if (remoteEnabled) {
    registerExtension();
    startHeartbeat();
    startPollingLoop();
  } else {
    stopHeartbeat();
    stopPollingLoop();
  }
}

// Listen for storage changes (toggled from options page).
if (chrome.storage && chrome.storage.onChanged) {
  chrome.storage.onChanged.addListener((changes, area) => {
    if (area !== 'sync') return;
    if (STORAGE_KEY_ENGINE_URL in changes) {
      _engineUrl = changes[STORAGE_KEY_ENGINE_URL].newValue || '';
    }
    if (STORAGE_KEY_DIRECTORY in changes) {
      _directory = changes[STORAGE_KEY_DIRECTORY].newValue || '';
    }
    if (STORAGE_KEY_REMOTE_ENABLED in changes) {
      applyRemotePilotingState(Boolean(changes[STORAGE_KEY_REMOTE_ENABLED].newValue));
    }
  });
}

// ── Bootstrap: read storage and start if enabled ─────────────────────────

(async function initRemotePiloting() {
  try {
    const { engineUrl, directory, remoteEnabled } = await readStorage();
    _engineUrl = engineUrl;
    _directory = directory;
    if (remoteEnabled) {
      applyRemotePilotingState(true);
    }
  } catch (_) {
    // Non-fatal; remote piloting stays disabled.
  }
})();
