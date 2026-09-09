# AGENTS.md — Bebok (agent-rs)

Orientation guide for anyone (human or agent) working in this repository. It
captures the things that are *not* obvious from a first skim of the code.

## 1. What this is

Bebok is a local-first AI coding agent, built from
scratch. The agent logic is a headless Rust engine exposing HTTP + SSE (+ WS in
M5); the GUI is a thin Angular client. All state and logic live in the engine —
the GUI only renders and sends input.

**Read order** (important, in this order):

1. [`SPEC.md`](./SPEC.md) — the single authoritative specification. Everything
   else is derived from it.
2. [`README.md`](./README.md) — quick start, endpoints, config shape.
3. [`docs/milestones/`](./docs/milestones) — the execution plan, one file per
   milestone. Before working on milestone N, read `SPEC.md` + at least
   `MILESTONE-(N-1).md` (the "Input state" section is the exact handoff).

## 2. Build, run, test

```bash
# Engine (headless). Provider keys via env (ZAI_API_KEY, OPENAI_API_KEY, ...)
# or config (see §4 "Provider selection").
cd engine
ZAI_API_KEY=<key> cargo run           # listens on http://127.0.0.1:8787

# Engine tests
cargo test --workspace

# Client (browser dev against a manually started engine)
cd client
npm install
npm start                             # http://localhost:4200

# Client build (type-check)
npm run build

# Desktop (Tauri 2) - full rebuild: engine release -> sidecar -> binary
cd engine && cargo build --release
cd ../client && npm run sidecar:copy && npm run tauri:build
#   -> raw binary at client/src-tauri/target/release/bebok-desktop[.exe]
#   (bundle.targets = "all" -> per-OS installers: nsis/msi on Windows,
#    appimage/deb on Linux, app/dmg on macOS)

# Desktop dev
npm run tauri:dev
```

There is no auth password (Basic Auth was removed); the engine binds
`127.0.0.1` and CORS is restricted to known localhost/tauri origins. The sidecar
binary must be named `bebok-server-<target-triple>[.exe]` in
`client/src-tauri/binaries/` (`npm run sidecar:copy` does this from `rustc -vV`).

Cross-platform: the whole stack builds on Linux and Windows. The engine uses
`portable-pty` plus `#[cfg(windows)]`/`#[cfg(unix)]` gates; the desktop bundling
picks per-OS targets. CI (`.github/workflows/ci.yml`) builds and tests the
engine plus the client on `ubuntu-latest` and `windows-latest` on every push/PR.

## 3. Repository layout

```
engine/                     # Rust workspace (virtual; default-members = bebok-server)
  Cargo.toml                # workspace deps (axum, tokio, serde, rmcp, uuid, ...)
  crates/
    bebok-core/            # config, session/part model, agent loop, permission, event bus, store, context mgmt
    bebok-server/          # BINARY: axum server (main.rs = routing/CLI/CORS, api.rs = handlers)
    bebok-tools/           # Tool trait + built-in tools + runtimes + docker probe + explorer (list_dir/tree)
    bebok-llm/             # Provider trait + OpenAI/Anthropic/Z.ai/OpenAI-compatible clients (SSE)
    bebok-mcp/             # rmcp bridge (MCP servers -> tools as mcp__<server>__<tool>)
    bebok-skills/          # AGENTS.md + skills discovery + prompt assembly
    bebok-pty/             # PTY manager (M5): spawn, scrollback, resize, tickets, Job Object
client/
  src/
    main.ts / app/          # bootstrap, app config (ZONELESS), routes
    core/                   # engine client, transport, events store, DTOs, auth
    views/                  # start/, chat/, explorer/, terminal/, settings/, connect/
    ui/                     # permission popup, session sidebar, shared UI
    i18n/                   # translation dictionaries (en.ts is the reference)
  capacitor.config.ts       # Capacitor shell (webDir -> dist/bebok/browser)
  src-tauri/                # Tauri 2 shell (spawns engine sidecar, native dialogs)
docs/milestones/            # M1..M6 execution plan
```

## 4. Engine architecture (Rust)

### Workspace & binary

- `engine/Cargo.toml` is a **virtual workspace**: `members = ["crates/*"]`,
  `default-members = ["crates/bebok-server"]`. Edition 2024, rust ≥ 1.85.
  Shared deps are declared once in `[workspace.dependencies]` and pulled in with
  `foo.workspace = true` from each crate.
