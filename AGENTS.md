# AGENTS.md — Bebok

Orientation guide for anyone (human or agent) working in this repository at
release 1.10.0. It captures what is *not* obvious from a first skim of the code.

## 1. What this is

Bebok is a local-first AI coding agent. The agent logic is a headless Rust
engine exposing HTTP + SSE (+ WebSocket for the terminal); the GUI is a thin
Angular client (desktop via Tauri 2 with the engine as a sidecar, or browser
mode against a manually started engine). All state and logic live in the
engine — the GUI only renders and sends input.

Read order: [`README.md`](./README.md) (features, quick start, config shape)
→ [`CONTRIBUTING.md`](./CONTRIBUTING.md) (rules, release flow; Polish: `CONTRIBUTING.pl.md`)
→ this file → the code: `engine/crates/bebok-server/src/routes/mod.rs` (the
single route table), `engine/crates/bebok-core/src/agent/mod.rs`,
`engine/crates/bebok-core/src/store/mod.rs`, `client/src/app/app.routes.ts`.
There is no `SPEC.md`; the code is the specification.
For how the orchestrator decomposes, spawns and supervises sub-agents see
[`docs/orchestrator.md`](./docs/orchestrator.md). For how an external agentic CLI
is driven as a provider (Claude Code, and the `CliBackend` contract for adding
others) see [`docs/cli-agents.md`](./docs/cli-agents.md). For driving the user's
own Chrome through the Bebok Companion extension see
[`docs/chrome-extension-remote-browser.md`](./docs/chrome-extension-remote-browser.md) (Polish).

## 2. Build, run, test

All orchestration lives in `scripts/bebok.mjs` (Node ≥ 20, zero deps) and is
exposed through the root `package.json`:

```bash
npm run doctor                     # rustc/cargo, node/npm, tauri cli, OS-level Tauri deps
npm run full-build-dev -- --open   # cargo build -p bebok-server (debug) + start it + ng serve
npm run full-build-app             # cargo build --release --target <triple> + sidecar:copy + tauri build
npm run engine | npm run client    # one half only
npm run engine:sweep [-- --dry-run] # trim engine/target to the size cap (default 12GB)
npm run engine:clean               # cargo clean - wipe engine/target
npm run check:windows              # compile + clippy the engine for x86_64-pc-windows-gnu (zig)
npm run lint:cfg                   # same script, --check mode (cfg-only lint)
npm run package:mcp                # dist/bebok-mcp-<ver>-<os>-<arch>.{zip,tar.gz}: release `bebok-server` + Claude Code installer
npm run release:check              # readiness list (read-only); release -- X.Y.Z runs it first
npm run release:status             # where an in-flight release PR / draft stands
npm run release -- X.Y.Z           # guided release: changelog + bump + release/X.Y.Z PR (CI does the rest)
dev.cmd / ./dev.sh [tauri] [flags] # shortcuts for full-build-dev
```

`full-build-dev` waits for the engine's `BEBOK_READY http://127.0.0.1:8787/?token=…`
stdout line, then prints `http://localhost:4200/?engine=<url-encoded url>`; the
client adopts `?engine=` once (`client/src/core/transport.strategy.ts`) and
strips it. Flags: `--port`, `--client-port`, `--no-auth` (`BEBOK_NO_AUTH=1`),
`--diagnostic` (`BEBOK_DIAGNOSTIC=1`), `--open`, `--tauri`; `full-build-app`:
`--bundles`, `--skip-engine`, `--skip-tauri`. The script always extends
`BEBOK_CORS` with the ng-serve origin.

Every orchestrated engine build auto-trims `engine/target` to
`BEBOK_SWEEP_MAXSIZE` (default `12GB`) with `cargo-sweep`
(`cargo install cargo-sweep`) — oldest artifact generations go first and
cargo rebuilds what is missing, so the dir no longer grows without bound;
`npm run engine:sweep` runs the same trim on demand (add `--dry-run` to
preview). The dev/test profiles use `debug = "line-tables-only"`
(`engine/Cargo.toml`) so fresh builds are far smaller; raise it per session
with `CARGO_PROFILE_DEV_DEBUG=2` when you need full debuginfo.

