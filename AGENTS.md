# AGENTS.md — Bebok

Orientation guide for anyone (human or agent) working in this repository at
release 1.5.0. It captures what is *not* obvious from a first skim of the code.

## 1. What this is

Bebok is a local-first AI coding agent. The agent logic is a headless Rust
engine exposing HTTP + SSE (+ WebSocket for the terminal); the GUI is a thin
Angular client (desktop via Tauri 2 with the engine as a sidecar, or browser
mode against a manually started engine). All state and logic live in the
engine — the GUI only renders and sends input.

Read order: [`README.md`](./README.md) (features, quick start, config shape)
→ [`CONTRIBUTING.md`](./CONTRIBUTING.md) (conventions, release checklist)
→ this file → the code: `engine/crates/bebok-server/src/routes/mod.rs` (the
single route table), `engine/crates/bebok-core/src/agent/mod.rs`,
`engine/crates/bebok-core/src/store/mod.rs`, `client/src/app/app.routes.ts`.
There is no `SPEC.md`; the code is the specification.

## 2. Build, run, test

All orchestration lives in `scripts/bebok.mjs` (Node ≥ 20, zero deps) and is
exposed through the root `package.json`:

```bash
npm run doctor                     # rustc/cargo, node/npm, tauri cli, OS-level Tauri deps
npm run full-build-dev -- --open   # cargo build -p bebok-server (debug) + start it + ng serve
npm run full-build-app             # cargo build --release --target <triple> + sidecar:copy + tauri build
npm run engine | npm run client    # one half only
dev.cmd / ./dev.sh [tauri] [flags] # shortcuts for full-build-dev
```

`full-build-dev` waits for the engine's `BEBOK_READY http://127.0.0.1:8787/?token=…`
stdout line, then prints `http://localhost:4200/?engine=<url-encoded url>`; the
client adopts `?engine=` once (`client/src/core/transport.strategy.ts`) and
strips it. Flags: `--port`, `--client-port`, `--no-auth` (`BEBOK_NO_AUTH=1`),
`--diagnostic` (`BEBOK_DIAGNOSTIC=1`), `--open`, `--tauri`; `full-build-app`:
`--bundles`, `--skip-engine`, `--skip-tauri`. The script always extends
`BEBOK_CORS` with the ng-serve origin.

Manual steps still work: `cd engine && cargo run` (engine on `:8787`),
`cd client && npm install && npm start` (`:4200`), desktop =
`cargo build --release` → `npm run sidecar:copy` → `npm run tauri:build`
(sidecar must be `client/src-tauri/binaries/bebok-server-<triple>[.exe]`).

Tests and lints (CI runs exactly these on `ubuntu-22.04` + `windows-latest`):

```bash
cd engine && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings
cd engine && cargo test --workspace          # ~553 tests; browser round-trip auto-skips without Chrome
cd client && npm run build                   # type-check + bundle (what CI runs)
cd client && npm test                        # ng test, Karma + Jasmine, ~63 spec files (not in CI)
```

Engine tests must run with `BEBOK_NO_AUTH` unset. Linux from a Windows host:
`docker run --rm -v <clone>:/src -w /src/engine rust:1.97-bookworm bash -c "apt-get install -y -qq git pkg-config libssl-dev && cargo test --workspace"`
(use a real clone, not a linked worktree — its `.git` file points at a Windows
path; use `bash -c`, not `bash -lc`; copy into a named volume if `npm ci` over
the bind mount is slow). Known at 1.5.0: five process-kill tests fail on Linux.

## 3. Repository layout