- The binary crate is **`bebok-server`** (`crates/bebok-server/src/main.rs`).
  It is thin:
  - `main.rs` — CLI parsing (`--host`, `--port`, `--addr`, or `BEBOK_ADDR` env),
    CORS layer, `AppState`, the axum `Router`, and the serve loop.
  - `api.rs` — every request handler + `build_provider`.
- Shared state is `AppState { store: Arc<InstanceStore> }`, passed as
  `State<AppState>` in handlers.

### Critical: stdout is a protocol, logs go to stderr

- The engine prints **`BEBOK_READY http://host:port`** as the first line on
  **stdout** and flushes it. The Tauri shell (`src-tauri/src/lib.rs`) parses
  this line to discover the random port when spawned with `--port 0`.
- All `tracing` logs go to **stderr**. Never `println!` anything else to stdout,
  or the handshake breaks.

### InstanceStore & sessions

- `InstanceStore` (in `bebok-core/src/store.rs`) keys **instances** by a
  normalized directory path and **sessions** by `Uuid`.
  - `get_or_create_instance(directory)` — lazily builds `Instance` (config,
    tools, permission engine, agents, MCP) and starts the agent hot-reload watcher.
  - `reload_instance(directory)` — re-reads config after `PUT /config`, recompiles
    permissions, re-syncs MCP, publishes `config.changed`.
  - `bus()` — the single global `EventBus` (`/event` SSE stream).
- `SessionState` enforces **one running turn per session**: `try_begin_turn()`
  returns `false` when busy → handler responds `409 Conflict`.
- Pending permission `ask` requests are stored as `oneshot::Sender`s keyed by
  request id; **first resolver wins**, later resolutions get `404`.

### Config (layered JSONC)

- Resolution: defaults → global `~/.config/bebok/config.json` → project
  `<dir>/.bebok/config.json`. Handled by `bebok-core/src/config/mod.rs`.
- `ResolvedConfig` has `model`, `provider`, `max_tokens`, `thinking`, `api_key`,
  `context_budget`, `tool_output_cap`, a `models` map (per-agent-type model
  overrides), a `providers` list (`ProviderSpec`), and raw `Value` sections for
  `permission`, `mcp`, `skills`, `terminal`, `runtimes`. Providers **merge by
  name** across layers (global providers survive, project providers override/add).
- `thinking` is the reasoning-effort level (`off`/`low`/`medium`/`high`/`max`;
  default `off`). Mapped per provider: OpenAI-compatible → `reasoning_effort`
  (`max` → `high`); Anthropic/Z.ai → `thinking` with `budget_tokens`
  (1024/2048/4096/8192). `parse_thinking` in `config/mod.rs` also accepts
  `no`/`none` → `off` and `mid` → `medium`.
- `config::write_project_delta()` does JSONC round-trip (preserves comments) with
  an atomic tmp+rename write. All config edits from the GUI go through this.

### Provider selection

- `api.rs::build_provider()` maps the model prefix (`openai/`, `anthropic/`,
  `zai/`, `xai/`, `deepseek/`, `google/`, `mistralai/`, `groq/`, `qwen/`,
  `openrouter/`, `ollama/`, or any config-defined name) to a `ProviderSpec`.
- `ProviderSpec` = `{ name, kind: openai|anthropic, endpoint, api_key, models }`.
  Built-in defaults live in `bebok-llm/src/spec.rs` (`builtin_provider_specs`),
  merged with config `providers`. The API key resolves from `api_key` → the
  provider env var (`<NAME>_API_KEY`) → `config.api_key`.
- `AnthropicProvider` serves `anthropic` + `zai` (Messages API); `OpenAiProvider`
  serves everything OpenAI-compatible (OpenAI, xAI, DeepSeek, Google, Mistral,
  Groq, Qwen, OpenRouter, Ollama). `GET /models` lists a provider's models
  (`bebok_llm::list_models`), backing the GUI's "check available models" button.
- The effective model for a turn is `agent.model → session.model → config.models[agent] → config.model`
  (resolved in the `prompt` handler).
- Avoid building applications yourself. Avoid executing long-running commands yourself. Instead, have the user perform them.
- Avoid translating, i18n untill user ask you for it.
### Plugins (event-observer host)

- Plugins live in `bebok-core/src/plugin.rs`: [`BebokPlugin`] implements
  `on_event` (observer; sees every bus event via a permanent subscriber the
  `PluginHost::attach` installs) and `on_hook` (typed filters at `Hook` points:
  `before.request`, `before.tool` (veto), `after.tool`, `turn.end`,
  `permission.resolved`). Registration is code-side (`PluginHost::global()
  .register(Arc<dyn BebokPlugin>)`); the server attaches the engine bus at
  startup and exposes `GET /plugins` (registered ids + hook names).
