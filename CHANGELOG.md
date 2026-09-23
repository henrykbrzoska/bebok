# Changelog

## 1.8.5 — 2026-09-23

### Fixes
- `fetch` now returns the readable text of web pages instead of raw HTML, so long pages no longer lose their middle to output truncation.

## 1.8.4 — 2026-09-21

### Features
- Extension server status & control: port scan discovery (8780–8790) from
  background worker with `/version` probe, status dot in popup, start command
  copy, fixed port and token control in Options.
- Fixed-engine profile: the client can save a server address + token as a
  "fixed" profile (`localStorage bebok.remote.profileKind`). When fixed, the
  Start screen skips the address form and shows a status dot with a "Use
  different server" link; the sidebar footer gains a Connect/Disconnect
  toggle. A deliberate disconnect blocks automatic reconnection until the
  user reconnects manually.
- SSE exponential backoff: reconnection attempts after a dropped stream grow
  from 1.5 s to a 30 s cap and reset on live. The `visibilitychange` event
  triggers an immediate retry when the user returns to the tab.
- `bebok-server --token <val>`: stable, predictable engine token via CLI flag
  (highest precedence: `--token` > `BEBOK_TOKEN` env > token file > random);
  `--port` fallback from `BEBOK_PORT` env with u16 validation; readable
  `AddrInUse` error when the port is occupied; dynamic CORS origin for
  non-default ports; `scripts/bebok.mjs` passes `BEBOK_PORT` through to both
  `cmdFullBuildDev` and `cmdEngine`.
- Companion extension: pinned `BEBOK_TOKEN` + **Search local Bebok** that
  probes the Engine URL, `8787` and the last known port with the pinned
  token and saves the working URL — auto-reconnect after restarts with no
  more pasting. New **Pinned token** field in Options; `BEBOK_PORT` pins
  the desktop sidecar to a fixed port; `scripts/bebok.mjs` passes both
  through. Details in the extension README ("Stable token + port").

### Fixes
- Companion extension Options: new **Search local Bebok** button probes the
  Engine URL field (or `127.0.0.1:8787`) and saves it when the engine needs
  no token; otherwise it says whether to paste the fresh `BEBOK_READY` URL
  or start the engine. A positive **Test connection** now saves both fields
  automatically. (Token auto-handoff for desktop random-port mode is still
  open — the scan deliberately never guesses tokens.)
- Remote browser piloting reconnects by itself after an engine restart
  (heartbeat re-registers a wiped registry), rejects commands to a stale
  extension with a clear wake-up hint instead of a 30 s hang, and no longer
  breaks on `tabs` with an empty or legacy body. The Companion Options
  page shows live connection proof with accurate error hints (stale token
  vs unreachable engine vs asleep worker).
- Companion extension requests `<all_urls>` host access: without it Chrome
  refuses script injection into ordinary tabs ("Extension manifest must
  request permission to access this host") and remote commands
  (`getPageContext`, `click`, `type`, screenshots) fail everywhere except
  activated tabs. After updating, reload the extension (↻) and re-allow
  site access if Chrome asks.

## 1.8.3 — 2026-09-21

### Features
- Remote browser piloting (Chrome extension preview): the agent can drive
  the user's active tab through a pull queue (register, heartbeat, pending,
  result) with `browser.remote.enabled` routing. Multiple commands in flight
  per session (waiters keyed by command id).
- Chrome companion extension: new `tabs` remote command listing open tabs
  (`{id, title, url, active, windowId}`, optional `chrome.tabs.query`
  filter) without switching tabs. The extension README now documents the
  Options status texts, the stale-Engine-URL trap in desktop mode (random
  port + fresh token per launch), the MV3 service-worker sleep (~35 s) with
  wake-up steps, and the unpacked-reload step after editing extension files.
- Skills: new bundled layer — `skills/<name>/SKILL.md` next to the engine
  binary (repo `skills/` dir in dev) is loaded first and can be overridden
  per-user (`~/.config/bebok/skill/`) or per-project (`.bebok/skill/`).
  Ships with the `remote-browser-extension` skill (pilot the user's own
  Brave/Chrome tabs via the Companion extension). Toggles in
  Settings → Skills work for bundled skills as before.

## 1.8.2 — 2026-09-18