Manual steps still work: `cd engine && cargo run` (engine on `:8787`),
`cd client && npm install && npm start` (`:4200`), desktop =
`cargo build --release` → `npm run sidecar:copy` → `npm run tauri:build`
(sidecar must be `client/src-tauri/binaries/bebok-server-<triple>[.exe]`).

Tests and lints (CI runs exactly these on `ubuntu-22.04` + `windows-latest`):

```bash
npm run check:windows                        # compile + lint the engine for Windows (zig-backed; run before pushing engine changes)
cd engine && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings
cd engine && cargo test --workspace          # browser round-trip auto-skips without Chrome
cd client && npm run build                   # type-check + bundle (what CI runs)
cd client && npm test                        # ng test, Karma + Jasmine (not in CI)
```

Engine tests must run with `BEBOK_NO_AUTH` unset. Linux from a Windows host:
`docker run --rm -v <clone>:/src -w /src/engine rust:1.97-bookworm bash -c "apt-get install -y -qq git pkg-config libssl-dev && cargo test --workspace"`
(use a real clone, not a linked worktree — its `.git` file points at a Windows
path; use `bash -c`, not `bash -lc`; copy into a named volume if `npm ci` over
the bind mount is slow).

## 3. Repository layout

```text
engine/                      Rust workspace (virtual; edition 2024; rust ≥ 1.88; default-members = bebok-server)
  crates/bebok-core/         config/ (loader, model, jsonc, writer, projects, providers, verify, diagnostics),
                             session/ (persist), store/ (instance_store, instance, lifecycle, session_state),
                             agent/ (preset, catalog, request, turn, gate, exec, observe, images, files,
                               delegation, delegation_policy, model_policy, supervision_tools, task_tool,
                               fleet_tool, watchdog, status_rows, sampling_defaults, prompt_env,
                               verify_prompt, build_test_prompt, build_test_gate,
                               code_map_gen, code_map_prompt, code_graph_tools, code_graph_prompt,
                               ast_search, code_ast_tools, code_index_tools, index_prompt, jev_tool),
                             code_graph/ (parser, graph), plugin/ (host + hooks, index_rescan),
                             plugin_decl, plugin_registry, plugin_download, plugin_process,
                             batch, scheduler, fleet_gen, provider,
                             permission/ (engine, matcher, rule, store), change_tracking, git, stats,
                             tool_safety, context, event, debug, llm_trace
  crates/bebok-server/       BINARY: main, cli, auth, cors, middleware, error, state, server, mcp_server,
                             services/ (turn, provider_factory, batch), routes/ (session, agents, browser,
                               browser_remote, changes, processes, git, projects, config, providers, mcp,
                               meta, fs, fs_browse, tools, stats, events, debug, pty, cli, ssh, batch,
                               schedules, plugins, code_graph, code_map, common, mod = route table)
  crates/bebok-tools/        Tool trait + registry + builtin_tools(); file tools and coreutils-style
                             natives (head, tail, wc, tree, find, sort, uniq, diff, stat, du, sed, cp, mv,
                             rm, ln, chmod, gzip, base64, sha256sum, …); bash (background), bash_kill,
                             processes (registry), ssh/ (persistent sessions + `ssh` tool),
                             browser/ (driver, page, remote, discovery, settings, args, frames, console,
                             tools, verify_tools), pathguard, explorer, runtimes, docker, fetch
  crates/bebok-llm/          provider (Provider trait, error kinds, retry-after), spec (12 built-ins:
                             zai, openai, anthropic, xai, deepseek, google, mistralai, groq, qwen,
                             openrouter, opencode, ollama), openai, anthropic, openai_responses, wire,
                             protocols/ (openai_chat, anthropic_messages), kind ProviderKind::{Openai,Anthropic,Cli},
                             providers/ (xai, deepseek, groq, qwen, openrouter, openrouter_batch, opencode,
                             ollama), model_catalog (vendored models.dev), cost, cache_policy
  crates/bebok-cli-agents/   agentic CLIs as providers: trait CliBackend + process runner
                             (line streaming, hard timeout, tree kill, Windows .cmd shim),
                             CliProvider: bebok_llm::Provider, discovery, transcript,
                             backends/ (claude). CLI tool use is presentation only, never
                             a StreamEvent::ToolCall - see docs/cli-agents.md
  crates/bebok-mcp/          rmcp bridge (stdio + streamable HTTP) → tools mcp__<server>__<tool>
  crates/bebok-skills/       AGENTS.md + skill/<name>/SKILL.md discovery, frontmatter, toggles
  crates/bebok-pty/          PTY manager (portable-pty, scrollback ring, single-use tickets, Job Object)
  crates/bebok-ast-cli/      standalone `bebok-ast` binary (JSON request -> AST matches, Rust/TS); legacy:
                             the `code_ast` tool uses the embedded tree-sitter `agent/ast_search.rs` instead
client/                      Angular 20.3 (standalone, signals, zoneless) — see §5
  src-tauri/                 Tauri 2 shell (spawns sidecar --port 0, parses BEBOK_READY, browser viewer window,
                             updater commands update_check / update_install / relaunch_after_update)
  chrome-extension/          Bebok Companion (MV3, Chrome/Edge/Brave + Firefox): registers with the engine,
                             pulls browser commands; manifest.firefox.json + scripts/build-firefox-extension.mjs
                             emit dist/firefox-extension/ (event page instead of a service worker)
  android/, capacitor.config.ts   Capacitor shell + EngineLauncher plugin — wired but NOT part of any release
  scripts/                   copy-sidecar.mjs, bump-version.mjs (npm run version:bump), tauri helpers
scripts/bebok.mjs            root orchestration;  scripts/release.mjs  guided release (PR -> draft -> merge -> publish)
scripts/latest-json.mjs      updater manifest (CI);  scripts/release.md  runbook (CI mechanics)
scripts/check-windows.mjs    zig-backed Windows cross-check (`check:windows`, `lint:cfg`)
skills/                      repo-level skills (remote-browser-extension/SKILL.md)
docs/                        orchestrator.md, cli-agents.md, chrome-extension-remote-browser.md
.github/workflows/ci.yml     fmt + clippy + build + test (engine), npm ci + build (client)
.github/workflows/release.yml tag-triggered 4-leg matrix, SHA256SUMS.txt, optional signing
docs/screenshots/            README images;  CHANGELOG.md;  LICENSE (AGPL-3.0-or-later)
```