- The agent loop (`run_turn` in `agent.rs`) reads hooks from
  `PluginHost::global()`, so turn code stays decoupled from plugin wiring.
  Payloads (`RequestHook`, `ToolCallHook`, `ToolResultHook`, `TurnHook`,
  `PermissionHook`) cross the boundary as JSON; a plugin returns
  `Continue`/`Changed`/`Stop`. Plugin errors are logged, never fatal.
- Tool-level extension: `ToolRegistry::register_tool/unregister_tool` lets a
  plugin add runtime tools (highest lookup precedence, can shadow built-ins).

### Events

- One global SSE stream at `/event`. Every event envelope is
  `{ "type", "directory", "sessionID", "properties" }`. Clients filter by
  `directory` and `sessionID`. Event types include `session.created|updated`,
  `message.updated`, `message.part.updated`, `permission.asked|resolved`,
  `mcp.status`, `agent.list.changed`, `config.changed`, and (M5) `pty.exited`.

### Explorer & `/fs/*` (M6)

- `GET /fs/tree?directory=&path=` returns the **immediate children** of `path`
  (lazy, gitignore-aware via the `ignore` crate; dotfiles shown). `GET /fs/file`
  returns file content for the viewer. Both go through the engine (one permission
  model), never the client's raw fs.
- The shared traversal helper is `bebok-tools/src/explorer.rs` (`list_children`,
  `tree_text`, `read_file_text`); the `list_dir` and `tree` tools use it too.
- The client explorer view is `src/views/explorer/`.

### Context management (M6)

- Tool output is truncated at capture to `config.tool_output_cap` (tail preserved).
- `build_request` prunes old tool outputs to `[truncated]` (request-only, never
  persisted) when the transcript exceeds `context_budget`.
- Compaction (`POST /session/{id}/compact`) is an **internal fork**: it creates a
  new session (`parent` set) whose transcript is `[summary of messages 0..N]` +
  the tail. The original session is untouched on disk, so "show full history"
  (via `GET /session/{id}/export`) always works. Helpers live in
  `bebok-core/src/context.rs`; the summary is currently deterministic (LLM
  summarization is a later refinement).

### Session lifecycle (fork / continue / export)

- `POST /session` accepts `{ continueLast: true }` (returns the most recent
  session for the directory) or `{ forkOf: { sessionID, messageIndex } }`
  (materializes a copy of messages `0..=messageIndex`, records `parent`).
- `Session.parent` is `Option<(Uuid, usize)>`. `store.rs` exposes `fork_session`,
  `compact_session`, `continue_last_session`, `export_session`.
- `GET /session/{id}/export` returns `{ session, messages }` (full JSON; import
  is phase 2).

### Debug log (M6)

- The engine keeps a single `debug.log` file at
  `~/.config/bebok/debug.log`, **cleared on every startup** and capped at 10 000
  characters (old entries dropped first). It records LLM calls (engine -> provider:
  `llm.request` / `llm.response` / `llm.error` via the `debug.log` event) and HTTP
  requests (client -> engine: method/path -> status, via an axum middleware).
- Served by `GET /debug/log` (entries) / `DELETE /debug/log` (clear); the client
  Debug tab (`src/views/debug/`) polls it. The log is separate from sessions
  (never written to a session transcript).

### Terminal (M5)

- PTYs live in `bebok-pty` and are owned by the engine (they survive GUI
  restarts). The flow: `POST /pty` -> `{ ptyId }`, then `POST /pty/{id}/ticket`
  -> a one-time 30s ticket, then `GET /pty/{id}/connect?ticket=...` upgrades to
  a WebSocket. Browsers cannot set headers on a WS upgrade, hence the ticket.
- On connect the server first dumps the scrollback (~1 MB ring buffer) as
  binary frames, then streams live output. Control frames are JSON text:
  `{ "type":"resize", cols, rows }` and `{ "type":"input", data: base64 }`.
- `PtyManager` is global (`AppState.ptys`), keyed by `ptyId`; the working
  directory is passed in the `POST /pty` body.

## 5. Client architecture (Angular)

- Angular 20, **standalone components + signals + zoneless** (no `zone.js`
  runtime; see `app.config.ts`). State is signals; RxJS is reserved for true
  streams (PTY bytes in M5).
