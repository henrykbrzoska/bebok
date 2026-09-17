/**
 * Bebok Chrome extension - background service worker.
 *
 * Owns every HTTP call to the local Bebok engine (only the worker is covered
 * by the extension's `host_permissions`). Two messages are served:
 *
 *   { type: 'bebok.ask', question, directory, engineUrl, screenshot? }
 *     -> { ok: true, answer, sessionID }
 *        { ok: false, error }
 *   { type: 'bebok.testConnection', engineUrl, directory }
 *     -> { ok: true, model }
 *        { ok: false, error }
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
