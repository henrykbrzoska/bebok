# CLI agents

Bebok can run a turn — or a delegated sub-agent — through an external agentic
CLI instead of an HTTP API. Claude Code is the first backend; the design makes
adding another one a single file.

## Why a separate crate

```text
bebok-llm        ProviderKind::Cli (enum variant only, no process logic)
   ▲  depends on
bebok-cli-agents trait CliBackend + process runner + CliProvider: bebok_llm::Provider
   ▲                       └── backends/claude.rs (and future ones)
   │  depends on
bebok-core       provider.rs: dispatch on ProviderKind + project root
   ▲
bebok-server     routes/cli.rs (GET /cli/backends, POST /cli/backends/{id}/probe)
```

`bebok-llm` speaks HTTP protocols and owns no processes. `bebok-cli-agents`
depends on it (`Provider`, `StreamEvent`, `Usage`) and adds the process layer.
There is no cycle: llm ← cli-agents ← core ← server.

## The one hard rule

**A CLI's tool activity is never a `StreamEvent::ToolCall`.**

An agentic CLI performs its own tool use: it runs Bash, edits files, reads the
tree. It cannot be handed Bebok's tool registry the way `ChatRequest.tools`
hands one to an HTTP model. If CLI tool use surfaced as `StreamEvent::ToolCall`,
the agent loop (`bebok-core/src/agent/turn.rs`) would try to execute it through
`exec.rs` against a registry that never defined those tools — a failed turn at
best, a hallucinated tool at worst.

So a CLI is a **transport** provider. `CliEvent::ToolActivity` is a presentation
event, mapped in v1 to `StreamEvent::Thinking` (the client already renders that
as a collapsible section).

## Known limitations (by design, not bugs)

| Limitation | Consequence |
|---|---|
| Bebok's permission engine (`permission/`) does not cover CLI tools | The only guard is the CLI's own flags (`--permission-mode`, `--disallowedTools`). |
| `change_tracking.rs` sees no CLI edits | Those edits are missing from the Changes panel (no `write_file` hook fires). A post-turn git snapshot is the planned fix. |
| CLI models are absent from `model_catalog.rs` | No context window or pricing table; the context budget comes from the `Usage` the CLI reports. |
| v1 has no `--resume` | The whole transcript is re-sent every turn. Correct, but more expensive than it needs to be. |

## Configuration

A CLI is an ordinary provider entry — no new config section. `extra` is already
round-tripped verbatim by the JSONC writers, so nothing in the loader or the
writers had to change.

```jsonc
{
  "providers": [
    {
      "name": "claude-cli",
      "kind": "cli",
      "models": ["opus", "sonnet", "haiku"],
      "extra": {
        "backend": "claude",
        "mode": "agent",
        "allow_write": false,
        "timeout_secs": 900
      }
    }
  ],
  "model": "claude-cli/sonnet"
}
```

| `extra` key | Default | Meaning |
|---|---|---|
| `backend` | *(required)* | Backend id from the registry (`claude`). Missing is an error, never guessed. |
| `command` | — | Explicit binary path, bypassing discovery. |
| `args` | `[]` | Extra flags appended verbatim, last (so they can override defaults). |
| `mode` | `agent` | `agent` or `text`. |
| `allow_write` | **`false`** | Whether the CLI may modify files. |
| `timeout_secs` | `900` | Per-turn hard timeout, clamped to 30–7200. |
| `max_turns` | — | Forwarded as `--max-turns`. |

Because `provider_auth` returns `ProviderAuth::None` for `kind: "cli"`, the
provider needs no API key, `is_configured()` is true without one, and the
settings UI hides the key and base-URL fields.

## Security

- `allow_write` defaults to **false**. The Claude backend then emits
  `--permission-mode plan` *and* `--disallowedTools Edit Write Bash` — belt and
  braces, because a permission mode alone is one config key away from wrong.
- `--permission-mode bypassPermissions` and `--dangerously-skip-permissions` are
  **never** emitted, even with `allow_write: true` (`acceptEdits` still keeps the
  CLI's shell prompts).
- The CLI always runs with `current_dir` set to the instance root. No `--add-dir`
  is added without explicit configuration.
- `spec.api_key` is never passed to the child process; the CLI has its own
  authentication.
- A hard timeout plus a tree-wide kill on drop, so an aborted turn does not
  leave a zombie CLI holding the project open.

## Adding a new CLI

1. Create `engine/crates/bebok-cli-agents/src/backends/<name>.rs`.
2. Implement `CliBackend`: `id`, `label`, `capabilities`, `default_models`,
   `binary_names`, `env_override`, `build_command`, `parse_line`.
3. Add one line to `all_backends()` in `backends/mod.rs`.
4. Add unit tests over recorded JSONL lines (never require the real binary in CI).
5. If a new backend needs a change **anywhere outside its own file**, the trait
   is wrong — fix the trait, not the backend.

`parse_line` should be forgiving: an unknown `type` or a non-JSON line returns
`Ok(None)` with a `tracing::debug!`, because CLIs add fields between releases
and a stray log line on stdout must not fail a turn.

## Platform notes

- On Windows, npm installs Claude Code as `claude.cmd`. The runner detects a
  `.cmd`/`.bat` and wraps it as `cmd /C <program> <args…>`; `discovery.rs` tries
  `name.exe`, `name.cmd`, `name.bat`, then the bare name, honouring `PATHEXT`.
- The child is spawned as a new process group (Unix `process_group(0)`, Windows
  `CREATE_NEW_PROCESS_GROUP`), so the kill hits the whole tree — the npm wrapper
  spawns a `node` grandchild that a plain `kill` would orphan.
- `--disallowedTools` takes a variadic value list. Claude Code therefore needs a
  `--` separator before the prompt, or the prompt is swallowed as another tool
  name.

## Endpoints

- `GET /cli/backends` — registered backends, whether the binary was found, its
  version, capabilities and default models. Cached ~60 s.
- `POST /cli/backends/{id}/probe` — runs `--version` on demand for the settings
  "Test" button and returns stdout/stderr.