## 4. Engine architecture (Rust)

**stdout is a protocol.** The engine prints exactly one line on stdout —
`BEBOK_READY http://host:port/?token=<64 hex>` — and flushes it; the Tauri shell
and `bebok.mjs` parse it. All logs go to stderr (`RUST_LOG`). Never `println!`.

**MCP server mode** (`bebok-server/src/mcp_server.rs`): `bebok-server mcp
[--project DIR] [--read-only] [--exclude a,b]` serves the project instance's
tools over MCP stdio (hand-rolled JSON-RPC: `initialize` / `ping` /
`tools/list` / `tools/call` + `notifications/cancelled`); no HTTP listener, no
`BEBOK_READY`, stdout is the JSON-RPC stream. Delegation tools (`task`, `fleet`,
`task_*`) and tools bridged from the project's own MCP servers are not exposed;
runs YOLO (Bebok permission rules are not consulted, the client is the only
gate; narrow with `--read-only` / `--exclude`). One process = one
Bebok session id (browser / ssh state). Claude Code:
`claude mcp add bebok -- <path>/bebok-server mcp --project <dir>`.

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
`api_key`, `max_tokens`, `thinking`, `sampling` (global + per-agent, over the
hardcoded `agent/sampling_defaults.rs`), `context_budget`, `tool_output_cap`, `yolo`,
`allowed_paths` (parsed, not yet enforced), `permission`, `mcp`, `skills`, `terminal`,
`runtimes`, `browser` (incl. `browser.remote.*`), `verify` (`frontend`, `buildTest`),
`tool_safety`, `delegation` (`max_concurrent`, `max_restarts`, `watchdog_secs`),
`fleet`, `ui`, `code_map`, `code_graph`, `ast_search` (all three off by default),
`ssh` (on by default), plus the `projects` registry (global only).

