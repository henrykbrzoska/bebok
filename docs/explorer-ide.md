# Explorer → IDE (handoff)

> Implemented: §0 engine endpoints, §1.1–1.5 tree operations
> (new/rename/delete/relative-path/reveal), §2 dirty state + Ctrl+S,
> §2.1 editor-first + go to definition + editor context menu, the
> "Save / Discard on navigation away" guard. Remaining: binary guard +
> external open (§2), agent integration (§3),
> navigation/search/breadcrumbs/icons (§4), multi-root/worktrees (§5).
> Next: editor autocomplete — see [`editor-completion.md`](./editor-completion.md).

Status: the Explorer is a read-only browser with preview and edit.
Tree rows and the pane header already have context menus
(`explorer.file`, `explorer.pane` in
`client/src/views/explorer/explorer.menus.ts`); the file provider
branches on `isDir` / extension / writability, so every item below
slots in as a new provider entry plus a view/engine callback — no
changes to the overlay or directive needed.

## 0. Engine prerequisite (blocks §1–§2)

Today only `GET /fs/tree`, `GET /fs/file`, `PUT /fs/file` exist
(`engine/crates/bebok-server/src/routes/fs.rs`, paths validated
against the project root). File management needs three new endpoints
in `routes/fs.rs`, registered in `routes/mod.rs`:

- `POST /fs/file` — create empty file (parents created as needed).
- `POST /fs/dir` — create directory.
- `DELETE /fs/path` — delete file or empty dir (refuse non-empty
  without `?recursive=true`).
- Rename = create + delete, or a dedicated `POST /fs/rename`
  (`{ from, to }`) so it stays atomic; prefer the dedicated endpoint.

Reuse the existing path-guard validation from `fs.rs`. Every mutating
endpoint must go through the permission engine like `PUT /fs/file`
does. Add `engine-client.service.ts` methods + specs alongside.

## 1. Tree operations (context menu first, toolbar second)

1. **New file / New folder** on directory rows — inline input in the
   tree (like VS Code), Enter commits via `POST /fs/file|dir`,
   Escape cancels. Refresh only the parent node, not the whole tree.
2. **Rename** (F2 + menu) — same inline-input pattern, `POST
   /fs/rename`. Block names with path separators.
3. **Delete** (Del + menu) — confirmation modal reusing the Changes
   revert-confirm pattern; non-empty dirs need explicit "recursive"
   checkbox mapping to `?recursive=true`.
4. **Copy relative path** — trivial once the project root is in the
   payload (already available in the view).
5. **Reveal in tree** — pane header item that expands the tree to the
   open file and selects it; the payload already carries an optional
   `revealInTree` callback slot, wire it to the tree component.
6. **Duplicate** — nice-to-have; `GET` + `POST` under a new name.

## 2. Editor upgrades

- **Dirty state**: track unsaved edits per file; dot on the tab/tree
  row, "Save / Discard" on navigation away. `PUT /fs/file` already
  exists — only the client state is missing.
- **Save shortcut**: Ctrl/Cmd+S in the editor today does nothing;
  bind it to save.
- **Binary guard**: refuse to open (or preview) files over a size cap
  (~1 MB) or with NUL bytes in the first chunk — currently large
  files are loaded whole into the viewer.
- **External open**: "Open with system app" / "Show in folder" via
  Tauri shell `open`; no-op (hidden item) in the browser build.

## 2.1 Editor-first, go to definition, editor menu (done)

- **Open in edit mode**: every text file opens straight in the editor
  (`ExplorerView.openFile` sets `editing`); images stay a preview. The
  editor is a transparent `<textarea>` over a highlight.js `<pre>`
  (`highlightedDraft`, scroll synced both ways so the find bar can paint
  matches into the layer). Header: HTML / Markdown *Preview* toggles,
  *Discard changes*, *Save* (enabled only when dirty). Tab inserts two
  spaces.
