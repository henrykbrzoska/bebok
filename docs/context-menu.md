# Context menu (WP-CTXMENU)

Right-click menus for chat, Explorer, terminal, debug log and settings.
One abstraction, one overlay, identical behaviour in the web browser and in
the Tauri desktop webview.

## Why a DOM overlay instead of native menus

Tauri offers native menus, but they would look and behave differently from
the browser build (no native menu API there) and cannot reuse the app's
styles or i18n. The context menu is therefore a single Angular overlay
(`ContextMenuComponent`) mounted once in `AppShell`, next to the command
palette. Right-click anywhere with an `[appContextMenu]` directive opens it
at the cursor position; it closes on Escape, outside click, scroll, resize
or navigation. No new dependencies, no platform branches.

## Architecture

```text
client/src/ui/context-menu/
  context-menu.types.ts      ContextMenuItem / ContextMenuProvider / ContextMenuContext
  context-menu.service.ts    provider registry + open/close + clipboard helpers
  context-menu.directive.ts  [appContextMenu] / [appContextMenuData] on any element
  context-menu.ts/.html/.css the overlay (positioning, keyboard, submenus)

client/src/views/<view>/<view>.menus.ts   one provider file per view
```

Three roles, deliberately separated:

1. **Provider** (`ContextMenuProvider<T>`) — a pure function of the
   right-click context. It builds the item list and nothing else. No engine
   calls, no clipboard access, no router. One file per view next to the view
   itself (`explorer.menus.ts`, `chat.menus.ts`, `terminal.menus.ts`,
   `debug.menus.ts` which also holds the settings providers).
2. **View** — registers its providers once (constructor:
   `menus.register(EXPLORER_FILE_PROVIDER)`), attaches them in the template
   (`[appContextMenu]="'explorer.file'" [appContextMenuData]="fileMenuData(row)"`)
   and passes its own callbacks through the payload (`actions: { open, edit,
   copyPath, … }`). Every side effect lives in the view.
3. **Overlay + service** — `ContextMenuService` owns the single open menu
   (position, payload, current text selection) and clipboard helpers with a
   legacy `execCommand` fallback for non-secure contexts. The component
   clamps the menu into the viewport, opens submenus to the right (flipped
   when there is no room) and supports keyboard navigation (arrows / Enter /
   Escape). Every item carries `data-testid="ctx-<id>"`.

Adding a menu to a new target is three lines: write a provider, register it,
put the directive on the element. No changes to the overlay or the service.

## Menus per view

| Target (provider id) | Items |
|---|---|
| Chat user bubble (`chat.message-user`) | Copy selection (when text selected), Copy message, Roll back here (same fork action as the ↩ button) |
| Chat assistant message (`chat.message-assistant`) | Copy selection, Copy message, Quote in composer (reserved for a composer-quote action) |
| Chat tool row (`chat.tool`) | Copy tool output, Copy tool input |
| Explorer tree row (`explorer.file`) | File: Open, Open in preview (previewable extensions), Edit, Copy path, Copy file name. Directory: Expand/Collapse folder, Copy path. Plus Copy selection when text is selected |
| Explorer pane header (`explorer.pane`) | Open in preview, Edit, Copy path, Copy file content |
| Terminal screen (`terminal.screen`) | Copy selection (disabled without an xterm selection), Paste (via async clipboard → xterm input), Clear screen |
| Terminal tab (`terminal.tab`) | Switch to this tab (disabled on the active tab), Copy title, Close terminal |
| Terminal process row (`terminal.process`) | Open log, Open URL (only when the process exposes one), Copy command, Stop (only while running) |
| Debug log row (`debug.row`) | Copy selection, Copy row, Copy path (HTTP rows), Filter by this (puts the row text into the filter box) |
| Debug LLM call card (`debug.call`) | Copy request JSON, Copy response JSON |
| Settings raw JSON (`settings.raw`) | Copy, Paste (at caret), Select all, Format (pretty-print) |
| Settings permission rule (`settings.rule`) | Copy rule (`pattern -> action`), Delete rule |

All labels are i18n keys (`ctx.*`, plus reused `explorer.*`, `processes.*`,
`term.*`, `settings.*` keys) in all 12 dictionaries. Non-English locales
currently carry English machine text for the new `ctx.*` keys — translate on
pass like any other key (missing keys are a compile error, so nothing can
silently fall through).

## Towards an IDE (Explorer)

The Explorer file provider already branches on `isDir`, file extension and
writability, so IDE operations slot in as new items without touching the
overlay or the directive. Suggested next items, in dependency order:

1. **New file / New folder** (directory rows) — needs an engine endpoint;
   today only `GET /fs/tree`, `GET /fs/file` and `PUT /fs/file` exist, so
   create/rename/delete must land in `routes/fs.rs` first.
2. **Rename / Delete** (file rows, confirmed like the Changes revert) —
   same endpoint prerequisite.
3. **Reveal in tree** (pane header → selects the file in the tree) — the
   payload already carries an optional `revealInTree` callback slot.
4. **Copy relative path** — trivial once the project root is in the payload.
5. **Ask agent about this file** — fills the chat composer with the path
   (or opens a session scoped to it); pairs naturally with the existing
   "Quote in composer" slot on assistant messages.
6. **Open with system app** (Tauri shell `open`) / **Show in folder** —
   desktop-only affordances, harmless no-ops in the browser.

None of these require changes to `context-menu.*` — only new provider items
plus the engine/view callbacks behind them.