**CLI providers**: `kind: "cli"` (config `providers[].extra.backend`) builds a
`bebok-cli-agents::CliProvider` instead of an HTTP adapter — it spawns the CLI in
`instance.root`. `bebok_llm::build_provider` refuses a `Cli` spec (`adapter_for`
returns `Err`), and `bebok_core::provider::build_provider_for(.., &ProviderCtx)` is
the entry point every real turn uses. `bebok_core::provider::build_provider`
(no ctx) stays HTTP-only, so `fleet_gen` cannot accidentally drive a CLI. Routes:
`GET /cli/backends`, `POST /cli/backends/{id}/probe`, `GET /cli/profile/{provider}`
(`routes/cli.rs`). **Hooks are the permission path** for managed profiles
(`profile` `bebok`/`isolated`): Claude Code runs `bebok-server cli-hook pre|post|stop`,
which posts to `POST /cli-hook/{session}/{event}` (`routes/cli_hook.rs`) with
`BEBOK_HOOK_URL`/`BEBOK_HOOK_TOKEN`; `pre` is fail-closed (any failure answers `deny`).
The token is a per-turn credential from `bebok_core::cli_turn_token`, scoped to one
session, revoked at turn end and never written to disk. The generated profile lives
under `<data dir>/bebok/cli-profiles/<project-key>/<backend>/` (`profile.rs`, with
`profile.lock.json`).

**Providers** (`bebok-llm`): `provider_from_model` = text before the first `/`
→ `ProviderSpec {name, kind: openai|anthropic, endpoint, api_key, models, extra}`.
`opencode` (OpenCode Zen) speaks the OpenAI *Responses* protocol
(`openai_responses.rs`) and mimics the official CLI headers (free tier is gated
on them); `wire.rs` holds the tool/content mapping shared by all protocols.
Key precedence: config entry → `<NAME>_API_KEY` → top-level `api_key`.
Effective model per turn (`services/turn.rs`): prompt-body → agent preset →
session → `models.<agent>` → `model`. `model_catalog.rs` loads the vendored
models.dev snapshot (override with `BEBOK_MODEL_CATALOG`) for context window,
pricing and capabilities. `provider.rs` maps
vendor errors to `rate_limited` / `server_error` / `auth_error` / … and the turn
runner retries transient ones (3 attempts, backoff, `retry-after` honoured).

**Agent loop** (`bebok-core/src/agent/`): presets `code`/`ask`/`plan`/`debug`/
`orchestrator` (+ file agents with hot reload); `request.rs` builds the request
and prunes for `context_budget` (tool outputs first, then images); `exec.rs`
runs tools, caps output, snapshots files for change tracking; `turn.rs` streams.
Delegation: the orchestrator spawns sub-agents only via `fleet`, fanning out to
roster members named in the prompt's `Fleet:` roster (copied from the resolved
`fleet` config); children run under a per-session `SlotGate` capped by
`delegation.max_concurrent` (`mode` and `model_policy` are gone, ignored with a
warning) and are supervised with `task_status` / `task_wait` / `task_cancel`.
`watchdog.rs` ticks every `delegation.watchdog_secs` per child and restarts a
looping/wandering child in the same session (keeps its slot) up to
`max_restarts`, then hands the work back to the orchestrator (`task.restarted`
event). `verify_prompt.rs` injects the frontend-verification prompt section
(`Status: PASS|FAIL` gate); `build_test_prompt.rs` injects the build & test
policy (`verify.buildTest` = `auto|ask|off`) and `build_test_gate.rs` hard-blocks
test-runner bash commands under `off` *before* the permission engine. Prompt
attachments: images (`images.rs`) and UTF-8 text files (`files.rs`, base64 on
the wire). Sub-agents never receive delegation tools.

**Code navigation** (all opt-in except the index plugin): `code_map`
(`code_map_gen.rs` / `code_map_prompt.rs`) injects a cached directory → one-line
description map (`<project>/.bebok/code-map.json`, refreshed after 24 h,
`GET /code-map`, `POST /code-map/regenerate`); `code_graph` builds a module
dependency graph from `use`/`mod`/imports (`code_graph/`, cached in
`.bebok/code_graph.json`, `GET /code-graph`) read by `code_graph_depends` /
`code_graph_dependents` / `code_graph_impact`; `ast_search` registers `code_ast`
(embedded tree-sitter, Rust/TS/JS/PHP/Python, `max_files` cap); the
`bebok-index` plugin backs `code_index_status` / `code_index_search` and
`index_prompt.rs` tells every agent (incl. sub-agents) to query it before
`grep`/`glob` (override text with `AGENT_INDEX.md` in the plugin slot). These
tools live in `bebok-core` (not `bebok-tools`) to avoid a `core → tools → mcp →
core` cycle and are registered conditionally in `store/instance_store.rs`.

