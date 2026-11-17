# Explorer editor — autocomplete without AI (handoff)

> Status: **Steps 1–3 implemented, not compiled or tested yet; member
> completion (Step 4) still planned.** Engine: `agent/symbol_index.rs`,
> `agent/complete.rs`, `POST /fs/complete`. Client: `explorer-complete.ts`
> (pure helpers + spec), popup in `explorer.ts|html|css`,
> `engine-client.service.ts::fsComplete`, DTOs in `engine.dtos.ts`.
> Continues
> [`explorer-ide.md`](./explorer-ide.md) §2.1 (editor-first, go to
> definition) and §4.1 (Find in Files). Decision: reuse the engine's
> tree-sitter grammars instead of adding CodeMirror / Monaco / LSP.

## Why this approach

- The engine already parses Rust, TS/JS, PHP, Python and Go with
  tree-sitter (`bebok-core/src/agent/goto_definition.rs`,
  `agent/ast_search.rs` behind `code_ast`). `find_symbols` already
  returns functions, classes, methods, types and fields with kind, file
  and line — Ctrl+click and the *Symbols* tab of Ctrl+Shift+F use it.
- No new client dependency. Suggestions are as good as CodeMirror fed
  from the engine, and we get **local variables / parameters in scope**,
  which CodeMirror's language packs do not provide.
- CodeMirror 6 stays an option for later (huge files, multi-cursor);
  the engine part below would stay unchanged. Monaco is too heavy;
  language servers (intelephense, rust-analyzer, tsserver, gopls) are a
  possible later step for type-aware completion.
- `code_ast` is an agent tool for structural queries — do not call the
  tool; use the same grammars directly from a new module.

## Current editor (what we build on)

- `client/src/views/explorer/explorer.ts|html|css`: transparent
  `<textarea>` over a highlighted `<pre>` (same pattern as Raw JSON).
- `client/src/views/explorer/explorer-editor.ts`: pure helpers (symbol
  under cursor, line ↔ offset math) + `explorer-editor.spec.ts`.
- `client/src/core/text-undo.ts`: global undo/redo for textareas/inputs
  (registered in `app.config.ts`). Completion inserts must go through it
  (or emit an `input` event it records) so Ctrl+Z undoes an accepted
  suggestion as one step.
- Engine routes: `GET /fs/definition`, `GET /fs/search` (text + symbols,
  index-ranked via `code_index_tools.rs::index_ranked_paths`),
  module `agent/project_search.rs`.

## Plan

### Step 1 — symbol cache in the engine

`find_symbols` re-parses the whole project (up to `max_files`) on every
call: fine for Ctrl+Shift+F, too slow per keystroke.

- New in-memory per-instance index (e.g. `agent/symbol_index.rs`, held on
  the instance in `store/`): `path → (mtime, Vec<SymbolHit>)`.
- Lazy build on first use; re-parse a file only when its mtime changed or
  after a save from the editor (`PUT /fs/...` write path) — and ideally on
  agent writes (`change_tracking.rs` hook / `exec.rs`).
- Respects `.gitignore` like `project_search.rs`; same `max_files` cap.
- Prefix lookup (case-insensitive, camel/snake-hump match is a bonus).
- `find_symbols` / Symbols tab / `/fs/definition` can switch to the cache
  afterwards (optional, same results, faster).
- While here: extract shared grammar/extension tables so `code_ast`
  (`ast_search.rs`) also gets Go.

### Step 2 — `POST /fs/complete`

Request `{ path, text, offset, limit? }` (text = unsaved buffer).
Parse the buffer with tree-sitter and return ranked items:

1. locals and parameters visible at `offset` (walk enclosing scopes:
   function params, `let`/`const`/`var`, `$var` assignments in PHP,
   Python assignments, Go `:=`, closure/loop bindings);
2. symbols of the current file;
3. project symbols matching the prefix (Step 1 cache);
4. language keywords (static list per language).

Item: `{ label, kind, detail?, path?, line?, source: local|file|project|keyword }`.
Deduplicate by label (best source wins), cap with `limit` (default ~50).
Register in `routes/mod.rs`, DTO in `client/src/core/engine.dtos.ts`,
method in `engine-client.service.ts`. Rust unit tests per language
(scope walk is the risky part).

### Step 3 — client popup (implemented, unverified in browser)

- Popup anchored at the caret: compute caret x/y from the `<pre>` layer
  (mirror span at the offset; line/offset helpers already exist).
- Triggers: typing an identifier char after a debounce (~150 ms, min
  prefix 1–2), and Ctrl+Space on demand. Cancel stale requests.
- Keys: ↑/↓, PgUp/PgDn, Enter/Tab accept, Esc close; typing filters
  client-side before the next response arrives.
- Accept replaces the identifier prefix before the caret; undoable as one
  step (see `text-undo.ts`).
- Kind badge + `detail`/file on the right; `data-testid` on items.
- i18n: any new strings into all 12 dictionaries (append-only keys).
- Pure ranking/filter/replace helpers in `explorer-editor.ts` (or a new
  `explorer-complete.ts`) with a spec next to it.

### Step 4 (second iteration) — member completion after `.`, `->`, `::`

- `self.` (Rust/Python), `this.` (TS/JS), `$this->` (PHP): methods and
  fields of the enclosing class/impl.
- `Type::` / `Class::`: associated/static methods and constants.
- No type inference: `$user->` does not know `$user` is `User`; fall back
  to generic project members (methods/fields ranked by prefix).

## Suggested session order

1. Step 1 + Step 2 with tests (engine only, `cargo test` / clippy).
2. Step 3 (popup), then browser/Tauri check of caret positioning.
3. Step 4.
4. CHANGELOG bullet under `## Unreleased` and a short section in
   `explorer-ide.md`.

## Open notes from the previous session

- `mcp_server::tests::initialize_list_and_call` (bebok-server) fails with
  `read_file: cancelled`; not caused by the explorer work, cause not
  investigated yet.
- Project policy: ask before running builds/tests.
- Explorer UI changes (Find in Files dialog CSS, undo/redo, inline preview
  column) have not been verified in a browser or in Tauri yet.
