# Chrome Extension jako pilot przeglądarki — plan

## Cel

Silnik Bebok widzi prawdziwą przeglądarkę użytkownika przez Chrome Extension
tak samo jak widzi wbudowany Chromium — agent może czytać DOM, pisać, klikać,
nawigować, odpalać JS, czytać konsolę, robić screenshoty, łączyć się z DevTools.

Extension nie jest już passywnym „czytaczem stron" — staje się aktywnym agentem
na polecenie silnika.

---

## Architektura docelowa

```
Agent (LLM)                    Engine (Rust)                   Extension (Chrome)
     │                              │                               │
     │  browser_open(url)           │                               │
     │──────────────────────────────>│                               │
     │                              │──WS/HTTP──────────────────────>│  → chrome.tabs.update(url)
     │                              │                               │
     │  browser_screenshot()        │                               │
     │──────────────────────────────>│                               │
     │                              │──WS/HTTP──────────────────────>│  → captureVisibleTab / CDP
     │                              │<─PNG──────────────────────────│
     │                              │                               │
     │  browser_click(selector)     │                               │
     │──────────────────────────────>│                               │
     │                              │──WS/HTTP──────────────────────>│  → inject JS: element.click()
     │                              │                               │
     │  browser_console()           │                               │
     │──────────────────────────────>│                               │
     │                              │──WS/HTTP──────────────────────>│  → chrome.debugger API
     │                              │<─console entries──────────────│
```

---

## Protokół komunikacji (engine ↔ extension)

### WebSocket / HTTP (faza MVP: HTTP, docelowo: WebSocket)

```
WS /browser/connect?token=&directory=&extension_version=1.5.0
→ { sessionID, capabilities: ["navigate","screenshot","evaluate","debugger","console","input"] }
```

### Komendy

```jsonc
// Engine → Extension
{ "id": "uuid", "method": "navigate", "params": { "url": "https://..." } }

// Extension → Engine (odpowiedź)
{ "id": "uuid", "result": { "url": "...", "title": "..." }, "error": "..." }

// Extension → Engine (asynchroniczny event)
{ "event": "console", "data": { "level": "error", "text": "..." } }
```

### Lista komend

| Metoda | Params | Wynik |
|---|---|---|
| `navigate` | `{url, wait_ms?}` | `{url, title}` |
| `screenshot` | `{full_page?, format?}` | `{data: base64, media_type}` |
| `click` | `{selector?, x?, y?, wait_ms?}` | `{url, title}` |
| `type` | `{selector, text, clear?, submit?}` | `{url, title}` |
| `evaluate` | `{js}` | `{value}` |
| `getText` | `{selector?, max_chars?}` | `{text, url, title}` |
| `find` | `{query?}` | `{elements: [...]}` |
| `console` | `{level?, max?}` | `{entries: [...]}` |
| `wait` | `{selector?, text?, timeout_ms?, network_idle?}` | `{met: bool}` |
| `history` | `{action: back\|forward\|reload}` | `{url, title}` |
| `getPageContext` | `{max_chars?}` | `{url, title, text, selection}` |

---

## Session mapping

Extension workuje z **aktywnym tabem**. Silnik workuje z **sessionID**.

Mapowanie `tabId ↔ sessionID`:
```jsonc
// background.js — przechowuje
{
  "sessionTabMap": {
    "session-uuid-1": { "tabId": 123, "directory": "/path/to/project" },
    "session-uuid-2": { "tabId": 456, "directory": "/other/project" }
  }
}
```

---

## Fazy

### Faza 1: Komunikacja engine ↔ extension

#### 1.1 HTTP endpoint w silniku

**Pliki:**
- `engine/crates/bebok-server/src/routes/browser_remote.rs` (nowy)
- `engine/crates/bebok-server/src/routes/mod.rs` (rejestracja route)

```
POST /browser/remote/{action}?directory=
```

Engine przyjmuje żądanie od agenta, forwarduje do extension po HTTP (extension
nasłuchuje na localhost), zwraca odpowiedź.