```text
engine/                      Rust workspace (virtual; edition 2024; rust ≥ 1.85; default-members = bebok-server)
  crates/bebok-core/         config/ (loader, model, jsonc, writer, projects, providers, verify),
                             session/ (persist), store/ (instance_store, instance, lifecycle, session_state),
                             agent/ (preset, catalog, request, turn, gate, exec, observe, images,
                               delegation, delegation_policy, model_policy, supervision_tools, task_tool,
                               fleet_tool, status_rows, verify_prompt, prompt_env),
                             permission/ (engine, matcher, rule, store), change_tracking, git, stats,
                             tool_safety, context, plugin, event, debug, llm_trace
  crates/bebok-server/       BINARY: main, cli, auth, cors, middleware, error, state, server,
                             services/ (turn, provider_factory), routes/ (session, agents, browser,
                               changes, processes, git, projects, config, providers, mcp, meta, fs,
                               fs_browse, tools, stats, events, debug, pty, common, mod = route table)
  crates/bebok-tools/        Tool trait + registry + builtin_tools(); file/shell tools; bash (background),
                             bash_kill, processes (registry), browser/ (driver, discovery, settings,
                             frames, console, tools, verify_tools), pathguard, explorer, runtimes, docker, fetch
  crates/bebok-llm/          provider (Provider trait, error kinds, retry-after), spec (11 built-ins),
                             protocols/ (openai_chat, anthropic_messages), providers/ (xai, deepseek, groq,
                             qwen, openrouter, ollama), model_catalog (vendored models.dev), cost, cache_policy
  crates/bebok-mcp/          rmcp bridge (stdio + streamable HTTP) → tools mcp__<server>__<tool>
  crates/bebok-skills/       AGENTS.md + skill/<name>/SKILL.md discovery, frontmatter, toggles
  crates/bebok-pty/          PTY manager (portable-pty, scrollback ring, single-use tickets, Job Object)
client/                      Angular 20.3 (standalone, signals, zoneless) — see §5
  src-tauri/                 Tauri 2 shell (spawns sidecar --port 0, parses BEBOK_READY, browser viewer window,
                             updater commands update_check / update_install / relaunch_after_update)
  android/, capacitor.config.ts   Capacitor shell + EngineLauncher plugin — wired but NOT part of any release
  scripts/                   copy-sidecar.mjs, bump-version.mjs (npm run version:bump), tauri helpers
scripts/bebok.mjs            root orchestration;  scripts/release.mjs  guided release (PR -> draft -> merge -> publish)
scripts/latest-json.mjs      updater manifest (CI);  scripts/release.md  runbook (CI mechanics)
.github/workflows/ci.yml     fmt + clippy + build + test (engine), npm ci + build (client)
.github/workflows/release.yml tag-triggered 4-leg matrix, SHA256SUMS.txt, optional signing
docs/screenshots/            README images;  CHANGELOG.md;  LICENSE (AGPL-3.0-or-later)
```

## 4. Engine architecture (Rust)

**stdout is a protocol.** The engine prints exactly one line on stdout —
`BEBOK_READY http://host:port/?token=<64 hex>` — and flushes it; the Tauri shell
and `bebok.mjs` parse it. All logs go to stderr (`RUST_LOG`). Never `println!`.

**Auth** (`bebok-server/src/auth.rs`): a per-launch capability token
(`BEBOK_TOKEN` pins it, `BEBOK_NO_AUTH=1` disables it with a loud warning) is
required on every request as `Authorization: Bearer` or `?token=`; exempt:
`OPTIONS` and `GET /pty/{id}/connect` (ticket). `token=` is scrubbed from the
access log. CORS (`cors.rs`) allows localhost/tauri/capacitor origins plus
`BEBOK_CORS`. `GET /config` and `/debug/log` redact secrets; `/debug/log` is 404
unless `BEBOK_DIAGNOSTIC=1`.

**Store & sessions** (`bebok-core/src/store/`): `InstanceStore` keys instances
by normalised directory and sessions by `Uuid`; `get_or_create_instance` lazily
builds config, tools, permission engine, agent catalog and MCP; `reload_instance`
re-reads config after `PUT /config` and publishes `config.changed`. One running
turn per session (`try_begin_turn` → 409 when busy). Lifecycle: fork, compact
(internal fork with parent), truncate (in place), export, delete (refused while
running; reports `is_worktree`). Data root: `dirs::data_dir()/bebok`
(`%APPDATA%\bebok`, `~/.local/share/bebok`, `~/Library/Application Support/bebok`):
`instances/<hash>/`, `sessions/<id>/session.json` + `msg-NNNNNN.json` + `changes/`.

**Config** (`bebok-core/src/config/`): JSONC, layers defaults → global
(`dirs::config_dir()/bebok/config.json`) → project (`<project>/.bebok/config.json`).
Providers merge by name; `models`, `tool_safety`, `delegation` merge per key;
`fleet` replaces. Writers round-trip JSONC with comments (atomic tmp+rename):
`write_project_delta` / `write_global_delta` / `write_full_project`. Never rewrite a
config file from a parsed value alone. Sections: `model`, `models`, `providers`,
`api_key`, `max_tokens`, `thinking`, `context_budget`, `tool_output_cap`, `yolo`,
`permission`, `mcp`, `skills`, `terminal`, `runtimes`, `browser`, `verify`,
`tool_safety`, `delegation`, `fleet`, `ui`, plus the `projects` registry (global only).

**Providers** (`bebok-llm`): `provider_from_model` = text before the first `/`
→ `ProviderSpec {name, kind: openai|anthropic, endpoint, api_key, models, extra}`.
Key precedence: config entry → `<NAME>_API_KEY` → top-level `api_key`.
Effective model per turn (`services/turn.rs`): prompt-body → agent preset →
session → `models.<agent>` → `model`. `model_catalog.rs` loads the vendored
models.dev snapshot (override with `BEBOK_MODEL_CATALOG`) for context window,
pricing, capabilities and the `cheaper` delegation policy. `provider.rs` maps
vendor errors to `rate_limited` / `server_error` / `auth_error` / … and the turn
runner retries transient ones (3 attempts, backoff, `retry-after` honoured).

