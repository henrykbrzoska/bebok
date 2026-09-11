# AGENTS.md — Bebok

Orientation guide for anyone (human or agent) working in this repository. It
captures the things that are *not* obvious from a first skim of the code.

## 1. What this is

Bebok is a local-first AI coding agent, built from
scratch. The agent logic is a headless Rust engine exposing HTTP + SSE (+ WS
for the terminal); the GUI is a thin Angular client. All state and logic live
in the engine — the GUI only renders and sends input.

**Read order** (important, in this order):

1. [`README.md`](./README.md) — quick start, endpoints, config shape.
2. This file — architecture, module map, conventions, gotchas.
3. The code itself — `engine/crates/bebok-server/src/routes/mod.rs` (route
   table), `engine/crates/bebok-core/src/agent/mod.rs`,
   `engine/crates/bebok-core/src/store/mod.rs`, `client/src/app/app.routes.ts`.

> Note: there is no `SPEC.md` and no `docs/milestones/` in this repo. Older
> versions of these docs referenced them — those links are dead. The code is
> the specification.

## 2. Build, run, test

```bash
# Engine (headless). Provider keys via env (ZAI_API_KEY, OPENAI_API_KEY, ...)
# or config (see §4 "Provider selection").
cd engine
ZAI_API_KEY=<key> cargo run           # listens on http://127.0.0.1:8787

# Engine tests
cargo test --workspace                # run from engine/

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
Engine bind flags: --host IP, --port PORT, --addr IP:PORT, or BEBOK_ADDR
env (cli.rs::parse_cli; --help prints usage; defaults 127.0.0.1:8787).
Extra CORS origins via BEBOK_CORS (comma separated, see cors.rs).

There is no auth password (Basic Auth was removed); the engine binds
127.0.0.1 and CORS is restricted to known localhost/tauri/capacitor origins.
The sidecar binary must be named bebok-server-[.exe] in
client/src-tauri/binaries/ (npm run sidecar:copy → scripts/copy-sidecar.mjs,
triple taken from rustc -vV).

Cross-platform: the whole stack builds on Linux and Windows. The engine uses
portable-pty plus #[cfg(windows)]/#[cfg(unix)] gates; the desktop bundling
picks per-OS targets. CI (.github/workflows/ci.yml, jobs engine + client)
builds and tests the engine (cargo build/test --workspace from engine/)
plus the client (npm ci + npm run build) on ubuntu-latest and
windows-latest on every push to main/master and every PR.

## 3. Repository layout

engine/                     # Rust workspace (virtual; default-members = bebok-server)
  Cargo.toml                # virtual workspace, members = ["crates/*"], edition 2024, rust ≥ 1.85
  crates/
    bebok-core/            # config/, session/, agent/, permission/, store/, event bus, plugin, context, debug
    bebok-server/          # BINARY: main.rs + server.rs + state.rs + cli.rs + cors.rs + error.rs
                            #        + middleware.rs + routes/ (facade) + services/ (orchestration)
    bebok-tools/           # Tool trait + built-in tools + registry + runtimes + docker probe + explorer
    bebok-llm/             # Provider trait + OpenAI/Anthropic/Z.ai/OpenAI-compatible clients (SSE) + spec.rs
    bebok-mcp/             # rmcp bridge (MCP servers -> tools as mcp__<server>__<tool>)
    bebok-skills/          # AGENTS.md + skills discovery + frontmatter + prompt assembly
    bebok-pty/             # PTY manager: spawn, scrollback ring, resize, tickets, Job Object
client/
  src/
    main.ts / app/          # bootstrap, app config (ZONELESS), routes
    core/                   # engine client, transport, events/activity/tabs/prefs/css stores, DTOs, fetch wrapper
    views/                  # start/, chat/, explorer/, terminal/, settings/, connect/, config/, debug/
    ui/                     # permission-popup/, session-sidebar/, html-preview/, shared UI
    i18n/                   # 12 translation dictionaries (en.ts is the reference) + service + index
  scripts/copy-sidecar.mjs  # release binary -> src-tauri/binaries/bebok-server-<triple>[.exe]
  capacitor.config.ts       # Capacitor shell (webDir -> dist/bebok/browser)
  src-tauri/                # Tauri 2 shell (spawns engine sidecar, native dialogs)
## 4. Engine architecture (Rust)

### Workspace & binary

- engine/Cargo.toml is a virtual workspace: members = ["crates/*"],
default-members = ["crates/bebok-server"]. Edition 2024, rust-version = "1.85".
Shared deps are declared once in [workspace.dependencies] and pulled in with
foo.workspace = true from each crate.
- The binary crate is bebok-server. It is thin and split after the
backend refactor (no api.rs anymore — it was split into routes/ + services/):
- main.rs (~40 lines) — parse CLI, build AppState, call server::serve.
- cli.rs — BindSpec + parse_cli (--host / --port / --addr, BEBOK_ADDR).
- server.rs — bind, BEBOK_READY handshake, build_api_router() + CORS +
access-log middleware, debug-log wiring, serve loop.
- state.rs — shared state (see below).
- cors.rs — default origins + BEBOK_CORS extras.
- error.rs — ApiError → HTTP status mapping.
- middleware.rs — log_http access log (skips /event, /debug*, */connect).
- routes/ — facade, one module per area, thin extract -> service -> json
handlers: mod.rs (the single route table, build_api_router),
session.rs, config.rs, meta.rs (agents/plugins/docker/models),
mcp.rs, fs.rs, debug.rs, events.rs, pty.rs (`#[cfg(not(target_os
= "android"))]), common.rs`.
- services/ — orchestration: turn.rs (prompt_turn: claim slot → resolve
agent/model → assemble_prompt → provider → append message → spawn turn →
202), provider_factory.rs (build_provider).
- Shared state is
AppState { store: Arc, ptys: Arc, debug: Arc },
passed as State in handlers. ptys is `#[cfg(not(target_os =
"android"))]` — PTY routes are compiled out on Android.

### Critical: stdout is a protocol, logs go to stderr

- The engine prints BEBOK_READY http://host:port as the first line on
stdout and flushes it. The Tauri shell (src-tauri/src/lib.rs) parses
this line to discover the random port when spawned with --port 0.
- All tracing logs go to stderr. Never println! anything else to stdout,
or the handshake breaks.

### InstanceStore & sessions

- The store is a module directory, not one file: bebok-core/src/store/ =
mod.rs (re-exports) + instance.rs (Instance: directory/root, live
ResolvedConfig, tools, permission engine, agent catalog, MCP manager,
context_notes) + instance_store.rs (directory-keyed runtime) +
lifecycle.rs (fork/compact/truncate/export/delete) + session_state.rs
(one turn slot, abort token, permission oneshots).
- InstanceStore keys instances by normalized directory path and
sessions by Uuid:
- new() / with_data_dir(path) constructors; bus() returns the single
global EventBus (/event SSE stream); data_dir() is the engine data dir.
- get_or_create_instance(directory) — lazily builds Instance (config,
tools, permission engine, agents, MCP) and starts the agent hot-reload watcher.
- reload_instance(directory) — re-reads config after PUT /config, recompiles
permissions, re-syncs MCP, publishes config.changed.
- Lifecycle (lifecycle.rs): fork_session, compact_session,
truncate_session, export_session, delete_session (refused with 409
while a turn runs); query/creation: create_session,
continue_last_session, open_session, list_sessions, session_meta.
- SessionState enforces one running turn per session: try_begin_turn()
returns false when busy → prompt_turn responds 409 Conflict.
- Pending permission ask requests are stored as oneshot::Senders keyed by
request id; first resolver wins, later resolutions get 404.

### Config (layered JSONC)

- Resolution: defaults → global ~/.config/bebok/config.json → project

/.bebok/config.json. Split across bebok-core/src/config/:
model.rs (ResolvedConfig, UiConfig, builder, defaults), loader.rs
(layer application, parse_thinking, provider_from_model,
project_config_path, global_config_path), providers.rs
(save_provider_models, save_provider_models_global), writer.rs
(write_project_delta, write_global_delta, write_full_project),
jsonc.rs, diagnostics.rs.
- ResolvedConfig has model, provider, max_tokens, thinking, api_key,
models (generic per-agent-type overrides map), providers
(Vec), context_budget, tool_output_cap, yolo
(auto-allow everything, dangerous), raw Value sections for permission,
mcp, skills, terminal, runtimes, and ui: UiConfig (client-only
custom CSS text + file list, sanitized and never executed by the engine).
Providers merge by name across layers (global providers survive, project
providers override/add; unknown names are appended as custom providers).
- thinking is the reasoning-effort level (off/low/medium/high/max;
default off). Mapped per provider: OpenAI-compatible → reasoning_effort
(max → high); Anthropic/Z.ai → thinking with budget_tokens
(1024/2048/4096/8192). parse_thinking (in config/loader.rs) also accepts
no/none → off and mid → medium.
- Writers do a JSONC round-trip (preserve comments) with an atomic tmp+rename
write: write_project_delta / write_global_delta for GUI edits,
write_full_project for full rewrites. Never rewrite a config file from a
parsed value alone.
### Provider selection

- services/provider_factory.rs::build_provider(config, model) maps the model
prefix (provider_from_model = text before the first /: openai/,
anthropic/, zai/, xai/, deepseek/, google/, mistralai/, groq/,
qwen/, openrouter/, ollama/, or any config-defined name) to a
ProviderSpec.
- ProviderSpec = `{ name, kind: openai|anthropic, endpoint?: string,
api_key?: string, models: string[] }`. Built-in defaults live in
bebok-llm/src/spec.rs (builtin_provider_specs, default_endpoint,
resolve_provider_specs), merged with config providers. The API key
resolves from spec.api_key → the provider env var (spec.env_var(), e.g.
ZAI_API_KEY) → config.api_key fallback.
- AnthropicProvider serves anthropic + zai (Messages API); OpenAiProvider
serves everything OpenAI-compatible. GET /models?directory=&provider= lists
a provider's models (bebok_llm::list_models), persists them into the project
config (save_provider_models + reload_instance), backing the GUI's "check
available models" button.
- The effective model for a turn (in services/turn.rs::prompt_turn) is:
prompt-body model → agent preset model → session.model →
config.model_for(agent) → config.model. An explicit agent on the prompt
wins and is persisted for subsequent turns.
- Prompt assembly (assemble_prompt in services/turn.rs): global/project
AGENTS.md + enabled skills (`bebok-skills::discover/apply_toggles/
assemble_prompt) plus mid-chat environment notes (take_context_notes`:
MCP/skill/yolo toggles). This is the seam for future editable prompts.

### Agent loop

- Split, no behavior change: bebok-core/src/agent/ = preset.rs (Agent +
CODE/ASK/PLAN/DEBUG/ORCHESTRATOR prompts) + catalog.rs (AgentInfo,
AgentCatalog, hot-reload watcher emitting agent.list.changed) +
request.rs (RequestBuilder, build_request, prune_for_budget) +
turn.rs (TurnRunner + run_turn delegating shim) + gate.rs (permission
strategy) + exec.rs (tool execution incl. output cap) + observe.rs
(emit_message/emit_part/emit_session, title_from).
- Public surface is re-exported from agent/mod.rs (Agent, AgentCatalog,
run_turn, build_request, …).

### Permission engine

- Split: bebok-core/src/permission/ = engine.rs (gate, decision cache,
CompiledLayer, recompile on config.changed) + matcher.rs
(call_string, glob over tool(arg-text)) + rule.rs (Rule, Action,
parse_rules) + store.rs (read_rules_from, persist_project_rule).
- Resolution order: agent overrides → project rules → global rules → default
(Allow for read-only tools, Ask for mutating tools, yolo allows all).
ask suspends the loop on a per-request oneshot; always on allow
persists an ask → allow project rule.

### Plugins (event-observer host)

- Plugins live in bebok-core/src/plugin.rs: BebokPlugin implements
on_event (observer; sees every bus event via a permanent subscriber the
PluginHost::attach installs) and on_hook (typed filters at Hook points:
before.request, before.tool (veto), after.tool, turn.end,
permission.resolved — see hook_names()). Registration is code-side
(PluginHost::global().register(Arc)); the server attaches
the engine bus at startup and exposes GET /plugins (registered ids + hook
names).
- The agent loop (TurnRunner/run_turn in agent/turn.rs) reads hooks from
PluginHost::global(), so turn code stays decoupled from plugin wiring.
Payloads (RequestHook, ToolCallHook, ToolResultHook, TurnHook,
PermissionHook) cross the boundary as JSON; a plugin returns
Continue/Changed/Stop. Plugin errors are logged, never fatal.
- Tool-level extension: ToolRegistry::register_tool/unregister_tool lets a
plugin add runtime tools (highest lookup precedence, can shadow built-ins).

### Events

- One global SSE stream at /event (routes/events.rs, broadcast EventBus
capacity 1024). Every event envelope is
{ "type", "directory", "sessionID", "properties" }. Clients filter by
directory and sessionID.
- Event types actually emitted: session.created / session.updated /
session.deleted, message.updated, message.part.updated,
permission.asked / permission.resolved, agent.list.changed,
config.changed, debug.log (LLM calls forwarded into the debug log),
pty.exited. There is no mcp.status event — McpManager::status() is
a synchronous query served by GET /mcp.

### Explorer & /fs/*

- GET /fs/tree?directory=&path= returns the immediate children of path
(lazy, gitignore-aware via the ignore crate; dotfiles shown). `GET/PUT
/fs/file?directory=&path=` reads/writes file content for the viewer. Both go
through the engine (one permission model), never the client's raw fs.
- The shared traversal helper is bebok-tools/src/explorer.rs
(explorer_walker, list_children, tree_text, read_file_text,
write_file_text), re-exported as bebok_core::explorer; the list_dir
and tree tools use it too.
- The client explorer view is src/views/explorer/.

### Built-in tools (native shell replacements)

- bebok-tools ships native Rust equivalents of the most important shell
  commands, so the agent does not have to guess `sh` vs `cmd` syntax:
  - read-only: `pwd`, `head`, `tail`, `wc`, `list_dir`, `tree`, `stat`, `du`,
    `glob`, `grep`, `sort`, `uniq`, `diff`, `which`;
  - mutating: `write_file`, `append_file`, `edit_file`, `mkdir`, `touch`,
    `cp`, `mv`, `rm`, `chmod`.
- Every tool implements `bebok-tools/src/tool.rs::Tool`; `is_read_only`
  defaults to false and is what the permission engine keys off (read-only ->
  Allow, mutating -> Ask).
- The set is registered in `bebok-tools/src/lib.rs::builtin_tools()`. Presets
  with a tool whitelist (`ask`, `plan` in `bebok-core/src/agent/preset.rs`)
  list the read-only ones explicitly, so a new read-only tool must be added
  there too.
- The agent prompts (`bebok-core/src/agent/preset.rs`) and this file tell the
  model to prefer these over `bash` (builds, tests, git and package managers
  are what `bash` is still for). Keep the lists in sync when adding tools.

### Context management

- Tool output is truncated at capture to config.tool_output_cap (tail preserved).
- build_request prunes old tool outputs to a compact digest (request-only, never
persisted) when the transcript exceeds context_budget; images are pruned after
tool results (see "Image attachments" below).
- Compaction (POST /session/{id}/compact) is an internal fork: it creates a
new session (parent set) whose transcript is [summary of messages 0..N] +
the tail. The original session is untouched on disk, so "show full history"
(via GET /session/{id}/export) always works. Helpers live in
bebok-core/src/context.rs; the summary is currently deterministic (LLM
summarization is a later refinement).
- Rollback (POST /session/{id}/truncate { keep }) rewinds the *same* session
in place (no fork); refused while a turn runs.

### Image attachments (multimodal prompts)

- Wire shape: `POST /session/{id}/prompt { ..., images: [{ media_type, data,
  name }] }` with raw base64 (`data:` URL prefixes are accepted and stripped).
  The client stages/renders (views/chat/chat.ts) and never defines the contract.
- Limits + validation live in ONE place: `bebok-core/src/agent/images.rs`
  (`MAX_IMAGES_PER_PROMPT = 5`, `MAX_IMAGE_BYTES = 5 MiB` decoded,
  `ALLOWED_IMAGE_TYPES = png/jpeg/webp/gif`). The prompt route
  (`services/turn.rs::validate_images`), the `task` tool and the `fleet`
  tool all call `validate_agent_images`, so there is a single contract to
  change. Client mirrors: `chat.ts` MAX_IMAGES / MAX_IMAGE_BYTES /
  ACCEPTED_IMAGE_TYPES, `engine.dtos.ts` PromptImage / ImagePart, and the
  `chat.attach*` keys in every `i18n/*` dictionary.
- Validation is payload-based, not client-trust-based: strict base64 decode,
  decoded-size check, and a magic-byte check that must agree with the declared
  `media_type`. A JPEG relabelled `image/png` is a 400, never a provider error.
- Composer normalization (client-only): files >1 MiB are downscaled to a
  2048 px longest edge and re-encoded (target ~2 MiB) before staging, so a
  5 MiB photo does not become ~6.7 MB of base64 in every request/transcript.
  Animated GIFs pass through untouched; the engine limits stay authoritative.
- Vision capability: `images::model_supports_images` is a conservative
  deny-list (deepseek-chat/reasoner, llama-3.3/3.1 text models). A prompt with
  images for a known text-only model fails fast with a 400 *before* the user
  message is appended (no orphaned attachment). Unknown models are assumed
  capable; if such a model still rejects the images, the turn error published
  via `session.updated { error }` is annotated with a vision hint instead of
  only the raw provider error.
- Pruning: `prune_for_budget` prunes tool results first, then images
  oldest-first, replacing each with
  `[image omitted to fit the context budget: <mime>]` so nothing is dropped
  silently and an attachment-heavy transcript still converges. The newest image
  survives whenever the budget allows.
- Wire format: `to_openai_messages` emits `content_parts` as a
  text + `image_url` array -- including when the same message carries
  `tool_results` (user content is never dropped there) -- and
  `to_anthropic_messages` emits `tool_result` blocks first, then `image`,
  then `text` (Anthropic ordering requirement).
- Tests: `cd engine && cargo test -p bebok-core image` (validation, magic
  bytes, pruning), `cargo test -p bebok-llm` (openai/anthropic wire format),
  `cargo test -p bebok-server` (HTTP-layer 400s). Manual end-to-end check:
  attach a PNG + a JPEG in the composer against a vision model and confirm the
  reply references the image content.

### Session lifecycle (fork / continue / export / truncate / delete)

- POST /session accepts { continueLast: true } (returns the most recent
session for the directory) or { forkOf: { sessionID, messageIndex } }
(materializes a copy of messages 0..=messageIndex, records parent).
- Session.parent is Option<(Uuid, usize)>. Lifecycle methods live in
store/lifecycle.rs: fork_session, compact_session,
continue_last_session (also in instance_store.rs), export_session,
truncate_session, delete_session (deletes memory + disk transcript,
emits session.deleted).
- GET /session/{id}/export returns { session, messages } (full JSON; import
is phase 2).

### Debug log

- The engine keeps a single debug.log file next to the global config
(global_config_path().with_file_name("debug.log"),
i.e. ~/.config/bebok/debug.log), cleared on every startup (DebugLog::new
truncates) and capped at DEBUG_LOG_MAX_CHARS = 10 000 characters (oldest
entries dropped first, oversized entries truncated). It records LLM calls
(engine → provider via the debug.log event, forwarded in server.rs) and
HTTP requests (client → engine: method/path → status, via the log_http
axum middleware, which skips /event, /debug* and */connect).
- Served by GET /debug/log (entries) / DELETE /debug/log (clear); the client
Debug tab (src/views/debug/) polls it. The log is separate from sessions
(never written to a session transcript).

