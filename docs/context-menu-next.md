# Context menu — next steps (handoff)

Status: the abstraction layer is done and merged. One DOM overlay
(`client/src/ui/context-menu/`), one provider file per view
(`<view>.menus.ts`), identical behaviour in web and Tauri builds.
See `docs/context-menu.md` for the architecture.

## 1. Kill the Tauri native menu at the source

Today the webview's native menu (Back/Forward/Reload/…) is only
*covered* by our overlay on elements with `[appContextMenu]`, plus a
document-level fallback that swallows it on plain areas. The robust fix
is to disable it in the shell so it can never win a race:

- `client/src-tauri/tauri.conf.json` → `app.windows[].` — check for a
  `disableDefaultContextMenu`-style option in the Tauri version in use;
  if absent, intercept the right-click in `client/src-tauri/src/lib.rs`
  (or a `WebviewWindow` event handler) and drop platform context-menu
  requests while letting text inputs keep theirs.
- Acceptance: right-click on a tool row, a chat bubble, an explorer row,
  a terminal screen, and empty pane background shows **only** our menu
  (or nothing on inputs) in a production `tauri build`, never the
  Back/Forward/Reload popup.

## 2. Cover the views that have no menu yet

In priority order (all follow the same 3-line recipe: provider →
`register()` → directive in template):

1. **Sidebar session list** — Open, Pin/Unpin, Rename, Delete, Copy
   session id. Payload: session id + callbacks already present in the
   sidebar component.
2. **Changes panel rows** — Revert file, Open diff, Copy path. Reuse the
   existing revert confirmation flow.
3. **Preview / Browser / Batch panels** — Copy URL, Reload, Open in
   external browser (Tauri shell `open`), Copy log line.
4. **Agents panel rows** — Open session, Copy agent name, Stop
   (delegated child).
5. **Topbar / pane headers** — low value, do last or skip.

Each provider gets a `*.spec.ts` next to it asserting the built item ids
for a representative payload (see `tool-part.spec.ts` → `chat.tool`
block as the template).

## 3. Overlay hardening

- **Submenu flip**: `context-menu.ts` opens submenus to the right with a
  flip when there is no room — verify against a narrow (400 px) window
  and add a spec if missing.
- **Long menus**: add max-height + scroll for providers that can grow
  (session list, debug rows with big payloads).
- **Keyboard**: arrows/Enter/Escape already work; add type-ahead
  (jump to item by first letter) if cheap.
- **Touch**: long-press should open the menu on touch devices
  (Capacitor scaffolding exists); currently right-click only.

## 4. i18n pass

All `ctx.*` keys exist in all 12 dictionaries but non-English locales
carry English machine text. Do a translation pass over `ctx.*` like any
other key batch (missing keys are a compile error — nothing falls
through silently, but the UX is half-English until translated).

## 5. Telemetry (optional)

Log `ctx.<providerId>.<itemId>` menu activations to the existing stats
pipeline so rarely-used items can be pruned. Keep it client-side only,
no engine changes needed.

## Suggested session order

1. Tauri native-menu kill (§1) — removes the whole bug class.
2. Sidebar + Changes menus (§2.1–2.2) — highest user value.
3. i18n pass (§4) — cheap, user-visible.
4. Overlay hardening (§3) — only after real menus exist to test with.
5. Remaining panels + telemetry (§2.3–2.5, §5).