**Plugins** (`plugin/`, `plugin_*.rs`): two layers. In-process `BebokPlugin`s on
the `PluginHost` observe every `EventBus` event and run filters at `Hook`
points (before request, before tool — can veto, after tool, end of turn, after
permission). *Dynamic* plugins are standalone binaries in
`<project>/.bebok/plugins/<name>/`, declared by `<project>/.bebok/plugins/<name>.json`,
spoken to over JSON-lines on stdio (`plugin_process.rs`). Only names listed in
the remote registry (`plugin_registry.rs`, cached with a TTL, offline fallback
= `bebok-index`) can be installed — via git clone or a prebuilt `asset_url`
with `asset_sha256` and zip/tar-slip checks (`plugin_download.rs`). Routes:
`/plugins`, `/plugins/registry`, `/plugins/{name}/install|toggle|update|status|{action}`;
events `plugin.changed`, `plugin.install.progress`. `bebok-jev` (`jev_tool.rs`) is
an optional permission auto-decider: when the gate says Ask it is consulted
first and any failure/uncertainty falls through to the user popup.

**Batch & schedules**: `batch.rs` + `services/batch.rs` run bulk LLM jobs through
the OpenRouter Batch API (`openrouter_batch.rs`, inline submit → poll →
results) without taking the session turn lock (`POST /session/{id}/batch`,
`GET /session/{id}/batches`, `/batch/{id}[/cancel]`, or a `:batch` prompt in
chat; events `batch.*`). `scheduler.rs` persists schedules as JSON under the
engine data dir and ticks every 30 s (`/schedules`, `POST /schedules/{id}/run`,
event `schedule.ran`; client view `/schedules`).

**SSH** (`bebok-tools/src/ssh/`): the `ssh` tool keeps one long-lived `ssh -T`
child per connection so `cd`/env persist between calls (`connect` / `run` /
`close` / `list`); `SshRegistry` is process-wide. Password prompts surface as
`ssh.prompt` events answered via `POST /ssh/prompt/{requestID}`
(`ui/ssh-prompt/`); `GET /ssh`, `POST /ssh/{id}/close`. `ssh.enabled = false`
unregisters the tool.

**Remote browser**: with `browser.remote.enabled` the `browser_*` tools drive a
tab in the user's own Chrome via `client/chrome-extension` (`RemotePage` in
`browser/remote.rs`) instead of the built-in Chromium. Pull protocol: the
extension `POST /browser/register`s, heartbeats every 5 s, polls
`/browser/remote/pending` and posts `/browser/remote/result`.

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
stream as `browser.frame` SSE events. `browser_solve_captcha`
(`browser/handoff.rs`) hands a CAPTCHA / anti-bot page to the user: it forces a
headed launch for that session (`BrowserDriver::page_with_display`, restarting a
headless browser), polls the page until no challenge marker is left and returns
the text — the cookies gained by solving stay in that session's browser. It
refuses on hosts without a window (`headed_blocked_reason`).

**Change tracking / git / stats** (`bebok-core`): `change_tracking.rs` records
`write_file` / `append_file` / `edit_file` targets with first-write snapshots
(diff and revert against git HEAD or the snapshot; `/session/{id}/changes*`).
`git.rs` shells out to the git CLI (30 s timeout): status for `/projects/{id}/git`,
worktrees under `<project>/.bebok/worktrees/<branch>` (engine writes
`.bebok/.gitignore`). `stats.rs` digests sessions for `GET /stats`.

**Events**: one SSE stream at `GET /event`, envelope
`{type, directory, sessionID, properties}`; types include `session.*`,
`message.updated`, `message.part.updated`, `permission.asked/resolved`,
`task.started/progress/restarted/aborted/ended`, `fleet.started`,
`browser.frame/closed`, `process.output/exited`, `agent.list.changed`,
`config.changed`, `pty.exited`, `ssh.prompt`, `batch.*`, `schedule.ran`,
`plugin.*`, `debug.log`.