### Features
- Skills: new bundled layer — `skills/<name>/SKILL.md` next to the engine
  binary (repo `skills/` dir in dev) is loaded first and can be overridden
  per-user (`~/.config/bebok/skill/`) or per-project (`.bebok/skill/`).
  Ships with the `remote-browser-extension` skill (pilot the user's own
  Brave/Chrome tabs via the Companion extension). Toggles in
  Settings → Skills work for bundled skills as before.
- Schedules: new built-in scheduler for recurring tasks (interval / daily /
  weekly per project, with prompt + agent preset, toggle, run-now and run
  history). The sidebar button now opens Schedules instead of Stats; the
  Stats view moved to a card in Settings → General.

### Fixes
- Plugins: the `list_install_toggle_roundtrip` engine test no longer needs
  network access (it seeds the plugin slot with a fixture manifest), so the
  Windows CI job is not at the mercy of the live registry download.
- Remote browser piloting (Chrome extension preview): the agent can drive the
  user's active tab through a pull queue (register, heartbeat, pending,
  result) with `browser.remote.enabled` routing in the engine. Multiple
  commands in flight per session (waiters keyed by command id);
  `extension_port` is not yet used by the pull flow.
- Chrome companion extension: new `tabs` remote command listing open tabs
  (`{id, title, url, active, windowId}`, optional `chrome.tabs.query`
  filter) without switching tabs. The extension README now documents the
  Options status texts, the stale-Engine-URL trap in desktop mode (random
  port + fresh token per launch), the MV3 service-worker sleep (~35 s) with
  wake-up steps, and the unpacked-reload step after editing extension files.
- New `remote-browser-extension` skill (Settings → Skills toggle): teaches
  the agent when to pilot the user's own Brave/Chrome tabs via the Companion
  extension pull queue (`POST /browser/remote/*` with
  `session_id=extension`) instead of the built-in Chromium, including the
  503-asleep / 404-unregistered / 401-stale-URL failure modes. The skill now
  ships bundled with Bebok (see above) instead of living in `.bebok/`.

## 1.8.1 — 2026-09-18

### Features
- Index: the code index now rebuilds automatically a few seconds after file
  writes (debounced per project directory, fire-and-forget); the manual
  "Rebuild index" button remains as a fallback.
- Config: new `allowed_paths` key (global or project config) — an allowlist of
  directories for multi-workspace ("hub") setups. It is parsed, resolved into
  the effective config and preserved through config edits (JSONC round-trip,
  project layer replaces the global list). Not yet enforced — gate-keeping of
  tool calls arrives in a follow-up.

## 1.8.0 — 2026-09-17

### Features
- Chrome companion extension (`client/chrome-extension`, MV3, unpacked): ask the
  local engine about the page you are reading — popup with question +
  attach toggles (URL/title, selection, page text, screenshot), right-click
  "Ask Bebok about selection", options page with Test connection. See
  `client/chrome-extension/README.md` for install (paste the `BEBOK_READY` URL).

### Fixes
- Plugins: `code_index_status`/`code_index_search` now lazy-register
  `bebok-index` from its on-disk declaration + slot, so they keep working
  right after an engine restart without a prior status call; a crashed or
  timed-out plugin reports `{ok:false,error}` (instead of a misleading
  "not registered"), the subprocess keeps a persistent stdout reader
  (no lost responses on back-to-back calls), stderr is captured on
  failure (bounded, for diagnostics), and the binary resolution is cached.
- Plugins: `prompt_file` from the slot manifest is validated (plain file
  name only — absolute paths, `..` and separators are rejected and fall
  back to `AGENT_INDEX.md`); it is now also a typed `PluginManifest` field.
  A corrupt declaration is fail-closed (treated as disabled) everywhere —
  tools, prompt injection and `is_disabled` agree.
- Plugins: `GET /plugins` no longer claims `binary:"present"` for the
  typeless conversion and `installed` is consistently `is_dir()`; toggle ON
  re-registers the plugin; `POST …/update` shares the `slot_state` binary
  check (manifest/entrypoint aware); `GET …/status` forwards the
  normalized instance root; `GET /plugins/registry` returns
  `{plugins,cached}`; invoking a reserved action name
  (`install|update|toggle|status`) is a 400.
- Plugins (Settings → General): rebuild no longer reports success on
  `ok:false` (sets the inline error instead), the Update label shows the
  registry version (`Update vX` only when it differs from the installed
  one), toggle/install/update refresh the index card, three quiet-poll
  failures in a row show `unknown` instead of a stale `ready`, and the
  status dot + version got `data-testid`s.

