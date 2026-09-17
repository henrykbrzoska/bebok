# Bebok Companion (Chrome, MV3)

Unpacked Chrome extension that asks the **local Bebok engine** about the page
you are reading. No bundler, no dependencies — plain HTML/CSS/JS.

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

## Use

- Type a question, tick what to attach (URL/title, selection, page text,
  screenshot), press **Ask** (or Ctrl/Cmd+Enter).
- Right-click selected text → **Ask Bebok about selection** (prefills the
  popup with a quote).
- The answer renders in the popup; **Session … ↗** opens the raw transcript.

## How it works

- `content.js` only *reads* the page on demand (`bebok.pageContext` →
  `{ url, title, selection, metaDescription, text }`, text capped ~4000 chars).
- `background.js` (service worker) owns all engine HTTP:
  `POST /session` → `POST /session/{id}/prompt` (+ optional
  `captureVisibleTab` PNG in `images[]`) → poll `GET /session/{id}` until
  `running === false` → read the last assistant `text` parts from
  `GET /session/{id}/message`.
- Settings live in `chrome.storage.sync` (`engineUrl`, `directory`); the
  context-menu handoff uses `chrome.storage.session` (`pendingSelection`).

## Files

| File | Role |
| ---- | ---- |
| `manifest.json` | MV3 manifest (popup, worker, permissions) |
| `popup.html` / `popup.js` / `popup.css` | Ask UI |
| `content.js` | Page-context reader |
| `background.js` | Engine client + context menu |
| `options.html` / `options.js` | Engine URL + directory + Test |
| `icons/` | `B` glyph (SVG 16/48/128) |