**Agent loop** (`bebok-core/src/agent/`): presets `code`/`ask`/`plan`/`debug`/
`orchestrator` (+ file agents with hot reload); `request.rs` builds the request
and prunes for `context_budget` (tool outputs first, then images); `exec.rs`
runs tools, caps output, snapshots files for change tracking; `turn.rs` streams.
Delegation: `task_tool.rs` (`task`) spawns child sessions through
`delegation.rs` (`SlotGate` = `max_concurrent`, throttled `task.progress`),
`supervision_tools.rs` adds `task_status` / `task_wait` / `task_cancel`,
`model_policy.rs` picks the child model, `status_rows.rs` emits token-free
progress rows, `delegation_policy.rs` + `verify_prompt.rs` inject the
supervision and frontend-verification prompt sections (`Status: PASS|FAIL` gate).
Sub-agents never receive delegation tools under a policy.

**Permissions** (`bebok-core/src/permission/`): rules are `globset` patterns
over `tool(arg-text)` with `allow` / `ask` / `deny`; order YOLO → agent →
project → global → default (Allow read-only, Ask mutating); `browser_*` asks by
default (except under `verify.frontend = auto`); "always allow" persists a
project rule. `tool_safety.rs` classifies tools `safe` / `caution` / `dangerous`
/ `uncategorized` (informational, exposed by `GET/PUT /tools/safety`).

**Tools** (`bebok-tools`): every tool implements `tool.rs::Tool`;
`is_read_only` drives the default verdict; register in `lib.rs::builtin_tools()`
and add read-only tools to the `ask` / `plan` whitelists in `agent/preset.rs`.
`pathguard.rs` confines paths to the project root (rejects absolute, drive, UNC,
`..`, escaping symlinks). `bash.rs` runs `BEBOK_SHELL` → `cmd /C` / `$SHELL` /
`sh -c`; `background: true` registers the process in `processes.rs`
(`<project>/.bebok/run/<id>.log`, tree kill, `bash_kill`). `browser/` drives one
Chromium per session over CDP (`chromiumoxide`; binary from `BEBOK_BROWSER`,
`CHROME`, PATH or fixed paths — never downloaded; `BEBOK_BROWSER_HEADLESS`,
`BEBOK_BROWSER_NO_SANDBOX`); display `headed` / `viewer` / `drawer`; frames
stream as `browser.frame` SSE events.

**Change tracking / git / stats** (`bebok-core`): `change_tracking.rs` records
`write_file` / `append_file` / `edit_file` targets with first-write snapshots
(diff and revert against git HEAD or the snapshot; `/session/{id}/changes*`).
`git.rs` shells out to the git CLI (30 s timeout): status for `/projects/{id}/git`,
worktrees under `<project>/.bebok/worktrees/<branch>` (engine writes
`.bebok/.gitignore`). `stats.rs` digests sessions for `GET /stats`.

**Events**: one SSE stream at `GET /event`, envelope
`{type, directory, sessionID, properties}`; types include `session.*`,
`message.updated`, `message.part.updated`, `permission.asked/resolved`,
`task.started/progress/ended`, `browser.frame`, `agent.list.changed`,
`config.changed`, `pty.exited`.

**Terminal** (`bebok-pty`): `POST /pty` → `POST /pty/{id}/ticket` (30 s,
single-use) → `GET /pty/{id}/connect?ticket=` WebSocket; scrollback dumped first,
then live bytes; JSON control frames `resize` / `input`. PTY env is scrubbed of
`*PASSWORD*` / `*API_KEY*` / `*TOKEN*` / `*SECRET*`. Compiled out on Android.

## 5. Client architecture (Angular)

- Angular 20.3, standalone components, signals, zoneless (`app.config.ts`).
  State lives in signal stores; RxJS only for real streams (route params, PTY bytes).
- Routes (`src/app/app.routes.ts`): `/` start, `/chat/:sessionID`, `/settings`
  (`?tab=providers|agents|mcp|skills|permissions|appearance|rawJson`), `/terminal`,
  `/explorer`, `/debug`, `/stats`, `/about`, `/browser-view` (bare, no shell).
- `src/core/`: `engine-client.service.ts` (REST), `transport.strategy.ts`
  (tauri / http / capacitor detection, `?engine=` adoption, `localStorage`
  `bebok.remote.baseUrl` / `bebok.remote.token`), `auth.interceptor.ts` (Bearer,
  401 → reconnect banner), `engine.dtos.ts`, `markdown.ts`, `linkify.ts`,
  `inline-classify.ts`, stores: events, open-sessions, projects, processes,
  session-activity, task-progress, tool-safety, ui-prefs, explorer-selection.