**Protokół HTTP (MVP):**
- Extension rejestruje się: `POST /browser/register?port=&session_id=`
- Engine woła: `POST http://127.0.0.1:{port}/{action}` z payloadem
- Extension odpowiada synchronicznie

#### 1.2 `RemotePage` — abstrakcja

**Plik:** `engine/crates/bebok-tools/src/browser/remote.rs` (nowy)

```rust
pub struct RemotePage {
    http_port: u16,
    session_id: String,
}

impl RemotePage {
    pub async fn goto(&self, url: &str) -> Result<(), String> { ... }
    pub async fn screenshot(&self) -> Result<Vec<u8>, String> { ... }
    pub async fn evaluate(&self, js: &str) -> Result<Value, String> { ... }
    pub async fn click(&self, target: ClickTarget) -> Result<(), String> { ... }
    pub async fn url(&self) -> Result<String, String> { ... }
    pub async fn title(&self) -> Result<String, String> { ... }
    // ... inne metody odpowiadające istniejącym toolom
}
```

#### 1.3 `BrowserPage` trait

**Plik:** `engine/crates/bebok-tools/src/browser/driver.rs`

```rust
pub trait BrowserPage: Send + Sync {
    async fn goto(&self, url: &str) -> Result<(), String>;
    async fn screenshot(&self) -> Result<Vec<u8>, String>;
    async fn evaluate(&self, js: &str) -> Result<serde_json::Value, String>;
    async fn click(&self, target: ClickTarget) -> Result<(), String>;
    async fn type_text(&self, selector: &str, text: &str, clear: bool, submit: bool) -> Result<(), String>;
    async fn url(&self) -> Result<String, String>;
    async fn title(&self) -> Result<String, String>;
    async fn inner_text(&self, selector: Option<&str>) -> Result<String, String>;
}

impl BrowserPage for LocalPage { /* chromiumoxide Page */ }
impl BrowserPage for RemotePage { /* HTTP do extension */ }
```

**Plik:** `engine/crates/bebok-tools/src/browser/tools.rs`

Tooli nie zmieniają API — `page_for()` zwraca `Box<dyn BrowserPage>`.

### Faza 2: Extension — serwer HTTP + handler komend

#### 2.1 `background.js` — serwer HTTP

Extension nasłuchuje na `127.0.0.1` na losowym porcie. Po starcie rejestruje
się w silniku.

```javascript
// background.js
const PORT_KEY = 'listeningPort';

async function startServer() {
  // chrome.sockets.tcpServer API lub
  // XMLHttpRequest loop na chrome-extension:// internal port
  // MVP: chrome.runtime.onConnect named port 'bebok-engine'
}
```

**MVP podejście (prostsze):** silnik nie woła extension — extension woła silnik.
Silnik wystawia endpoint `POST /browser/register`, extension się Rejestruje
(co 5s heartbeat), silnik trzyma ostatnio aktywny port i dispatchuje komendy
przez `fetch` do extension.

#### 2.2 `background.js` — handler komend