### Terminal

- PTYs live in bebok-pty and are owned by the engine (they survive GUI
restarts). The flow: POST /pty → { ptyId }, then POST /pty/{id}/ticket
→ a one-time ticket (TICKET_TTL = 30s, single-use, first connect wins),
then GET /pty/{id}/connect?ticket=... upgrades to a WebSocket. Browsers
cannot set headers on a WS upgrade, hence the ticket.
- On connect the server first dumps the scrollback (`DEFAULT_SCROLLBACK_BYTES
= 1 MiB` ring buffer) as binary frames, then streams live output. Control
frames are JSON text: { "type":"resize", cols, rows } and
{ "type":"input", data: base64 }.
- PtyManager lives on AppState.ptys (#[cfg(not(target_os = "android"))]),
keyed by ptyId; the working directory is passed in the POST /pty body.
GET /pty lists sessions. On Android the whole PTY surface is compiled out
(portable-pty/termios does not build there) — the mobile client has no
terminal.

## 5. Client architecture (Angular)

- Angular 20.3, standalone components + signals + zoneless (no zone.js
runtime; see app.config.ts). State is signals; RxJS is reserved for true
streams (Subscription for paramMap, PTY bytes).
- Routing (app.routes.ts): / (start), /connect, /chat/:sessionID,
/settings, /terminal, /explorer, /debug, /config, wildcard → /.
?directory= query params carry the working directory. The start view
auto-connects on desktop (sidecar) and mobile (embedded engine attempt with
remote-URL fallback).
- ChatView (views/chat/chat.ts) is reused across tabs: sessionID is a
signal fed by route.paramMap.subscribe(...) → switchSession(nextID),
which resets

## [windows] — sidecar discovery, delegation smoke test, cmd gotchas (2026-09-11)

Desktop spawns the engine as a sidecar: `bebok-desktop.exe` launches
`bebok-server.exe` with `--port 0`, so the HTTP port is RANDOM on every
start. The engine prints `BEBOK_READY http://host:port` on stdout (parsed
by the Tauri shell, src-tauri/src/lib.rs); all logs go to stderr. There is
NO fixed 8787 for desktop — 8787 is only the default for a manually started
engine (`cd engine && cargo run`).

How to find the live sidecar engine (verified 2026-09-11, PID 5980 -> 127.0.0.1:64083):
- `tasklist | findstr /I bebok` lists both processes (bebok-desktop.exe + bebok-server.exe + PID).
- `netstat -ano | findstr LISTENING` then match the bebok-server PID to a
  `127.0.0.1:<port>` line — that port IS the engine (e.g. 64083).
- No BEBOK_ADDR/BEBOK_PORT/BEBOK_HOST env is visible in the agent shell; do not rely on env.
- Proof: GET /plugins -> 200, GET /agent?directory= -> orchestrator/code/ask/plan/debug (+ file agents),
  GET /session?directory= -> session list. Sidecar config = project .bebok/config.json
  (models.code/ask/plan/debug/orchestrator overrides, providers with keys).

Delegation smoke test through the SIDE CAR (not cargo run):
- POST /session {directory, agent:'orchestrator'} -> parentID (200).
- POST /session/{parentID}/prompt {message, agent:'orchestrator'} -> 202 running.
- Poll GET /session/{parentID}/message every 3s; assistant text arrives as parts[type=text].
- GET /session?directory= and filter sessions whose parent contains parentID to find the child;
  GET /session/{childID} + /message to verify child agent + final text.
- Lesson 2026-09-11: a polite "use the task tool once..." prompt does NOT guarantee a tool call —
  the orchestrator (Meta/muse-spark-1.3-contributor) answered "OK" directly with NO child session
  (52 sessions scanned, zero children). To force delegation say explicitly:
  "You MUST use the `task` tool now (agent='ask', name='smoke-ok2', ...) Do NOT answer directly -
  your first action must be a task tool call. After the tool returns, reply RESULT: <output>."
- Abort propagation is real: cancelling a child cancels the whole parent turn (second attempt
  returned "aborted" after user abort). task.started/task.ended SSE events carry taskID/status.
- Unit proof without engine: `cd engine && cargo test -p bebok-core task_` -> 5 passed
  (task_schema_documents_images, task_images_validated_not_appended_raw,
  task_args_deserialize_images, hetero_task_agent_overrides_member_agent, child_task_serde_uses_camel_case).

Windows cmd gotchas for probes (cmd /C, no sh utils):
- No head/tail/wc/grep/curl/jq — use `findstr`, `tasklist | findstr`, `netstat -ano | findstr`.
- `tasklist /FI "IMAGENAME eq ..."` quoting breaks under cmd-chained '&'; prefer plain
  `tasklist | findstr /I bebok`.
- `node -e "..."` with nested double quotes fails (Unterminated string constant); write probe
  .js files (http module only) and run `node probe.js` instead (probe-engine.js, delegate-test.js,
  inspect-parent.js, check-config.js pattern).
- set | findstr /I BEBOK to check env; node v24 + cargo 1.98 verified on this box.