**Terminal** (`bebok-pty`): `POST /pty` → `POST /pty/{id}/ticket` (30 s,
single-use) → `GET /pty/{id}/connect?ticket=` WebSocket; scrollback dumped first,
then live bytes; JSON control frames `resize` / `input`. PTY env is scrubbed of
`*PASSWORD*` / `*API_KEY*` / `*TOKEN*` / `*SECRET*`. Compiled out on Android.

## 5. Client architecture (Angular)

- Angular 20.3, standalone components, signals, zoneless (`app.config.ts`).
  State lives in signal stores; RxJS only for real streams (route params, PTY bytes).
- Routes (`src/app/app.routes.ts`): `/` start, `/chat/:sessionID`, `/settings`
  (`?tab=general|providers|agents|mcp|skills|permissions|appearance|rawJson`), `/terminal`,
  `/explorer`, `/debug`, `/stats`, `/schedules`, `/about`, `/updates`,
  `/browser-view` (bare, no shell).
- `src/core/`: `engine-client.service.ts` (REST), `transport.strategy.ts`
  (tauri / http / capacitor detection, `?engine=` adoption, `localStorage`
  `bebok.remote.baseUrl` / `bebok.remote.token`), `auth.interceptor.ts` (Bearer,
  401 → reconnect banner), `engine.dtos.ts`, `markdown.ts`, `linkify.ts`,
  `inline-classify.ts`, stores: events, open-sessions, projects, processes,
  session-activity, task-progress, tool-safety, ui-prefs, explorer-selection,
  batch, find, update; plus `model-list.ts`, `custom-css.service.ts`,
  `directory-picker.service.ts`, `browser-viewer.service.ts`.
- `src/ui/`: `shell/` (AppShell + shell.store), `sidebar/` (252 px / 60 px icon
  rail), `topbar/`, `right-drawer/` + `panels/` (session, explorer, terminal,
  agents, changes, preview, browser, batch), `command-palette/` (Ctrl/Cmd+K),
  `project-switcher/`, `new-session-dialog/` (agent, model, worktree),
  `model-select/`, `directory-browser/`, `find-bar/`, `ssh-prompt/`,
  `update-banner/`, `permission-popup/`, `reconnect-banner/`, `diff-view/`, `diff-overlay/`,
  `markdown-view/`, `html-preview/`, `code-highlight/` (highlight.js, lazy
  languages), `agent-transcript/`, `status-dot/`, `task-progress-line/`, `toast/`.
- `src/views/`: `chat/` (+ `parts/`: tool-group, tool-run-row, text-part,
  thinking-part, status-part, safety-legend, … and `chat-session.store.ts` with
  the context meter / auto-compaction), `settings/` (8 tabs; General holds the Code
  features card (code map / graph / AST / index); Agents holds the Delegation
  block, Frontend verification and Build & test policy; Permissions tab holds
  rules, YOLO, Tool safety and Browser display), `explorer/`, `terminal/`, `stats/`,
  `schedules/`,
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
- **Commits**: Conventional Commits - `feat:`, `fix:`, `docs:`, `chore:`,
  `refactor:`, `test:`, optional scope (`feat(updates): …`), `feat!:` or a
  `BREAKING CHANGE:` footer for breaking changes. The release script derives
  the next version and the changelog draft from these prefixes, so a
  mislabelled commit ships a wrong version number: `feat` = user-visible
  addition (minor), `fix` = user-visible correction (patch), everything else
  is invisible to users. Subject in the imperative, no trailing period.
  **Never add `Co-Authored-By`, `Generated with`, `Signed-off-by` or any
  other attribution trailer, footer or badge - not in commits, not in PR
  titles/descriptions, not in code comments, not in the changelog.** The
  author of a commit is the account that pushes it, full stop.
- **i18n**: every user-visible string is a key in all 12 dictionaries
  (`client/src/i18n/*.ts`); keys are append-only — never rename or delete;
  a machine translation for the non-English locales is acceptable.
- **Engine hygiene**: `cargo fmt` + `cargo clippy -D warnings` clean before a PR;
  new tools go through `builtin_tools()` and the preset whitelists (except
  conditionally registered tools like `code_ast`, gated by its config flag in
  `store/instance_store.rs`); new routes
  through `routes/mod.rs`; new config keys through `config/loader.rs::apply`
  plus the JSONC writers; new SSE event types documented in `event.rs`.
