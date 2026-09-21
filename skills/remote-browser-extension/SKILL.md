---
name: remote-browser-extension
description: Drive the user's own Brave/Chrome tabs via the Bebok Companion extension (remote piloting). Use when the user asks about THEIR browser, their open tabs, or something visible on their screen — instead of the built-in headless Chromium.
---

# Remote browser extension — pilot the user's own tabs

The user runs the **Bebok Companion** MV3 extension (unpacked from
`client/chrome-extension`) in their own Brave/Chrome. When remote piloting is
enabled, you can list and drive their real tabs through the engine's pull
queue — use this whenever the user talks about *their* browser, *their* open
tabs, or asks you to check/click/type something they see.

## How it works

```
You (fetch)                    Engine (Rust)                     Extension
  │                                │                                 │
  │  POST /browser/remote/<action> │                                 │
  │  ?session_id=extension         │                                 │
  │  &directory=<project-root>     │                                 │
  │───────────────────────────────>│  queues command, waits ≤ 30 s   │
  │                                │                                 │
  │                                │  ← GET /browser/remote/pending  │
  │                                │    (extension long-polls)       │
  │                                │                                 │
  │  {result: ...}                 │  ← POST /browser/remote/result  │
  │<───────────────────────────────│                                 │
```

Auth: same engine token as everything else — `Authorization: Bearer <token>`
or `?token=`. `session_id` is literally `extension` (the value the extension
registers under in `registerExtension()`). `directory` is the project root.

## Commands (POST body is JSON params)

| Action | Params | Result |
|--------|--------|--------|
| `tabs` | `{}` (or `chrome.tabs.query` filter, e.g. `{"active":true,"currentWindow":true}`) | `[{id, title, url, active, windowId}]` — all tabs, no switching |
| `navigate` | `{"url"}` | `{url, title}` (active tab) |
| `getPageContext` | `{"max_chars?"}` | `{url, title, text, selection, metaDescription}` |
| `getText` | `{"selector?","max_chars?"}` | `{text, url, title, truncated}` |
| `click` | `{"selector?"|"x","y","wait_ms?"}` | `{url, title}` |
| `type` | `{"selector","text","clear?","submit?"}` | `{url, title}` |
| `screenshot` | `{"format?"}` | `{data, media_type}` (base64 image) |
| `evaluate` | `{"js"}` | `{value}` (sync JS only — no Promises) |
| `history` | `{"action":"back\|forward\|reload"}` | `{url, title}` |
| `find` | `{"query?"}` | `{elements:[{role,name,selector,x,y}]}` |
| `wait` | `{"selector?","text?","timeout_ms?"}` | `{met}` |
| `console` | `{}` | `{entries: []}` (stub) |

All commands except `tabs` run on the **active tab** (`requireActiveTab()`).
`tabs` calls `chrome.tabs.query()` directly and works even on `chrome://`
pages where scripting commands fail.

Example — list the user's tabs:

```json
POST /browser/remote/tabs?session_id=extension&directory=/home/rajner/bebok
{}
→ {"result":[{"id":1072510390,"title":"Bebok options",
   "url":"chrome-extension://…/options.html","active":true,"windowId":1072510295}]}
```

## Failure modes — read these before retrying

- **HTTP 503 `extension did not return a result ... within 30 s`** — the
  extension's MV3 service worker is **asleep** (Brave/Chrome kills an idle
  worker after ~30–35 s, taking the heartbeat + long-poll loop with it).
  This is NOT a page error. Tell the user: open the extension's Options page
  (keeps the worker alive), reload the extension (🔃) or untick+retick
  **Enable remote piloting**, then say "dawaj" — retry within ~30 s.
- **HTTP 404 `... not registered`** — the extension never registered (or the
  engine restarted and dropped the registry). Same wake-up steps as above.
- **HTTP 401** — stale Engine URL: in desktop mode the engine listens on a
  random port (`--port 0`) with a fresh token per launch. The user must paste
  the new full `BEBOK_READY` URL (with `?token=`) into Options → Engine URL,
  press Test connection, and retick remote piloting.
- **Options shows `Enabled — extension will register on the next heartbeat`**
  — this is static text confirming settings (checkbox + URL + directory),
  NOT a live-connection proof. The only proof is fresh `POST
  /browser/heartbeat` lines in the engine log / `GET /debug/log`.

## Rules

- Prefer 1–2 distinctive commands over chains: `tabs` first (what is open?),
  then act on the active tab. Never `navigate` the user's active tab without
  asking — it hijacks the window they are looking at.
- `tabs` with `{}` lists every window; filter with `chrome.tabs.query`
  shapes (`active`, `currentWindow`, `url` pattern like
  `"https://github.com/*"`).
- Scripting commands (`click`, `type`, `evaluate`, screenshots) fail on
  `chrome://`, `chrome-extension://` and `file://` pages — use `tabs` (always
  works) or ask the user to open a normal `http(s)` page first.
- Full setup/troubleshooting for the human lives in
  `client/chrome-extension/README.md` — point them there, don't paste it.