```javascript
const handlers = {
  'navigate': async (params) => {
    await chrome.tabs.update(activeTabId, { url: params.url });
    // czekaj na chrome.tabs.onUpdated → status === 'complete'
    return { url: finalUrl, title };
  },

  'screenshot': async (params) => {
    const dataUrl = await chrome.tabs.captureVisibleTab(null, {
      format: params.format || 'png'
    });
    return { data: dataUrl.split(',')[1], media_type: 'image/png' };
  },

  'evaluate': async (params) => {
    const [{result}] = await chrome.scripting.executeScript({
      target: { tabId: activeTabId },
      func: (js) => { return eval(js); },
      args: [params.js]
    });
    return { value: result };
  },

  'click': async (params) => {
    await chrome.scripting.executeScript({
      target: { tabId: activeTabId },
      func: (selector, x, y) => {
        if (selector) document.querySelector(selector)?.click();
        else if (x !== undefined) {
          const el = document.elementFromPoint(x, y);
          if (el) el.click();
        }
      },
      args: [params.selector, params.x, params.y]
    });
    return { url: activeTabUrl, title: activeTabTitle };
  },

  'type': async (params) => {
    await chrome.scripting.executeScript({
      target: { tabId: activeTabId },
      func: (sel, text, clear, submit) => {
        const el = document.querySelector(sel);
        if (!el) throw new Error('no element');
        el.focus();
        if (clear) { el.value = ''; el.dispatchEvent(new Event('input', {bubbles:true})); }
        el.value = (el.value || '') + text;
        el.dispatchEvent(new Event('input', {bubbles:true}));
        if (submit) el.dispatchEvent(new KeyboardEvent('keydown', {key:'Enter',code:'Enter',bubbles:true}));
      },
      args: [params.selector, params.text, params.clear, params.submit]
    });
    return { url: activeTabUrl, title: activeTabTitle };
  },

  'getText': async (params) => {
    const [{result}] = await chrome.scripting.executeScript({
      target: { tabId: activeTabId },
      func: (selector, maxChars) => {
        const el = selector ? document.querySelector(selector) : document.body;
        const text = el ? el.innerText : '';
        return text.length > maxChars ? text.slice(0, maxChars) + '…' : text;
      },
      args: [params.selector || null, params.max_chars || 20000]
    });
    return { text: result, url: activeTabUrl, title: activeTabTitle };
  },

  'find': async (params) => {
    const [{result}] = await chrome.scripting.executeScript({
      target: { tabId: activeTabId },
      func: (query) => {
        const interactive = 'a,button,input,textarea,select,[role="button"],[role="link"],[onclick]';
        const elements = [...document.querySelectorAll(interactive)];
        const visible = elements.filter(el => {
          const r = el.getBoundingClientRect();
          return r.width > 0 && r.height > 0 && getComputedStyle(el).visibility !== 'hidden';
        });
        return visible.map(el => {
          const r = el.getBoundingClientRect();
          const name = el.getAttribute('aria-label') || el.textContent?.trim().slice(0,50) || el.tagName;
          const tag = el.tagName.toLowerCase();
          const id = el.id ? '#' + el.id : '';
          const cls = el.className && typeof el.className === 'string' ? '.' + el.className.trim().split(/\s+/).join('.') : '';
          return { role: tag, name, selector: tag + id + cls, x: Math.round(r.x + r.width/2), y: Math.round(r.y + r.height/2) };
        }).filter(e => !query || e.name.toLowerCase().includes(query.toLowerCase()));
      },
      args: [params.query || null]
    });
    return { elements: result };
  },

  'console': async (params) => {
    // Wymaga chrome.debugger — patrz faza 3
    // MVP: zwraca puste (brak historii konsoli bez debuggera)
    return { entries: [] };
  },

  'wait': async (params) => {
    const deadline = Date.now() + (params.timeout_ms || 5000);
    while (Date.now() < deadline) {
      const [{result: met}] = await chrome.scripting.executeScript({
        target: { tabId: activeTabId },
        func: (selector, text, networkIdle) => {
          if (selector && !document.querySelector(selector)) return false;
          if (text && !document.body?.innerText?.includes(text)) return false;
          return true;
        },
        args: [params.selector || null, params.text || null, params.network_idle || false]
      });
      if (met) return { met: true };
      await new Promise(r => setTimeout(r, 200));
    }
    return { met: false };
  },

  'history': async (params) => {
    if (params.action === 'reload') {
      await chrome.tabs.reload(activeTabId);
    } else if (params.action === 'back') {
      await chrome.tabs.goBack(activeTabId);
    } else if (params.action === 'forward') {
      await chrome.tabs.goForward(activeTabId);
    }
    await new Promise(r => setTimeout(r, 500));
    return { url: activeTabUrl, title: activeTabTitle };
  },

  'getPageContext': async (params) => {
    const [{result}] = await chrome.scripting.executeScript({
      target: { tabId: activeTabId },
      func: (maxChars) => {
        const NOISE = 'script,style,noscript,svg,canvas,template,nav,header,footer,aside,form,iframe,[aria-hidden],[hidden]';
        const CONTAINERS = 'article,main,[role="main"],#content,#main,.markdown-body,.content';
        const clean = (root) => {
          if (!root) return '';
          const c = root.cloneNode(true);
          c.querySelectorAll(NOISE).forEach(n => n.remove());
          return (c.textContent || '').replace(/\s+/g, ' ').trim();
        };
        let text = '';
        for (const s of CONTAINERS.split(',')) {
          const t = clean(document.querySelector(s));
          if (t.length >= 200) { text = t; break; }
        }
        if (!text) text = clean(document.body);
        text = text.slice(0, maxChars || 4000);
        return {
          url: location.href,
          title: document.title,
          selection: (window.getSelection()?.toString() || '').trim(),
          metaDescription: document.querySelector('meta[name="description"]')?.content || '',
          text, truncated: text.length >= (maxChars || 4000)
        };
      },
      args: [params.max_chars || 4000]
    });
    return result;
  }
};
```

