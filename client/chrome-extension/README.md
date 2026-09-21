# Bebok Companion (Chrome, MV3)

Unpacked Chrome extension that asks the **local Bebok engine** about the page
you are reading, and optionally lets the agent drive your browser remotely
("remote piloting"). No bundler, no dependencies — plain HTML/CSS/JS.

## Install

1. Start the engine and note the `BEBOK_READY` line:
   ```cmd
   cd engine
   cargo run -p bebok-server -- --port 8787
   ```
2. Open `chrome://extensions`, enable **Developer mode**, **Load unpacked**,
   pick this directory (`client/chrome-extension`).
3. Open the extension's **Options** and paste:
   - **Engine URL** — the full `BEBOK_READY` URL, token included
     (`http://127.0.0.1:8787?token=…`)
   - **Working directory** — the project the agent works in (`E:\bebok`)
4. Press **Test connection** (expects `OK`). Then click the toolbar icon.

> CORS: extension origins are `chrome-extension://<id>`. If the popup reports
> a CORS failure, restart the engine with the extension id allowed:
> `set BEBOK_CORS=chrome-extension://<id>` (see `engine/.../cors.rs`).

## Use — Ask Bebok (popup)

- Type a question, tick what to attach (URL/title, selection, page text,
  screenshot), press **Ask** (or Ctrl/Cmd+Enter).
- Right-click selected text → **Ask Bebok about selection** (prefills the
  popup with a quote).
- The answer renders in the popup; **Session … ↗** opens the raw transcript.

## Use — Remote piloting

Remote piloting lets the agent drive your real browser tabs. When enabled the
extension registers with the engine and listens for commands (navigate,
screenshot, evaluate, click, type, getText, getPageContext, find, wait,
history, console, tabs). All commands except `tabs` run on your active tab;
`tabs` lists tabs without switching them.

### Enable

1. Open the extension's **Options**.
2. Under **Remote piloting**, tick **Enable remote piloting**.
3. The engine must be running and the Engine URL / Working directory must be
   set and tested (see Install above).
4. The extension registers immediately on toggle and sends a heartbeat every
   5 s. The Options page shows the current status.

### How it works

```
Agent (LLM)                  Engine (Rust)                 Extension (Chrome)
     │                            │                             │
     │  navigate(url)             │                             │
     │───────────────────────────>│  POST /browser/remote/      │
     │                            │    navigate                 │
     │                            │  (queues command, waits)    │
     │                            │                             │
     │                            │  ← GET /browser/remote/     │
     │                            │      pending (long-poll)    │
     │                            │                             │
     │                            │    [{id, method, params}]   │
     │                            │────────────────────────────>│
     │                            │                             │
     │                            │                             │  → chrome.tabs.update
     │                            │                             │    chrome.scripting…
     │                            │                             │
     │                            │  ← POST /browser/remote/    │
     │                            │      result                 │
     │                            │    {id, result: {url,title}}│
     │                            │────────────────────────────>│
     │                            │                             │
     │  {result: {url, title}}    │                             │
     │<───────────────────────────│                             │
```

The pull model is required by Manifest V3 — the service worker cannot open a
listening TCP socket, so the engine cannot push commands.

### Supported commands

| Method | Params | Result |
|--------|--------|--------|
| `navigate` | `{url}` | `{url, title}` |
| `screenshot` | `{format?}` | `{data, media_type}` |
| `evaluate` | `{js}` | `{value}` |
| `click` | `{selector?, x?, y?, wait_ms?}` | `{url, title}` |
| `type` | `{selector, text, clear?, submit?}` | `{url, title}` |
| `getText` | `{selector?, max_chars?}` | `{text, url, title, truncated}` |
| `getPageContext` | `{max_chars?}` | `{url, title, text, selection, metaDescription}` |
| `find` | `{query?}` | `{elements: [{role, name, selector, x, y}]}` |
| `wait` | `{selector?, text?, timeout_ms?}` | `{met}` |
| `history` | `{action: back\|forward\|reload}` | `{url, title}` |
| `console` | `{}` | `{entries: []}` (phase 5) |
| `tabs` | `{}` (or `chrome.tabs.query` filter, e.g. `{active: true, currentWindow: true}`) | `[{id, title, url, active, windowId}]` — all tabs, no tab switching |

### Options page status messages

The `<p id="remoteStatus">` line under **Remote piloting** is static text —
it confirms your settings, **not** a live connection:

| Message | Meaning |
|---------|---------|
| `Disabled` | Checkbox unticked (`remoteEnabled === false`). |
| `Enabled but engine URL or directory is not set` | Ticked, but Engine URL or Working directory is empty — fill both in. |
| `Enabled — extension will register on the next heartbeat` | Ticked + URL + directory set. The extension registers immediately on toggle and then heartbeats every 5 s. If the engine sees no heartbeat, the extension's service worker is asleep (see "Service worker falls asleep" below). |

### Desktop mode: the Engine URL goes stale on every restart

When the engine runs as the Tauri desktop sidecar it listens on a **random
port** (`--port 0`) with a **fresh token per launch**. After every engine
restart you must:

1. Copy the new full `BEBOK_READY` URL **including `?token=…`**.
2. Paste it into **Options → Engine URL**.
3. Press **Test connection** (expects `OK`).
4. Untick + retick **Enable remote piloting** to force `POST /browser/register`.

A stale URL fails silently with 401 — the Options status text stays green
(see above) while the engine rejects every register/heartbeat.