- Plugins: Settings → General now has an **Update** button on every installed
  plugin. It re-resolves the plugin's release and downloads the binary for
  your platform when it is missing, so a plugin that was installed without an
  asset for your OS can be repaired in place (the button becomes the primary
  action and the row shows the available version); the plugin's version is
  shown in the button label.

### Fixes
- Plugins: updating a plugin whose platform binary is unavailable now reports
  a clear "binary missing for this platform" message (engine answers
  `503 binary_missing`, and `no_asset_for_platform` / `offline_fallback` /
  `checksum_mismatch` get their own messages) instead of a confusing
  `404 Not Found`.

## 1.7.2 — 2026-09-16

### Fixes
- **engine**: spawn dynamic plugin subprocesses reliably on Windows (7346c19)

## 1.7.1 — 2026-09-16

### Fixes
- Fix: dynamic plugin subprocesses now spawn reliably on Windows
  (`split_entrypoint` honours quotes so script paths with spaces from
  `%TEMP%` survive, `where` takes the first match line, test stubs prefer
  `pwsh` with a `powershell` fallback). Fixes the two
  `routes::plugins` invoke roundtrip failures on CI windows-latest.
- Settings → Agents: saving "Frontend verification" no longer wipes a
  previously saved "Build & test" value (and vice versa). Each card now
  merges the sibling `verify.*` key into its `PUT /config` delta, because
  the engine replaces the whole top-level `verify` section.

### Features
- Searchable model picker: every model dropdown (chat toolbar, new-session
  dialog, agent model override, fleet members) is now a filterable combobox
  with models grouped by provider, so long lists like OpenRouter's can be
  searched instead of scrolled.
- Native `code_index_status` and `code_index_search` tools: in-process tools
  in `bebok-core` that delegate to the `bebok-index` plugin, enabling the
  model to query the local code index (tantivy) directly from the agent loop.
  Both tools are read-only, abort-aware, and registered for all agent presets.
- Central enforcement of code-index-first: every prompt (user sessions and
  delegated sub-agents via task/fleet) now includes a "Code index first" section
  when the `bebok-index` plugin is available. The section instructs the model to
  query the local code index before falling back to `grep`/`glob`. The prompt
  text is read from the plugin's `AGENT_INDEX.md` (or a custom file named in
  `bebok-plugin.json`'s `prompt_file` field), with a built-in fallback when the
  file is missing. Injection happens in `apply_request_hook` so it covers all
  code paths — no prompt bypasses it.
- `GET /stats` now includes a `delegation` object with `direct` and `delegated` totals (tokens, cost, calls), splitting top-level sessions from sub-agent sessions by `parent`. Each `top_sessions` row carries a `parent` field (`null` or the parent UUID string).
- `ask` and `plan` presets now include `fetch` in their tool whitelist with an
  `Allow` permission rule, enabling read-only HTTP access for the local code
  index (`GET /plugins/bebok-index/status`, `POST /plugins/bebok-index/search`).
- Dynamic plugin loading: the engine can now spawn external plugin binaries
  as subprocesses (JSON-lines over stdio). `PluginProcess` manages the
  child lifecycle, `DynamicPlugin` wraps it behind `BebokPlugin`, and
  `PluginHost::invoke(name, action, input)` dispatches to the matching
  plugin. New HTTP routes `GET /plugins/{name}/status` and
  `POST /plugins/{name}/{action}` expose the protocol. Manifest
  `bebok-plugin.json` gains an optional `entrypoint` field (backward
  compatible). When the binary is missing, invocation returns `None`
  (graceful degradation, no panic).
- Orchestrator supervision: the orchestrator watches sub-agents for looping /
  wandering, cancels a stray child with `task_cancel` and re-delegates it with
  a corrective brief.

### Other
- BREAKING: removed the delegation mode / model_policy configuration —
  sub-agents spawn only from the fleet list, `delegation` keeps just
  `max_concurrent`, and `GET /delegation/models` is gone.

### Fixes
- Disabling a plugin via `POST /plugins/{name}/toggle` with `enabled: false`
  now immediately unregisters it from the global backend and blocks all access
  to its endpoints (`/plugins/{name}/status`, `/plugins/{name}/{action}`) and
  tools (`code_index_status`, `code_index_search`) for the project, even if
  the plugin is still registered globally for another project. Previously
  toggling a plugin off left the backend running and cross-project isolation
  was broken.