- **Go to definition** (Ctrl/Cmd+click, or the editor menu): the click
  moves the caret, `symbolAt()` (`explorer-editor.ts`) takes the
  identifier (PHP `$` stripped, namespace `\` not crossed) and
  `GET /fs/definition?directory=&path=&symbol=` answers. Engine:
  `bebok-core/src/agent/goto_definition.rs` — tree-sitter definition
  queries (functions, methods, classes, structs, interfaces, traits,
  enums, types, consts, fields; top-level variables) for Rust, TS/TSX,
  JS, PHP, Go (`tree-sitter-go`) and Python, within the clicked file's
  language family (TS and JS share one). Files are walked gitignore-aware
  (plus `node_modules`, `target`, `.git`, `.bebok` skipped), pre-filtered
  by a plain `contains(symbol)` and ranked: same file → file named after
  the symbol (`UserService.php`) → nearest directory. Purely syntactic, no
  type inference: overloaded names yield a picker. One hit jumps; the
  clicked declaration itself is filtered out; Alt+← walks back. Holding
  Ctrl/Cmd shows a pointer cursor over the editor. Gitignored `vendor/`
  is not searched.
- **Unsaved-changes guard**: tree clicks and cross-file jumps go through
  `requestOpen()`; a dirty buffer asks Save / Don't save / Cancel.
- **Editor context menu**: the textarea carries `[appContextMenu]=
  "'explorer.editor'"`, so the Tauri webview's native menu no longer
  appears. Items: Go to definition (when a symbol is under the caret),
  Cut, Copy, Paste, Select all, Find, Save. State is read lazily via
  `payload.state()` at right-click time.

## 3. Agent integration

- **Ask agent about this file** (tree + pane menus) — fills the chat
  composer with the path (or opens a session scoped to it). Pairs with
  the existing "Quote in composer" slot on assistant messages; decide
  on one shared composer-prefill helper in the chat store.
- **Apply agent edits**: agent `write_file`/`edit_file` on the open
  file should offer "Reload" (or auto-reload with a toast) instead of
  silently showing stale content. Subscribe to the existing
  `change_tracking` / session events the Changes panel already uses.

## 4. Navigation & search

### 4.1 Find in Files - Ctrl/Cmd+Shift+F (done)

- IntelliJ-style dialog (`views/explorer/find-in-files.{ts,html,css}`),
  opened by Ctrl/Cmd+Shift+F anywhere in the Explorer or the *Find in
  files* header button; prefilled with the editor selection (or the symbol
  under the caret). Query + Match case / Words / Regex toggles (Alt+C /
  Alt+W / Alt+X) + a file mask (`*.rs, !*.spec.ts`, globs over the file
  name or the relative path). Two tabs: **Text** and **Symbols**. ↑↓ /
  PgUp/PgDn select, Enter or double-click opens at the line (Alt+← back),
  Esc closes. The lower half previews ±8 lines of the selected hit.
- Engine: `GET /fs/search?directory=&q=&case=&word=&regex=&mask=&limit=`
  -> `{text: {results, files_matched, files_searched, truncated},
  symbols, indexed}`.
  - Text: `agent/project_search.rs`, gitignore-aware walk (skips `.git`,
    `node_modules`, `target`, `.bebok`, binaries, files > 4 MB), one hit
    per line, limit 500 (max 2000). When the `bebok-index` plugin is
    installed, its ranking for the query (`code_index_tools::
    index_ranked_paths`) is scanned first so a capped result keeps the most
    relevant files (`indexed: true`, shown as a badge). The index alone
    is not enough: it returns paths, not lines.
  - Symbols: `goto_definition::find_symbols`, the same tree-sitter
    definition queries as Ctrl+click, name *contains* the query
    (case-insensitive), ranked exact > prefix > substring, 100 max.
    Skipped for regex queries.
- "Open in preview" now works from the Explorer: the Preview panel is
  shown as a column next to the editor (the right drawer only renders on
  the Chat screen, so the button used to arm invisible state). The column
  follows the opened file and re-reads it every 5 s, so a save shows up.

- **Fuzzy file search** (Ctrl/Cmd+P): input in the explorer header,
  filters the loaded tree client-side first; server-side walk
  (`GET /fs/tree` recursive or a new search endpoint) when the tree is
  lazily loaded. Rank: basename substring > path substring.
- **Breadcrumbs** in the pane header (root › dir › file), each segment
  clickable to navigate the tree.
- **Expand/collapse all** on the tree root row.
- **File icons by extension** — extend the existing icon mapping,
  keep emoji-free (use the app's icon set).

## 5. Multi-root / worktrees (later)

- The engine already has worktrees under
  `<project>/.bebok/worktrees/<branch>`; surface them as switchable
  roots in the explorer header (the session dialog already knows
  about worktrees — reuse that listing).
- Pinned/favourite files section at the top of the tree (client-side
  `localStorage`, per project).

## Suggested session order

1. Engine endpoints (§0) — nothing else unblocks without them.
2. New/Rename/Delete + inline input (§1.1–1.3) — the IDE core.
3. Dirty state + Ctrl+S (§2) — makes editing trustworthy.
4. Reveal in tree + breadcrumbs (§1.5, §4) — cheap navigation wins.
5. Ask-agent + reload-on-agent-edit (§3) — the Bebok-specific payoff.
6. Fuzzy search, worktree roots, favourites (§4–§5) — polish.