#### 2.3 `manifest.json` — nowe permisje

```jsonc
{
  "permissions": [
    "storage", "activeTab", "scripting", "contextMenus",
    "tabs",              // NOWE: query, update, create, remove
    "debugger"           // NOWE: chrome.debugger (CDP access)
  ]
}
```

### Faza 3: Silnik — integracja z istniejącymi toolami

#### 3.1 `BrowserDriver` — tryb remote

**Plik:** `engine/crates/bebok-tools/src/browser/driver.rs`

```rust
enum PageSource {
    Local {
        browser: Browser,
        page: Page,
        handler: JoinHandle<()>,
        console: SharedConsole,
        console_task: Option<JoinHandle<()>>,
    },
    Remote {
        client: RemotePageClient,  // HTTP client do extension
        session_id: String,
    },
}

struct SessionBrowser {
    source: PageSource,
    last_used: Instant,
    directory: String,
    headed: bool,
    seq: u64,
}
```

#### 3.2 Config — przełącznik

**Plik:** `engine/crates/bebok-core/src/config/loader.rs` — `apply()`

```jsonc
{
  "browser": {
    "display": "headed",
    "remote": {                   // NOWE
      "enabled": true,            // true = extension, false = lokalny Chrome
      "tab_id": null,             // auto-wybierz aktywny tab
      "extension_url": "http://127.0.0.1:XXXX"  // auto-discoverowane
    }
  }
}
```

#### 3.3 `BrowserPage` trait

**Plik:** `engine/crates/bebok-tools/src/browser/driver.rs`

```rust
#[async_trait]
pub trait BrowserPage: Send + Sync {
    async fn goto(&self, url: &str) -> Result<(), String>;
    async fn screenshot(&self) -> Result<Vec<u8>, String>;
    async fn evaluate(&self, js: &str) -> Result<serde_json::Value, String>;
    async fn click(&self, target: ClickTarget) -> Result<(), String>;
    async fn type_text(&self, selector: &str, text: &str, clear: bool, submit: bool) -> Result<(), String>;
    async fn url(&self) -> Result<String, String>;
    async fn title(&self) -> Result<String, String>;
    async fn inner_text(&self, selector: Option<&str>) -> Result<String, String>;
    async fn wait_for_navigation(&self) -> Result<(), String>;
}
```

`LocalPage` (chromiumoxide `Page`) i `RemotePage` (HTTP do extension)
implementują ten trait. Tooli nie zmieniają API.

#### 3.4 Routing tooli

```rust
// driver.rs — page() z BasedPage zamiast Page
pub async fn page(&self, session_id: &str, root: &Path) -> Result<Box<dyn BrowserPage>, String> {
    // auto-decide: remote czy local na podstawie configu
    let settings = self.settings_for(root);
    if settings.remote_enabled {
        let client = self.remote_client(session_id).await?;
        Ok(Box::new(RemotePage { client, session_id: session_id.to_string() }))
    } else {
        let sb = self.ensure_local(session_id, root).await?;
        Ok(Box::new(LocalPage { page: sb.page.clone() }))
    }
}
```

---

## Fazy dalsze (nie w MVP)

### Faza 4: Nowe tooli