- **Client hygiene**: signals over RxJS, no zone.js, `npm run build` must pass;
  `data-testid` on new interactive elements; specs next to the file.
- **Secrets**: never commit keys (`.gitignore` covers `.env*`, `.bebok/`); tests
  use isolated data dirs (`InstanceStore::with_data_dir`).
- **Changelog**: every user-visible change adds a bullet under `## Unreleased`
  in `CHANGELOG.md` in the same PR (grouped `### Features` / `### Fixes` /
  `### Other` like the existing sections). Those bullets become the update
  notes users read inside the app, so write them for users, not for
  reviewers.
- **Releasing** - an agent never tags, never creates a GitHub Release, never
  edits `client/package.json` / `tauri.conf.json` / `Cargo.toml` versions by
  hand and never uploads assets. The only sanctioned path is
  `npm run release` (readiness check -> version suggested from the commits
  -> changelog section -> version bump in all seven manifests ->
  `release/X.Y.Z` PR); CI builds a draft from that PR and the merge tags and
  publishes it. When asked to "release", run `npm run release:check`, report
  the blockers, then run `npm run release` and hand the PR link back - do
  not merge it yourself. Details: [`CONTRIBUTING.md`](./CONTRIBUTING.md#releasing);
  CI mechanics: [`scripts/release.md`](./scripts/release.md).

## 7. Gotchas

- The desktop sidecar listens on a random port (`--port 0`); `8787` applies only
  to a manually started engine. On Windows find it via `tasklist | findstr bebok`
  + `netstat -ano | findstr LISTENING`.
- `bash` on Windows runs `cmd /C`: no `head`/`grep`/`curl`; agents should use the
  native tools. Set `BEBOK_SHELL` to change the shell for both `bash` and the terminal.
- Cancelling a delegated child aborts the whole parent turn; the orchestrator may
  answer without delegating unless the prompt demands a `task` call.
- **Windows is not your machine.** `#[cfg(windows)]` code is not compiled on
  Linux, so an import or const used only there is `dead_code` on Linux and an
  `-D warnings` failure on Windows — the most common way a green local clippy
  run turns into a red `windows-latest` job. `npm run check:windows` runs the
  real compiler for `x86_64-pc-windows-gnu` via zig, so it answers exactly
  instead of guessing from `#[cfg]` attributes; run it before pushing engine
  changes. Structurally, give each platform its own `#[cfg(...)] mod` with its
  own imports and consts — see `bebok-cli-agents/src/runner.rs`'s `platform`
  modules. An item referenced from only one branch then cannot exist by
  accident.
- A CLI hook without its turn token is denied fail-closed: `BEBOK_HOOK_TOKEN` exists
  only in the child environment of a CLI turn, so a hand-run `bebok-server cli-hook pre`
  always answers `deny`. Debug hooks through the engine, not by piping JSON by hand.
- A `profile: "user"` CLI provider writes no Bebok hooks, so its tool calls skip the
  permission engine and `change_tracking.rs`: only the CLI's own flags guard them.
  Even in managed modes, `Bash` writes are not tracked. `extra.allow_write` defaults
  to false for that reason — never flip it silently, and never emit `bypassPermissions`.
- On Windows npm installs Claude Code as `claude.cmd`; the runner wraps
  `.cmd`/`.bat` as `cmd /C`. Claude's `--disallowedTools` is variadic, so the
  prompt must follow a `--` separator or it is parsed as another tool name.
- Linked git worktrees break `git` inside Docker (absolute Windows `gitdir`).
- `code_map`, `code_graph` and `ast_search` are off by default: their tools and
  prompt sections simply do not exist until the project enables them, and the
  `code_graph_*` tools only read the cache (`{ok: false}` when never built).
  `code_index_*` needs the `bebok-index` plugin installed in the project slot.
- `ssh` is on by default; disable it per project to drop the tool schema from
  every request when no remote host is involved.
- Batch jobs always go through the `openrouter` provider spec (native Batch
  API) and the model must carry the `:batch` suffix
  (`openrouter/deepseek/deepseek-chat:batch`), otherwise submit is rejected.
- The `release` job's GitHub API upload can fail transiently; assets can be
  re-uploaded from the workflow artifacts with `gh run download` + `gh release upload`.