### Service worker falls asleep (MV3)

The heartbeat runs on `setInterval` inside the MV3 service worker. Brave/Chrome
kills an idle worker after ~30–35 s (about 7 heartbeats), taking the heartbeat
and the long-poll loop with it. Symptom: the extension registered fine, then
`POST /browser/remote/<action>` hangs the full 30 s and returns
`503 extension did not return a result ...`. To wake it:

1. Open the extension's **Options** page (an open Options page keeps the worker alive).
2. In `brave://extensions` (or `chrome://extensions`) click reload (🔃) on Bebok Companion, **or** untick + retick **Enable remote piloting**.
3. Within ~30 s, issue the command — e.g. list tabs (see Test 6 below).

### Reload after changing extension files

The extension is loaded **unpacked** (`Load unpacked` → `client/chrome-extension`),
so code edits (e.g. a new handler in `background.js`) are not picked up
automatically. After every edit: `brave://extensions` → reload (🔃) on
Bebok Companion, then untick + retick **Enable remote piloting**.
Without the reload the old worker keeps running and new commands answer
`unknown action` or time out.

## How it works — architecture

- `content.js` only *reads* the page on demand (`bebok.pageContext` →
  `{ url, title, selection, metaDescription, text }`, text capped ~4000 chars).
- `background.js` (service worker) owns all engine HTTP:
  - **Ask path**: `POST /session` → `POST /session/{id}/prompt` (+ optional
    `captureVisibleTab` PNG in `images[]`) → poll `GET /session/{id}` until
    `running === false` → read the last assistant `text` parts from
    `GET /session/{id}/message`.
  - **Remote piloting**: register → heartbeat every 5 s → long-poll
    `GET /browser/remote/pending` → dispatch command on active tab →
    `POST /browser/remote/result`.
- Settings live in `chrome.storage.sync` (`engineUrl`, `directory`,
  `remoteEnabled`); the context-menu handoff uses `chrome.storage.session`
  (`pendingSelection`).

## Files

| File | Role |
| ---- | ---- |
| `manifest.json` | MV3 manifest (popup, worker, permissions) |
| `popup.html` / `popup.js` / `popup.css` | Ask UI |
| `content.js` | Page-context reader |
| `background.js` | Engine client, remote piloting loop, context menu |
| `options.html` / `options.js` | Engine URL + directory + remote piloting toggle |
| `icons/` | `B` glyph (SVG 16/48/128) |

## Manual testing — remote piloting

### Prerequisites

1. Engine running: `cargo run -p bebok-server -- --port 8787`
2. Extension loaded and configured (Engine URL + directory set, "Test
   connection" OK).

### Test 1 — Registration and heartbeat

1. Open **Options**, tick **Enable remote piloting**.
2. Open the extension's service worker DevTools: `chrome://extensions` →
   **service worker** link next to Bebok Companion.
3. In the Console, verify no errors after ~5 s (heartbeat fires every 5 s).
4. On the engine side, check the log for no registration errors.

### Test 2 — Navigate

In the agent session, send a prompt that triggers a `navigate` call to the
extension, or manually test from the engine's HTTP API:

```bash
# Queue a navigate command:
curl -X POST 'http://127.0.0.1:8787/browser/remote/navigate?session_id=ext&directory=/path' \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com"}' \
  -H 'Authorization: Bearer <token>'
```

The active tab in Chrome should navigate to `https://example.com`. The
response should be `{"result":{"url":"https://example.com","title":"Example Domain"}}`.

### Test 3 — Screenshot

```bash
curl -X POST 'http://127.0.0.1:8787/browser/remote/screenshot?session_id=ext&directory=/path' \
  -H 'Content-Type: application/json' \
  -d '{"format":"png"}' \
  -H 'Authorization: Bearer <token>'
```

The response should contain `{"result":{"data":"<base64…>","media_type":"image/png"}}`.

### Test 4 — Evaluate

```bash
curl -X POST 'http://127.0.0.1:8787/browser/remote/evaluate?session_id=ext&directory=/path' \
  -H 'Content-Type: application/json' \
  -d '{"js":"document.title"}' \
  -H 'Authorization: Bearer <token>'
```

Should return `{"result":{"value":"Example Domain"}}`.

### Test 5 — Full agent integration

With remote piloting enabled and the agent using `browser_open`, `browser_click`
etc., the agent should be able to interact with the user's real browser tabs
instead of the built-in Chromium.

### Test 6 — List tabs

No navigation needed — `tabs` calls `chrome.tabs.query()` directly and works
even on `chrome://` pages (where scripting-based commands fail):

```bash
curl -X POST 'http://127.0.0.1:8787/browser/remote/tabs?session_id=extension&directory=/home/rajner/bebok' \
  -H 'Content-Type: application/json' \
  -d '{}' \
  -H 'Authorization: Bearer <token>'
```

Should return all tabs of all windows:

```json
{"result":[{"id":1072510390,"title":"Bebok options",
  "url":"chrome-extension://ecpjfaioblmppkcaejjggkadfeoibbcc/options.html",
  "active":true,"windowId":1072510295}]}
```

Useful filters (passed straight to `chrome.tabs.query`):
`{"active":true,"currentWindow":true}` (active tab only),
`{"url":"https://github.com/*"}` (URL pattern). Note: `session_id=extension`
— the value the extension registers under (`registerExtension()` in
`background.js`). If the call hangs 30 s → `503`, the service worker is
asleep — wake it per "Service worker falls asleep" above and retry.
