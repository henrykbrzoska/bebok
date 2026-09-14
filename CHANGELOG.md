# Changelog

## Unreleased

### Mobile, relay and cloud chats
- The 1.6 mobile work lands: engine remote module (second listener on
  Tailscale/LAN, device tokens with an exhaustive route allowlist, QR
  pairing, SSE resync), the phone shell (`/m/**`: Chat, Remote, Agents,
  Changes, More), Android build with a foreground service.
- **Relay** (`relay/`, Cloudflare Worker + Durable Object): the engine keeps
  an outbound WebSocket to the worker and paired phones reach it over HTTPS
  from anywhere - no Tailscale or port forwarding. Same tokens and
  permissions as on the LAN. Settings -> Remote -> Relay (URL, toggle,
  status, Reset tunnel); the QR code advertises the relay endpoint and the
  phone roams between direct endpoints and the relay.
- **Cloud chats**: a cloud toggle in the chat toolbar mirrors that session
  to the relay after every turn (meta + newest messages); the phone lists
  and reads mirrored chats while the desktop is offline.
- Engine: `POST /remote/relay`, `POST /remote/relay/reset`,
  `POST /session/{id}/cloud`; `GET /remote/status` gains `relay`; partial
  `PUT /config` bodies now deep-merge instead of replacing a section;
  pairing works with a relay-only engine.

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