| Tool | Co robi |
|---|---|
| `browser_list_tabs` | `chrome.tabs.query()` → lista tabów |
| `browser_switch_tab` | `chrome.tabs.update(tabId, {active: true})` |
| `browser_new_tab` | `chrome.tabs.create({url})` |
| `browser_close_tab` | `chrome.tabs.remove(tabId)` |
| `browser_devtools_send` | Dowolna komenda CDP |
| `browser_devtools_eval` | `Runtime.evaluate` przez debugger |
| `browser_devtools_network` | `Network.getResponseBody` itd. |

### Faza 5: DevTools — `chrome.debugger` API

```javascript
// background.js
await chrome.debugger.attach({ tabId }, '1.3');
// Teraz pełny CDP:
// - Runtime.enable → console events
// - Page.captureScreenshot → full-page + element screenshots
// - DOM.getDocument + DOM.querySelector → element-level ops
// - Network.enable → request/response inspection
// - Input.dispatchMouseEvent/KeyEvent → native input
// - CSS.getComputedStyle → computed styles
```

`chrome.debugger` daje identyczne API co wbudowany chromiumoxide —
wszystkie CDP metody są dostępne.

### Faza 6: Visual verification (element screenshots)

Przez CDP `Page.captureScreenshot` z `clip` do box modelu elementu —
pełna symetria z `browser_screenshot` wbudowanego.

### Faza 7: Security + UX

- Consent flow: Chrome pyta „Allow debugging?" przy pierwszym attach
- Heartbeat extension ↔ engine
- Auto-reconnect przy utracie połączenia
- Docs w README

---

## Kolejność implementacji

| Faza | Zakres | Pliki | Trudność |
|---|---|---|---|
| **1** | HTTP endpoint + `RemotePage` + `BrowserPage` trait | `routes/browser_remote.rs`, `browser/remote.rs`, `browser/driver.rs` | 🔴 Wysoka |
| **2** | Extension: background.js handler komend + manifest | `background.js`, `manifest.json` | 🟡 Średnia |
| **3** | Routing local/remote + config | `browser/driver.rs`, `browser/tools.rs`, `config/loader.rs` | 🔴 Wysoka |
| **4** | Nowe tooli | `browser/tools.rs`, `browser/mod.rs` | 🟢 Niska |
| **5** | DevTools via `chrome.debugger` | `background.js` | 🟡 Średnia |
| **6** | Element screenshots | via CDP | 🟢 Niska |
| **7** | Security + UX polish | manifest, options, docs | 🟢 Niska |

---

## MVP (szybki start)

Najprostsza droga do działania:

1. Extension — `background.js` z handlerami komend, rejestruje się w engine
   co 5s (heartbeat) przez `POST /browser/heartbeat`
2. Engine — nowy route `POST /browser/remote/{action}?directory=` dispatchuje
   do aktywnej extension
3. BrowserDriver — `remote.enabled: true` w configu kieruje tooli do remote

To daje działający MVP w ~500 linii nowego kodu.

---

## Pliki do zmodyfikowania / utworzenia

### Nowe pliki
- `engine/crates/bebok-tools/src/browser/remote.rs` — RemotePage, RemoteClient
- `engine/crates/bebok-server/src/routes/browser_remote.rs` — HTTP dispatch

### Zmodyfikowane
- `engine/crates/bebok-tools/src/browser/driver.rs` — BrowserPage trait, PageSource enum
- `engine/crates/bebok-tools/src/browser/tools.rs` — page_for() → Box<dyn BrowserPage>
- `engine/crates/bebok-tools/src/browser/mod.rs` — re-eksport
- `engine/crates/bebok-server/src/routes/mod.rs` — rejestracja nowego route
- `engine/crates/bebok-core/src/config/loader.rs` — `browser.remote` config
- `engine/crates/bebok-core/src/config/model.rs` — `BrowserRemote` struct
- `client/chrome-extension/background.js` — handler komend
- `client/chrome-extension/manifest.json` — nowe permisje
- `client/chrome-extension/options.html` — opcja remote