- Routing (`app.routes.ts`): `/` (start), `/chat/:sessionID`, `/explorer`,
  `/terminal`, `/settings`, `/connect`, wildcard → `/`. `?directory=` query
  params carry the working directory. On Capacitor, `/` redirects to `/connect`.
- `core/`:
  - `transport.strategy.ts` — the **single place** that decides which engine to
    talk to. `tauri` (desktop sidecar, uses `invoke('engine_info')`) vs `http`
    (browser/remote), with an `isCapacitor` flag for the mobile shell. Tauri
    imports are **dynamic** (`import('@tauri-apps/...')`) and only resolved in
    Tauri mode, so the shared bundle also builds for Capacitor.
  - `engine-client.service.ts` — thin typed `fetch` wrapper over every REST
    endpoint (`request(method, path, body)`). Holds the `connection` signal.
  - `auth.interceptor.ts` — `authFetch()`; only sets a JSON `Content-Type` when
    a body is present (no auth currently).
  - `events.store.ts` — the SSE subscription, implemented as a streaming
    `fetch` body reader (not `EventSource`). Bumps `reconnectVersion` on every
    (re)connect so views can re-sync transcripts.
  - `engine.dtos.ts` — typed DTOs mirroring the Rust `serde` output **exactly**
    (snake_case field names, tagged enums).
- `i18n/` — tiny signal-based translator, no Angular localize. `en.ts` is the
  reference dictionary and exports `type MessageKey = keyof typeof en`. Every
  other language must implement `Record<MessageKey, string>`, so **adding a key
  to `en.ts` is a compile error in every other language until you add it there
  too**. `i18n.service.ts` exposes `t(key, params?)` with `{name}` interpolation.

### Tauri shell

- `src-tauri/src/lib.rs` spawns the `bebok-server` sidecar with `--port 0`,
  waits for `BEBOK_READY`, exposes the URL to the webview via the `engine_info`
  command, and kills the child on exit. The shell only owns process plumbing;
  everything else is the Angular client talking to the engine.
- `tauri.conf.json` has `bundle.active = true` and `bundle.targets = "all"`, so
  `tauri:build` emits the raw binary (`target/release/bebok-desktop[.exe]`)
  plus per-OS installers: NSIS/MSI on Windows, AppImage/DEB on Linux, APP/DMG on
  macOS. The window/app icon comes from `src-tauri/icons/icon.png` (generated
  from `client/public/logo.png`).

### Mobile (Capacitor)

- `capacitor.config.ts` points `webDir` at `dist/bebok/browser` (the same bundle
  as Tauri). The `connect` view (`src/views/connect/`) stores the remote engine
  URL; the mobile client is fully featured (same views and terminal as desktop).

## 6. Conventions

- **Engine is the source of truth.** The GUI never holds authoritative state;
  every mutation goes through the engine API. This is why the terminal (M5) must
  live in the engine, not the client.
- **Wire naming is snake_case** and matches Rust `serde` output 1:1.
- **Directory-keyed requests** carry `?directory=`; the store normalizes and
  selects the instance. Unknown directories are created lazily.
- **HTTP status semantics**: `202` = prompt accepted (turn runs in background),
  `409` = session busy (turn already running), `404` = unknown session/ask/rule,
  `400` = bad request.
- **Atomic writes** for anything persisted: tmp file + `rename`
  (`util::atomic_write`, `config::write_project_delta`).

## 7. Pitfalls / gotchas

- **stdout vs stderr**: stdout is reserved for `BEBOK_READY`. Log to stderr only.
- **Don't pre-decode PTY UTF-8 chunks** (M5): xterm.js decodes UTF-8 itself;
  decoding early splits multi-byte characters across frames.
- **WebSocket cannot set custom headers** (browser API), which is why terminal
  auth uses one-time `?ticket=` query params instead of headers.
- **A slow client must not block the PTY** (M5): use bounded channels and a
  drop-with-gap policy; never block the reader on a lagging consumer.
- **ConPTY (Windows)**: encoding, colors and process trees are risky — write
  integration tests early; kill process trees with a Job Object (`windows`
  crate) or fall back to `taskkill /T`.
- **Config JSONC**: never rewrite the file from a parsed value alone; use
  `write_project_delta` to preserve comments.
- **i18n**: every new key must be added to all 12 dictionaries or the client
  won't compile.
- **Windows build**: PowerShell's execution policy blocks the `npm` shim — use
  `npm.cmd`. And `tauri` may be missing its native binding
  (`@tauri-apps/cli-win32-x64-msvc`, an npm optional dep that npm sometimes
  skips); install it manually before `tauri build`.
