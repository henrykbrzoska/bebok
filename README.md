# Bebok

A local-first AI coding agent, built from scratch in Rust + Angular.
The engine runs without a cloud, and you keep full control over permissions, tools and configuration.

> **License**: source-available (proprietary). Not open source (yet) - see [LICENSE](./LICENSE).

## Status

| # | Milestone | Status |
|---|-----------|--------|
| 1 | Headless engine (config, sessions, Z.ai/GLM, tools, agent loop, HTTP+SSE) | ✅ |
| 2 | Permission engine (globset, `ask`, decision cache) | ✅ |
| 3 | Angular client (chat + SSE, permission popups, directory picker) | ✅ |
| 4 | Agents (presets + hot reload), skills/AGENTS.md, MCP bridge (rmcp) | ✅ |
| 5 | Terminal (PTY, xterm.js) | ✅ |
| 6 | Explorer, context compaction, mobile (Capacitor), more providers | ✅ |

Details in [docs/milestones](./docs/milestones). The authoritative specification is [SPEC.md](./SPEC.md).

## Features

- **Rust engine + SSE** - axum, tokio; all logic and state live in the engine (the GUI only renders).
- **Agent loop** - streaming + tool calls across **many providers** (OpenAI, Anthropic, Z.ai/GLM, xAI, DeepSeek, Google, Mistral, Groq, Qwen, OpenRouter, Ollama), selected by model prefix.
- **Permissions** - `globset` patterns (`tool(args)`), `allow`/`deny`/`ask` verdicts, decision cache, global + project rules.
- **Built-in tools** - `read_file`, `write_file`, `bash`, `glob`, `grep`, `list_dir`, `tree`.
- **MCP** - `rmcp` bridge (stdio + streamable HTTP); MCP tools join the shared permission gate as `mcp__<server>__<tool>`.
- **Agents** - built-in presets (`code`, `ask`, `plan`, `debug`, `orchestrator`) + files in `~/.config/bebok/agent/*.md` and `<project>/.bebok/agent/*.md` with hot reload.
- **Skills & AGENTS.md** - global/project `AGENTS.md` + `skill/*/SKILL.md` appended to the system prompt (with toggles).
- **Explorer** - gitignore-aware lazy file tree + preview/edit served through the engine (`/fs/*`).
- **Debug log** - a single `debug.log` file (cleared on startup, 10 KB cap) logging LLM + HTTP requests/responses, shown in the Debug tab.
- **Plugins** - in-process event-observer API: any `BebokPlugin` observes every bus event and can hook (mutate/veto) the agent loop at `before.request`, `before.tool`, `after.tool`, `turn.end`, `permission.resolved` (register code-side; introspect via `GET /plugins`).
- **Context management** - tool-output truncation, pruning, and compaction (internal fork; full history always available).
- **Sessions** - fork / `continueLast` / export; sidebar (tokens, prompt-cache, cost, files changed).
- **Configuration** - JSONC (with comments), per-agent-type model overrides, provider registry, interpreter paths (python/python3/node/php/docker), Docker access check.
- **Thinking effort** - configurable reasoning level (`off`/`low`/`medium`/`high`/`max`), mapped per provider (`reasoning_effort` for OpenAI-compatible, `budget_tokens` for Anthropic/Z.ai).
- **Terminal** - engine-owned PTY (portable-pty, scrollback, resize, one-time tickets) + xterm.js tabs with reattach (read-only on mobile).
- **i18n** - 12 languages, default English.

## Tech stack

- **Engine**: Rust (axum, tokio, rmcp, globset, notify, ignore, reqwest, serde).
- **Client**: Angular 20 (standalone components, signals, zoneless).
- **Desktop**: Tauri 2 (shell spawning the engine as a sidecar).
- **Mobile**: Capacitor (thin client → remote engine over LAN). //@todo no remote

## Structure

```
agent-rs/
├── engine/                 # Rust workspace (engine)
│   ├── crates/
│   │   ├── bebok-core/    # config, sessions, agent loop, permissions, store, context mgmt
│   │   ├── bebok-server/  # axum HTTP + SSE
│   │   ├── bebok-tools/   # Tool trait + built-in tools + runtimes + explorer
│   │   ├── bebok-llm/     # Provider trait + OpenAI/Anthropic/Z.ai/OpenAI-compatible clients
│   │   ├── bebok-mcp/     # MCP bridge (rmcp)
│   │   ├── bebok-skills/  # AGENTS.md + skills + frontmatter
│   │   └── bebok-pty/     # PTY manager (M5): scrollback, resize, tickets, Job Object
│   └── Cargo.toml
├── client/                 # Angular 20 (web) + Tauri 2 (desktop) + Capacitor (mobile)
│   ├── src/
│   │   ├── views/          # start/, chat/, explorer/, terminal/, settings/, connect/
│   │   └── i18n/           # translation files (en default)
│   ├── capacitor.config.ts # mobile shell (webDir -> dist/bebok/browser)
│   └── src-tauri/
├── docs/milestones/        # execution plan
├── SPEC.md                 # specification
└── LICENSE
```

## Requirements

