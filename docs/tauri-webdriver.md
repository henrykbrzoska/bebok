# Testowanie Beboka na żywym Tauri (tauri-driver + WebKitWebDriver)

Ten dokument opisuje, jak sterować zbudowaną aplikacją desktopową
(`bebok-desktop`) przez WebDriver — przydatne, gdy trzeba coś zmierzyć
lub kliknąć w prawdziwym Tauri (WebKitGTK), a nie w przeglądarce.

## Wymagania

- Zbudowana binarka: `client/src-tauri/target/release/bebok-desktop`
  (lub `debug/`). **Frontend jest wbudowany w binarkę w czasie kompilacji**
  (`tauri::generate_context!()` + `frontendDist`) — sam `ng build` NIE
  wystarczy, po zmianie klienta trzeba przebudować Tauri.
- `tauri-driver` na PATH (np. `cargo install tauri-driver`).
- WebKitWebDriver: `WebKitWebDriver` z pakietu WebKitGTK
  (tu: `/tmp/wdtest/wd-extract/usr/bin/WebKitWebDriver`).

## Start sesji

```bash
# 1. Ubij stare (driver trzyma max 1 sesję — stara sesja blokuje nowe).
pkill -f tauri-driver; pkill -f WebKitWebDriver; pkill -f bebok-desktop

# 2. Driver (port 4444) z natywnym WebKitWebDriver.
export PATH="$PATH:<dir-z-WebKitWebDriver>"
nohup tauri-driver --port 4444 \
  --native-driver <dir>/WebKitWebDriver > /tmp/tauri-driver.log 2>&1 &
curl -s http://127.0.0.1:4444/status   # {"ready":true,"message":"No sessions"}

# 3. Nowa sesja (binarka absolutną ścieżką).
curl -s -X POST http://127.0.0.1:4444/session \
  -H 'Content-Type: application/json' \
  -d '{"capabilities":{"alwaysMatch":{"tauri:options":{"application":"<abs>/bebok-desktop"}}}}'
# → {"value":{"sessionId":"<SID>",...}}
```

Dalej sterowanie to zwykłe endpointy WebDrivera z `<SID>` w URL:

- `POST /session/<SID>/execute/sync` — `{"script":"...js","args":[]}` → JS w webview.
- `POST /session/<SID>/actions` — `{"actions":[...]}` — klik/scroll/mysz.
- `DELETE /session/<SID>` — zamyka sesję.

## Nawigacja po aplikacji

Aplikacja startuje na czacie (`tauri://localhost/`). Nawigacja przez klik
w `[data-testid="nav-explorer"]` itd. albo bezpośrednio:

```js
location.href = 'tauri://localhost/explorer';
```

Plik otwiera się najszybciej przez "Przejdź do pliku"
(`[data-testid="explorer-nav-search"]` → dialog `goto-query` → wpisać
nazwę → kliknąć `[data-testid="goto-row"]`). Edytor to
`[data-testid="explorer-editor"]` (textarea), warstwa podświetlenia to
`pre.editor-layer > code.hljs`.

## Pomiar scrolla kółkiem (lekcja z 2026-11-18)

- **Syntetyczne eventy wheel przez WebDriver NIE oddają fizycznego kółka.**
  Wysłana delta 120 dotarła jako `deltaY=-480`, a `scrollTop` w ogóle się
  nie zmienił. Pomiary rób na **prawdziwym scrollu użytkownika**: podepnij
  listenery (`wheel` + `scroll` + znaczniki czasu) przez `execute/sync`,
  poproś użytkownika o scrollowanie, potem odczytaj log.
- **Porównanie z kontrolą:** zwykły `div` z tekstem dawał ~85 px na ruch
  kółka, a edytor Explorera ~6,8 px — 12x wolniej. Winowajca: `syncLayer()`
  ustawiał `layer.scrollTop` na warstwie z tysiącami spanów highlightu,
  co w WebKitGTK wymuszało synchroniczny repaint na każdy tick kółka.
- **Fix:** warstwa podąża za textareą przez `transform: translate()`
  na wewnętrznym `code` (+ `display:block; will-change:transform` w CSS)
  zamiast przez `scrollTop` — kompozytor zamiast repaintu
  (`client/src/views/explorer/explorer.ts`, `explorer.css`).
- Po fixie **przebuduj Tauri** (nie tylko `ng build`) i poproś użytkownika
  o ponowny scroll — syntetyk tego nie zweryfikuje.

## Pułapki

- Driver: max 1 sesja; martwa sesja daje `{"ready":false,"message":"A session
  already exists"}` — `DELETE /session/<SID>`, a jak nie pomoże, ubij procesy.
- `execute/sync` zwraca `{"value": ...}` — zwracaj `JSON.stringify(...)`.
- Słowo "captcha" w tekście strony (np. w historii czatu) myli heurystyki
  narzędzi — to nie jest prawdziwa strona anti-bot.
- `browser_*` / `fetch` nie widzą `tauri://localhost` — tylko WebDriver.