- `GET /plugins/{name}/status` and `POST /plugins/{name}/{action}` now
  automatically load and register declared, enabled, installed plugins on
  first access instead of returning 404. Previously the engine only
  registered plugins during startup, so a freshly installed plugin
  required a full restart to become reachable.
- Fix: plugin subprocesses are now spawned via their resolved binary path
  (slot dir or `PATH`) instead of the bare command name. Previously a
  registered plugin sitting in its slot dir failed to start with
  `No such file or directory`, because the OS resolves the binary via
  `PATH`, not via the child's working directory.
- Fix: `code_search` no longer appears twice in the tool list sent to the
  provider (DeepSeek rejected it with 400 "Tool names must be unique").
  `ToolRegistry::list()` now skips a built-in shadowed by a dynamic tool,
  same as `list_with_source()` already did.
- Fix: the code-index settings card now calls the correct plugin routes
  (`/plugins/bebok-index/status` and `/plugins/bebok-index/rebuild`) instead
  of the non-existent `/index/status` and `/index/rebuild`, which returned 404.

## 1.7.0 — 2026-09-14

### Auto-update
- Desktop shell: `tauri-plugin-updater` driven from Rust (`update_check`,
  `update_install` with `update://progress`, `relaunch_after_update`,
  `desktop_info`); the sidecar is killed in `on_before_exit` so the Windows
  installer can overwrite `bebok-server.exe`. Feed:
  `releases/latest/download/latest.json`, verified against
  `plugins.updater.pubkey`.
- Client: `UpdateStore` (check 10 s after start, then every 6 h; GitHub API
  fallback in browser mode / dev builds), non-modal `<app-update-banner>` with
  download progress, a version chip in the topbar (highlights `↑ x.y.z` when
  a newer release exists) opening the new Updates screen (`/updates`:
  versions, "Check for updates", install / download, release notes, recent
  releases from GitHub); install is blocked while an agent turn runs.
- Engine: `GET /version`.
- Release: `scripts/latest-json.mjs` composes `latest.json` in the `release`
  job; `TAURI_SIGNING_PRIVATE_KEY` is now required; macOS updater archives are
  named `bebok_<version>_<arch>.app.tar.gz`.
- Release process: `npm run release -- X.Y.Z` opens a `release/X.Y.Z`
  PR (changelog section + version bump); the PR builds a draft release, the
  merge tags and publishes it automatically (`release.yml` now also runs on
  `pull_request` from `release/**` and on `push` to `main`). Pre-release
  suffixes must be numeric (MSI). `CONTRIBUTING.md` documents the flow.

## 1.6.0 — 2026-09-13

Fleet generation, delegation roster, preview/explorer/chat upgrades,
about-page rewrite.

### Fleet generation
- New `fleet_gen` engine module: LLM-backed fleet generation from
  configured providers, with a default-member fallback and a
  generate endpoint surfaced in Settings.
- Settings > Agents gains "Generate fleet" (store + agents-tab +
  engine client/DTOs, strings in all 12 locales).

### Delegation
- The `Fleet:` paragraph of the delegation prompt is rendered from
  the resolved config (`FleetContext`), so the model sees the real
  member labels; agents without the `fleet` tool never see it.
- `task` / `fleet` tool wiring carries `childSessionID`, agent and
  effective model through structured output.

### Chat / Explorer / Preview
- Delegation tool rows: link to the child session, effective model
  display, collapsed-by-default rendering.
- Preview panel and Explorer upgrades (binary/media file support
  in `/fs`, UI improvements).
- Interrupted tool calls (Running/Pending after a crash or engine
  restart) are repaired to Error on session open so future prompts
  don't fail with "Missing tool response".

### Session & worktree
- `effective_model` / `effective_provider` on sessions and listings;
  worktree branch attached to worktree-backed sessions.
- Task abort cancels the child token and explicitly the parent
  orchestrator token (CancellationToken propagates parent→child only).

### About page
- Silesian bebok legend rewritten across all 11 locales; single
  `bebok.png` illustration replaces the two CC BY-SA JPGs.

## 1.5.0 — 2026-09-13

Rounds 2–4 after 1.4.1: 28 work packages merged on top of `13cf422`.
QA before release: engine 550 tests / 0 failures, client 576 / 0,
`cargo fmt` + `cargo clippy -D warnings` clean.