- `src/ui/`: `shell/` (AppShell + shell.store), `sidebar/` (252 px / 60 px icon
  rail), `topbar/`, `right-drawer/` + `panels/` (session, explorer, terminal,
  agents, changes, preview, browser), `command-palette/` (Ctrl/Cmd+K),
  `project-switcher/`, `new-session-dialog/` (agent, model, worktree),
  `permission-popup/`, `reconnect-banner/`, `diff-view/`, `diff-overlay/`,
  `markdown-view/`, `html-preview/`, `code-highlight/` (highlight.js, lazy
  languages), `agent-transcript/`, `status-dot/`, `task-progress-line/`, `toast/`.
- `src/views/`: `chat/` (+ `parts/`: tool-group, tool-run-row, text-part,
  thinking-part, status-part, safety-legend, … and `chat-session.store.ts` with
  the context meter / auto-compaction), `settings/` (7 tabs; Agents tab holds the
  Delegation block and Frontend verification card; Permissions tab holds rules,
  YOLO, Tool safety and Browser display), `explorer/`, `terminal/`, `stats/`,
  `about/` (renders `public/about/bebok.<lang>.md`), `updates/` (versions, update check, release list),
  `debug/`, `start/`, `browser-view/`.
- `src/i18n/`: 12 dictionaries (`en` is the reference; the `MessageKey` type is
  derived from it, so a missing key in another language is a compile error).
- Tauri shell (`src-tauri/src/lib.rs`): spawns `bebok-server --port 0`, parses
  `BEBOK_READY`, exposes `engine_info` and `open_browser_viewer` (second
  WebviewWindow on `/browser-view?session=`; browser mode falls back to `window.open`).
  Auto-update lives there too (`update_check` / `update_install` /
  `relaunch_after_update` / `desktop_info`) rather than in the JS updater
  plugin: only the Rust `UpdaterBuilder::on_before_exit` can kill the sidecar
  before the installer runs. The client side is `core/update.store.ts` +
  `ui/update-banner/`.
- Mobile: `capacitor.config.ts` + `android/` with an `EngineLauncher` plugin exist
  but no binaries are bundled and nothing is released; treat as unreleased scaffolding.

## 6. Conventions

- **Branches / merges**: `main` receives PR-only merges (no direct pushes). Work
  packages run in their own git worktree (`git worktree add ../bebok-wt/<name> -b wp/<name>`),
  one branch per package, rebased onto the integration branch before the PR.
- **Commits**: conventional prefixes (`feat:`, `fix:`, `docs:`, `chore:`, `WP-X:`);
  **no `Co-Authored-By` trailers**.
- **i18n**: every user-visible string is a key in all 12 dictionaries
  (`client/src/i18n/*.ts`); keys are append-only — never rename or delete;
  a machine translation for the non-English locales is acceptable.
- **Engine hygiene**: `cargo fmt` + `cargo clippy -D warnings` clean before a PR;
  new tools go through `builtin_tools()` and the preset whitelists; new routes
  through `routes/mod.rs`; new config keys through `config/loader.rs::apply`
  plus the JSONC writers; new SSE event types documented in `event.rs`.
- **Client hygiene**: signals over RxJS, no zone.js, `npm run build` must pass;
  `data-testid` on new interactive elements; specs next to the file.
- **Secrets**: never commit keys (`.gitignore` covers `.env*`, `.bebok/`); tests
  use isolated data dirs (`InstanceStore::with_data_dir`).
- **Versioning / release**: `cd client && npm run version:bump -- X.Y.Z` edits
  all seven manifests and lockfiles; `preflight` in `release.yml` fails if they
  disagree with the tag. Process: [`CONTRIBUTING.md`](./CONTRIBUTING.md#releasing)
  (`node scripts/release.mjs X.Y.Z` -> draft from the PR -> merge publishes); CI
  mechanics in [`scripts/release.md`](./scripts/release.md).

## 7. Gotchas

- The desktop sidecar listens on a random port (`--port 0`); `8787` applies only
  to a manually started engine. On Windows find it via `tasklist | findstr bebok`
  + `netstat -ano | findstr LISTENING`.
- `bash` on Windows runs `cmd /C`: no `head`/`grep`/`curl`; agents should use the
  native tools. Set `BEBOK_SHELL` to change the shell for both `bash` and the terminal.
- Cancelling a delegated child aborts the whole parent turn; the orchestrator may
  answer without delegating unless the prompt demands a `task` call.
- Linked git worktrees break `git` inside Docker (absolute Windows `gitdir`).
- The `release` job's GitHub API upload can fail transiently; assets can be
  re-uploaded from the workflow artifacts with `gh run download` + `gh release upload`.