- Rust ≥ 1.85 (edition 2024)
- Node.js ≥ 20 + npm
- (desktop) Tauri 2 system dependencies - see [tauri.app](https://tauri.app/start/prerequisites/)

The engine and client build on **Linux and Windows**. CI
([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) builds and tests the
engine plus the client on both `ubuntu-latest` and `windows-latest`.

## Running

### 1. Engine (headless)

```bash
cd engine
ZAI_API_KEY=<key> cargo run
```

- listens on `http://127.0.0.1:8787` (no password).
- provider API keys can be set via env vars (`ZAI_API_KEY`, `OPENAI_API_KEY`,
  `ANTHROPIC_API_KEY`, ...) or in the project/global config (see below).

### 2. Client (browser mode)

```bash
cd client
npm install
npm start            # http://localhost:4200
```

Enter the engine address (`http://127.0.0.1:8787`) and pick a project directory.

### 3. Desktop (Tauri)

```bash
cd engine && cargo build --release
cd ../client && npm run sidecar:copy && npm run tauri:build
```

- `tauri:build` compiles the frontend, bundles the engine sidecar and produces a
  platform binary + installers. `bundle.targets` is `"all"` in `tauri.conf.json`,
  which selects the per-OS defaults:
  - **Windows**: `bebok-desktop.exe` + NSIS/MSI installers
  - **Linux**: `bebok-desktop` + AppImage/DEB packages
  - **macOS**: `bebok-desktop` + APP/DMG bundles
- **Windows notes**: run `npm` as `npm.cmd` (PowerShell's execution policy
  blocks the `npm` shim); if `tauri build` errors at startup about a missing
  native binding, install it first:
  `npm i -D @tauri-apps/cli-win32-x64-msvc`.
- for development use `npm run tauri:dev`.

### 4. Mobile (Capacitor) //@TODO

```bash
cd client && npm run build && npx cap sync
npx cap open ios     # or: npx cap open android
```

The mobile client connects to a remote engine over LAN (see the `connect` screen);
it has the same full feature set as the desktop client (terminal included).

### Tests

```bash
cd engine && cargo test --workspace   # engine
cd ../client && npm run build         # client (build)
```

## Configuration

Configuration is JSONC, loaded in layers: global `~/.config/bebok/config.json`
→ project `<project>/.bebok/config.json` (overrides global; providers merge by
name). Sections:

```jsonc
{
  "model": "zai/glm-5.3-flash",    // default model (prefix selects the provider)

  // per-agent-type model overrides (code / ask / plan / debug / orchestrator)
  "models": {
    "plan": "anthropic/claude-sonnet-4-5"
  },

  // provider registry: name, kind, endpoint, api_key (empty = env var), models
  "providers": [
    { "name": "openai", "kind": "openai", "endpoint": "https://api.openai.com/v1", "api_key": "" },
    { "name": "ollama", "kind": "openai", "endpoint": "http://localhost:11434/v1", "api_key": "" }
  ],

  "api_key": "...",                  // fallback provider key
  "max_tokens": 8192,
  "thinking": "off",               // reasoning effort: off | low | medium | high | max
  "context_budget": 64000,           // tokens before pruning/compaction
  "tool_output_cap": 32768,          // per-tool-output truncation (bytes)

  "permission": { "rules": [
    { "pattern": "bash(git *)", "action": "allow" }
  ]},

  "mcp": {
    "filesystem": { "transport": "stdio", "command": "npx", "args": ["-y", "..."], "enabled": true },
    "github":     { "transport": "http", "url": "https://...", "headers": { "Authorization": "..." }, "enabled": false }
  },

  "skills": { "commit-helper": false },

  "runtimes": {
    "python": "/usr/bin/python3",
    "python3": "/usr/bin/python3",
    "node": "/usr/bin/node",
    "php": "/usr/bin/php",
    "docker": "/usr/bin/docker",
    "git": "/usr/bin/git"
  }
}
```

Most of this (model, per-type models, providers + "check available models",
API key, permission rules, MCP, skills, interpreter paths, Docker check) is
editable in the GUI: **Settings**.

## Endpoints (engine)

| Method | Path | Description |
|--------|------|-------------|
| POST/GET | `/session` | create (`continueLast`/`forkOf`) / list sessions |
| GET | `/session/{id}` | session meta |
| GET | `/session/{id}/message` | messages |
| POST | `/session/{id}/prompt` | start a turn (202) |
| POST | `/session/{id}/abort` | abort a turn |
| GET | `/session/{id}/export` | full JSON export (meta + transcript) |
| POST | `/session/{id}/compact` | compact as an internal fork |
| POST | `/session/{id}/permission/{requestID}` | permission decision |
| GET | `/agent` | agents (presets + files) |
| GET | `/mcp` | MCP servers + status |
| POST | `/mcp/{name}/toggle` | enable/disable an MCP server |
| GET/PUT | `/config` | resolved config |
| GET | `/docker` | Docker access probe |
| GET | `/models?directory=&provider=` | list a provider's available models |
| GET | `/fs/tree?directory=&path=` | gitignore-aware lazy file tree |
| GET/PUT | `/fs/file?directory=&path=` | file content (viewer / edit) |
| GET/DELETE | `/debug/log` | debug log (LLM + HTTP requests/responses) |
| GET | `/plugins` | registered plugins + exposed hook points |
| GET | `/event` | SSE stream |
| POST/GET | `/pty` | create / list terminal sessions |
| POST | `/pty/{id}/ticket` | one-time connect ticket |
| GET | `/pty/{id}/connect?ticket=` | WebSocket upgrade (terminal) |

## License

Source-available, proprietary - **not** open source (yet). See [LICENSE](./LICENSE).