### Chat
- Every tool call is collapsed by default into a one-line row (status, tool, argument preview, COMPLETED/FAILED/RUNNING) with three-level expand (group → call → arguments/output); thinking blocks collapsed; "Expand tool calls by default" preference.
- Consecutive assistant turns that contain only tool calls are merged into one group with a single header and summed In/Out usage.
- Long transcripts render the last 60 messages with "Load earlier messages"; the scroll minimap is gone; scroll-to-bottom waits for the transcript to actually render.
- Context meter in the session toolbar and Session panel (tokens sent vs. the model window from the catalog); "Compact now" (toolbar + palette); automatic compaction at 85 % with a server-side marker message and before/after counts; cost shows "—" when pricing is unknown.
- Safety-level dots on tool calls, colour-coded by category, with a legend.
- Typed inline code chips (paths, commands, identifiers) in assistant text.
- Syntax highlighting via highlight.js shared by chat, Preview, Explorer and the diff view.
- Bare URLs are clickable in chat, Preview and tool output; ATX headings render; model text is HTML-escaped.
- The effective model is shown everywhere (session, sub-agents, tool rows).

### Panels
- **Changes**: files modified by tools in the session, unified/split diff against git HEAD or a first-write snapshot, "Open in Explorer", confirmed revert; aggregates sub-agent sessions tagged by agent, with diff/revert reaching the child tracker. Replaces the heuristic FILES CHANGED list.
- **Preview**: Markdown renderer with file-name header, toolbar (file picker, Explorer, refresh, pin), empty state and modified-on-disk notice; file paths and `.md` links in assistant text / tool output open in it; per-file "Open in preview" in the drawer Explorer.
- **Agents**: live sub-agent list (`GET /session/{id}/agents`, refreshed on `task.*` SSE) with a read-only streaming transcript overlay; the whole card opens the transcript, "Open session" as secondary action; nested sub-agent sessions with a parent breadcrumb.
- **Browser**: latest screenshot and URL from the browser tools, "Open in window".
- **Processes**: background `bash` processes with log tailing and kill-tree.
- Right drawer: one scroller, sticky collapsible panel headers, 320 px default width, reveal requests from links; clicking a drawer Explorer file opens the full-screen Explorer; sidebar icon rail.

### Agents & verification
- Sub-agent delegation mode with supervision: `delegation.mode` / `max_concurrent` / `model_policy` (`inherit` | `cheaper` (default, catalog-based cheaper-sibling mapping) | explicit), background tasks with `task_status` / `task_wait` / `task_cancel`, per-session concurrency gate, throttled `task.progress` events, Delegation block in Settings > Agents; sub-agents get no delegation tools under a policy.
- Token-free progress rows in the parent transcript plus a narration policy; live progress on completed background task rows and the active-tasks block.
- Autonomous frontend verification policy (`verify.frontend`, "Frontend verification" card in Settings > Agents): start every dependency on its own non-default port, wait for readiness, screenshot the loaded page before interacting, check the API, fix and re-verify, strict FAIL conditions and an acceptance gate.
- `bash` gains `background: true` with a process registry, readiness waits (`ready_port` / `ready_text` / `ready_timeout`) and `bash_kill` without prompts.
- "Always allow this tool" persists a tool-level project rule that sticks for parent and sub-agent sessions.

### Projects & git
- Project groups in the registry (`PATCH /projects/{id}` with `group`) and a grouped, collapsible project switcher; PATCH allowed through the engine CORS layer.
- Git awareness: `GET /projects/{id}/git` (repo, branch, remote, GitHub, dirty count) backed by a git CLI module in `bebok-core`.
- Worktree-backed sessions: new-session dialog (agent / model / "Run in a git worktree") creating a branch under `.bebok/worktrees/` (gitignored), branch badge in the sidebar, worktree removal offered on delete (`DELETE /session` reports `is_worktree`).

### Browser tools
- `browser_open` / `screenshot` / `click` / `type` / `get_text` / `eval` / `console` / `wait` / `find` driving an installed Chrome/Edge/Chromium through CDP (`chromiumoxide`); screenshots reach the model as image parts; the browser is closed with the session.
- Display modes headed / viewer / drawer, a viewer window with a live frame stream and viewer API, HiDPI-correct captures, "Browser display" setting.
- `browser_*` defaults to Ask even when read-only; `browser_console` / `wait` / `find` are classified safe and auto-allowed under the verification policy.

### Stats
- `GET /stats`: session-digest scan with filtered aggregation and an event-invalidated cache.
- Stats screen: sortable tables, CSS daily chart, projects grouped by normalised directory, sidebar and palette entries; refreshes on the end-of-turn `session.updated` event.

### Settings / Safety
- Explicit tool safety categories exposed by `GET /tools/safety`, "Tool safety" settings with category colours and legend.
- Settings > Agents: Delegation and Frontend verification cards.
- Reconnect prompt when the engine token is rejected (401); token handling for the new endpoints.

### About page
- "What is Bebok?" page with the Silesian legend.

### Scripts / CI
- Tag-triggered multi-OS release workflow (`.github/workflows/release.yml`): preflight version check across all four manifests, linux-x64 / windows-x64 / macos-arm64 / macos-x64 matrix, standalone `bebok-server` binaries, portable archives, merged `SHA256SUMS.txt`, optional Authenticode / GPG / Tauri-updater signing gated on secrets, refusal to overwrite a published release from `workflow_dispatch`; runbook in `scripts/release.md`.
- `npm run version:bump -- x.y.z` edits every manifest and lockfile; `npm run full-build-dev` / `full-build-app` / `doctor` cross-platform orchestration in `scripts/bebok.mjs`; client adopts `?engine=<url>` bootstrap on first load.

### Fixes
- JSONC writer: `with_set` replaced nothing after an array of strings and appended duplicate top-level keys.
- Preview link interception broken by the innerHTML sanitizer stripping `data-*`; `browser_open` accepted `https:///path`; tool-run row lost its manual expand state while streaming; Agents panel double-fetched on mount.
- Page-level overflow from the status-dot hidden label; session toolbar fields overlapping at narrow widths; Stats daily chart collapsing and tables clipping their last column; Changes panel rows keep the basename visible.
- Slash-separated Explorer paths and reveal of the opened file in the tree; Chrome launch switches passed with a double `--` prefix; chromiumoxide per-event WS deserialisation warnings silenced.
- Reasoning no longer re-sent with tools to models that reject it; vendored models.dev snapshot extended with current-generation models.
- Tests: server test apps use an isolated data dir (they leaked sessions into `%APPDATA%\bebok`); browser round-trip test uses a free ephemeral port.

Known limitations: the Linux, macOS arm64/x64 legs and the MSI bundle of the
release workflow had never run in CI before this release; macOS builds are
unsigned (Gatekeeper warns), Windows binaries are unsigned (SmartScreen
warns). New i18n keys in the 11 non-English locales are machine translations.


## 1.4.1 — 2026-09-12

- Lista sesji natychmiast pokazuje aktualny tytuł i użycie tokenów; przełączenie projektu czyści poprzednie sesje i odrzuca spóźnione odpowiedzi.
- Wiadomości oczekujące w kolejce zachowują agenta i model wybrane przy wysłaniu.
- Dostosowano dwa testy ścieżek do krótkiej postaci katalogu tymczasowego Windows; dodano trzy anglojęzyczne zrzuty aplikacji do README.

## 1.4.0 — 2026-09-12

- Poprawiono wybór katalogu projektu w aplikacji przeglądarkowej i Tauri, z uwzględnieniem ścieżek Windows i Linux oraz ponownego połączenia eksploratora po zmianie projektu.
- Ujednolicono wersje klienta, silnika i aplikacji Tauri. Aplikacja desktopowa uruchamia dołączony silnik jako sidecar na lokalnym porcie.
- Poprawiono obsługę modeli OpenAI: parametr limitu tokenów, wywołania narzędzi z trybem rozumowania, katalog modeli i obsługę błędów providera.
- Naprawiono ponawianie wiadomości, skrócono odstępy w czacie oraz zaktualizowano logo i ikonę aplikacji Windows.
- Wzmocniono ochronę konfiguracji projektu przy operacjach na plikach i poprawiono obsługę poleceń oraz terminala na Windows.

Pakiety Windows są budowane z `engine/` przez `cargo build --release`, a następnie z `client/` przez `npm run sidecar:copy` i `npm run tauri:build`. Instalatory NSIS/MSI, samodzielne pliki EXE i sumy SHA-256 są załączane do wydania GitHub.

Znane ograniczenie: wybór katalogu na pulpicie Linux wymaga jeszcze ręcznego testu w sesji GTK/KDE; kompilacja na Linux jest sprawdzana przez CI.
