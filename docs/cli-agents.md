# CLI agents

Bebok can run a turn — or a delegated sub-agent — through an external agentic
CLI instead of an HTTP API. Claude Code is the first backend, Grok Build the second; the design makes
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
bebok-server     routes/cli.rs (GET /cli/backends, POST /cli/backends/{id}/probe,
                 GET /cli/profile/{provider}), routes/cli_hook.rs (permission hooks)
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

So a CLI is a **transport** provider. Its tool use is presentation only:
paired `tool_use`/`tool_result` blocks become `StreamEvent::ExternalTool` (see
[Native tool rows](#native-tool-rows)), and unpaired activity becomes
`StreamEvent::Thinking`.

## Known limitations

Status as of the CLI-parity work (`docs/plans/cli-parity/`):

| Limitation | Status |
|---|---|
| Bebok's permission engine covers CLI tools only through the managed profile's hooks. In `extra.profile: "user"` no Bebok hooks are written, so only the CLI's own flags (`--permission-mode`, `--disallowedTools`, `--allowedTools`) apply. | Still true for `user` mode. Solved for `bebok` and `isolated` (see [Permission hooks](#permission-hooks)). |
| Files a CLI shell command writes are not tracked. `Edit`, `Write`, `MultiEdit` and `NotebookEdit` are snapshotted before they run, but `Bash` is not, and nothing is snapshotted after the turn. Such files do not show in the Changes panel. | Still true. |
| A CLI cannot call Bebok's own tools (`code_index_*`, `code_ast`, `browser_*`, `ssh`, Bebok's `fetch`). | Still true. The MCP bridge that would expose them is planned (phase 04), not implemented. |
| CLI cost is what the CLI reports (`total_cost_usd`). Bebok does not price CLI turns. | By design. The context window is resolved from `model_catalog.rs` through the CLI's `system/init` model id. |
| The hook answer and the CLI flags are only as stable as the installed Claude Code. Shapes marked `[verify]` in the code were checked against one CLI version. | Ongoing risk. Flags are gated by a `--help` probe. |
| The `post` hook publishes a `tool.external` SSE event, but nothing in the client consumes it, and it is not a transcript part. The transcript's native rows come only from the CLI's JSONL stream. | Open. |

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
        "timeout_secs": 900,
        "profile": "bebok"
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
| `args` | `[]` | Extra flags appended verbatim, last (so they can override defaults). `bypassPermissions` and `--dangerously-skip-permissions` are refused with a config error. |
| `mode` | `agent` | `agent` or `text`. |
| `allow_write` | **`false`** | Whether the CLI may modify files. |
| `timeout_secs` | `900` | Per-turn hard timeout, clamped to 30–7200. |
| `max_turns` | — | Forwarded as `--max-turns`. |
| `profile` | `bebok` | `bebok`, `user` or `isolated`. See [Managed profile](#managed-profile). |
| `profile_overrides` | — | A JSON object deep-merged over the generated `settings.json`. Must be an object. |
| `system_prompt_mode` | `append` | `append` or `replace`. `replace` needs a CLI that accepts `--system-prompt-file`; otherwise the turn fails with a config error rather than quietly appending. Ignored in `user` mode. |

Because `provider_auth` returns `ProviderAuth::None` for `kind: "cli"`, the
provider needs no API key, `is_configured()` is true without one, and the
settings UI hides the key and base-URL fields.

## Managed profile

Bebok does not hand the CLI its rules as prompt text. For `bebok` and
`isolated` it writes a profile — settings, MCP config and system prompt — and
the CLI enforces them through its own configuration.

| `extra.profile` | What the CLI loads | Login |
|---|---|---|
| `bebok` (default) | The generated `settings.json` on top of the user's settings. `--setting-sources user` keeps the repository's own `.claude/` out, since it would duplicate `AGENTS.md`. `--mcp-config` (and `--strict-mcp-config` when the binary has it), so the user's own MCP servers are ignored in this mode. | The user's account, unchanged. |
| `user` | The user's configuration as before. Only `profile_overrides` and the project layer are added, and no Bebok hooks are written. | The user's account. |
| `isolated` | `CLAUDE_CONFIG_DIR` points at the profile directory; none of `~/.claude` is read. | A **separate** login, run against that directory. |

**Where it lives.** `<data dir>/bebok/cli-profiles/<project-key>/<backend>/`,
where `<data dir>` is `dirs::data_dir()` and `<project-key>` is the first 8
bytes of SHA-256 over the project path, in hex. The directory is outside the
repository, so it never shows in `git status`, and it contains no secrets: the
turn token travels through the environment only.

**Files.**

| File | Written when |
|---|---|
| `settings.json` | Always in managed modes; in `user` mode only when overrides exist. |
| `mcp.json` | Managed modes, when the binary has `--mcp-config`. Currently `{"mcpServers": {}}`: no MCP servers are bridged from Bebok. |
| `system.md` | When there is system-prompt text and the binary can take it from a file. |
| `profile.lock.json` | With every write: schema, generator, `contentHash`, CLI version, the flags used, and the file names. |

Writes are atomic (temp file plus rename). A turn compares the freshly rendered
content hash with the lock and rewrites only on a difference, or when a file is
missing. The hash covers the CLI version, the flags, argv, env and every file's
bytes.

**Generated settings** (managed modes). `permissions` depends on
`allow_write`: `plan` mode with `Edit`, `Write` and `Bash` denied, or
`acceptEdits` with a list of shell and file tools allowed. `includeCoAuthoredBy`
is `false` (AGENTS.md §6). Auto-update, nonessential traffic and telemetry are
off through `env`. The `hooks` block is described under
[Permission hooks](#permission-hooks).

**Layering.** Settings are built in this order: Bebok's base, then
`profile_overrides`, then the project layer `<project>/.bebok/cli/claude/settings.json`.
The project file must be a JSON object. The system prompt is the turn's own
prompt followed by `<project>/.bebok/cli/claude/system.append.md`, when present.

**Validation.** The merged settings are refused with a config error when they
set `permissions.defaultMode` to `bypassPermissions`, or when an override drops
one of Bebok's own hook entries. Arrays replace on merge, so an override that
lists only its own `PreToolUse` entry would silently remove Bebok's. The check
catches that.

**Flags are probed.** A managed profile needs `--settings`. If the binary's
`--help` does not list it, the turn fails with a message that names the fix
(update Claude Code, or set `"profile": "user"`). Every other optional flag
(`--setting-sources`, `--mcp-config`, `--strict-mcp-config`,
`--append-system-prompt-file`, `--system-prompt-file`) is added only when the
probe reports it.

**Preview.** `GET /cli/profile/{provider}?directory=…` returns the files, args,
env, probed flags and directory the next turn would use. No tokens are in it.

## Permission hooks

In managed modes the profile registers three Claude Code command hooks, all
running the Bebok binary itself (`std::env::current_exe()`):

| Hook | Matcher | CLI timeout | Engine route |
|---|---|---|---|
| `PreToolUse` → `bebok-server cli-hook pre` | `*` | 600 s | `POST /cli-hook/{session}/pre` |
| `PostToolUse` → `bebok-server cli-hook post` | `*` | 60 s | `POST /cli-hook/{session}/post` |
| `Stop` → `bebok-server cli-hook stop` | — | 60 s | `POST /cli-hook/{session}/stop` |

`cli-hook` reads the CLI's hook JSON from stdin and POSTs it to
`$BEBOK_HOOK_URL/<event>` with `Authorization: Bearer $BEBOK_HOOK_TOKEN`. It
prints the engine's answer on stdout.

**`pre` is the permission path.** For each tool call, the route:

1. Maps the call to a rule string: `cli:Bash(<command>)`, `cli:Edit(<project-relative path>)`,
   `cli:WebFetch(<url>)` and so on. The `cli:` prefix is deliberate: a `bash(…)`
   rule written for Bebok's own tool does not authorise a command line the CLI
   assembled. Write a rule twice if you want it for both.
2. For `Bash`, applies the build & test policy first.
3. For `Edit`, `Write`, `MultiEdit` and `NotebookEdit`, snapshots the target so
   the Changes panel can diff and revert it.
4. Runs the permission engine, then the jev auto-decider, then the user popup.
   The hook waits for the answer. Read-only tools default to `allow`; mutating
   tools default to `ask`.
5. Answers `hookSpecificOutput.permissionDecision` (`allow` or `deny`) with a
   reason.

The engine gives up after 595 s (`BEBOK_CLI_HOOK_TIMEOUT_SECS` overrides this),
five seconds before the CLI's own 600 s, so the CLI gets a readable `deny`
rather than "hook timed out".

**Fail-closed.** Any failure on `pre` produces `deny`: the engine is unreachable,
`BEBOK_HOOK_URL` or `BEBOK_HOOK_TOKEN` is missing, the route refuses the token,
the payload has no tool name, or the popup times out. `post` and `stop` exit
non-zero on failure, so the failure does not look like success. Nothing is
silently allowed.

**Turn token.** `bebok_core::cli_turn_token` issues a credential per turn:

- issued when the CLI provider is built for a turn (`services/turn.rs`), and
  delegated children get their own token (`agent/delegation.rs`);
- scoped to one session: a token of one session is refused (`403`) by another
  session's route;
- revoked when the turn ends or is cancelled, with a 12-hour expiry as a
  backstop;
- passed only in the child's environment (`BEBOK_HOOK_URL` =
  `<engine>/cli-hook/<session>`, `BEBOK_HOOK_TOKEN`), never written to the profile.

The route answers `401` for a missing, unknown or expired token, `403` for a
token of another session or a non-loopback peer (checked whenever the transport
reports a peer address), and `404` for an unknown event.

**Turn end.** `stop` publishes `cli.turn.stopped` with `changedFiles`, the number
of paths the change tracker holds for the session. Only managed modes publish
it, because only they write the hooks.

## Resume

A CLI conversation continues across turns through `--resume <key>`, so a
follow-up sends only the new messages.

- **Record.** The session keeps `cli_session` in its metadata: the CLI key,
  provider, model, system-prompt hash, `msg_index` (how many messages the CLI
  already has) and the profile hash. Sessions written before this field existed
  load with it empty.
- **Gate.** `--resume` is emitted only when the binary's `--help` lists it.
- **Use.** The record is used only when provider, model and system-prompt hash
  all match, the profile hash matches, and the transcript before `msg_index`
  holds no tool parts. Anything else sends the full transcript.
- **Invalidation.** Truncating the chat clears the record. A fork starts without
  it, and so does a compaction, which creates a new session. Changing the model,
  system prompt or profile falls back to the full transcript.
- **Lost conversation.** If the CLI answers that it has no such conversation
  (`No conversation found` in stderr or the result), the turn is retried once
  with the full transcript, provided nothing has been streamed yet. A second
  loss is reported as an error.

## Grok Build backend

`extra.backend: "grok"` (provider `grok-cli`, models `grok-4.7`, `grok-4.7-build-fast`,
`grok-4.6`, `grok-4.5`; binary override `BEBOK_GROK_BIN`). Checked against `grok` 1.0.89.

`grok --output-format streaming-messages-json` emits the same JSONL dialect as
Claude Code's `stream-json`, so `GrokBackend::parse_line` delegates to the Claude parser.
Differences:

| | Claude | Grok |
|---|---|---|
| Prompt | stdin stream-json | `--prompt-file` (temp file, no argv limit) |
| System prompt | `--append-system-prompt[-file]` / profile | `--rules` (argv, 100 KB cap) |
| Read-only | `plan` + `--disallowedTools` | `plan` + `--disallowed-tools run_terminal_command,search_replace,write` |
| `allow_write` | `acceptEdits` + tool list | `acceptEdits` + `--allow Bash/Edit/Write` |
| Managed profile, permission hooks | yes | **no** (Grok uses its own `.grok/config.toml`) |
| Images, MCP bridge | images yes | no |

`--always-approve` / `bypassPermissions` is never emitted. In a headless run Grok cannot ask,
so a shell command with a file redirect (`echo x > f`) is cancelled even with `allow_write`;
plain commands and the `write` / `search_replace` tools work.

## Native tool rows

A CLI tool call appears in the transcript as a tool part marked as executed by
Claude Code.

- **Source.** The CLI's JSONL stream is the source. Each `assistant` `tool_use`
  is remembered by its id. The `user` `tool_result` with the matching
  `tool_use_id` becomes `CliEvent::ExternalTool`, then
  `StreamEvent::ExternalTool { source: ExternalSource::Cli }`. `agent/turn.rs`
  appends it as a tool part with `external: true`, deduplicated by id. It is
  never executed by Bebok.
- **Unpaired** tool results stay as activity text in the Thinking block.
- **Client.** The tool row shows an "executed by the CLI agent" badge
  (`data-testid="tool-external-badge"`) and a diff view for edits.
- **HTTP models.** When a session continues on an HTTP provider, `agent/request.rs`
  renders external parts as text, `[Claude Code ran <name>: <input> → <output>]`,
  appended to the assistant message. No orphaned `tool_use` reaches the API.
- **Hook path.** `post` publishes a `tool.external` SSE event with `toolName`,
  `input`, `response`, `changed` and `source: "cli-hook"`. It is not a transcript
  part, and no client consumes it yet.

## MCP bridge (planned)

Phase 04 would expose Bebok's own tools to the CLI over MCP, using a per-turn
token on `POST /mcp/session/{id}`. That route, and any `mcp__bebok__*` tool, does
not exist yet. The prompt helper `tool_ref` already has the branch for it, but
`CliProvider::transport()` always reports `bebok_tools: false`. Until phase 04
lands, the CLI works with its own tools only.

## Security

- `allow_write` defaults to **false**. The Claude backend then emits
  `--permission-mode plan` *and* `--disallowedTools Edit Write Bash` — belt and
  braces, because a permission mode alone is one config key away from wrong.
- With `allow_write: true` the backend emits `--permission-mode acceptEdits`
  **and** `--allowedTools`, naming exactly the shell and file tools that mode
  covers. The second flag is not redundant: `-p` has nobody to answer a
  permission prompt, so a tool that would still ask — `Bash` above all — is
  denied outright and reported back as a `permission_denials` entry. Without it
  a write-enabled CLI can edit files but never run a build or a test, and the
  turn reads as stuck. `--permission-mode plan` + `--disallowedTools` stay the
  read-only path, and nothing outside that tool list is loosened.
- In managed modes the same posture is also written into `settings.json`, and
  the permission hooks gate every call on top of it.
- `--permission-mode bypassPermissions` and `--dangerously-skip-permissions` are
  **never** emitted, even with `allow_write: true`, and `extra.args` or
  `profile_overrides` cannot introduce them.
- The CLI always runs with `current_dir` set to the instance root. No `--add-dir`
  is added without explicit configuration.
- `spec.api_key` is never passed to the child process; the CLI has its own
  authentication.
- A hard timeout plus a tree-wide kill on drop, so an aborted turn does not
  leave a zombie CLI holding the project open.

## Adding a new CLI

1. Create `engine/crates/bebok-cli-agents/src/backends/<name>.rs`.
2. Implement `CliBackend`: `id`, `label`, `capabilities`, `default_models`,
   `binary_names`, `env_override`, `build_command`, `parse_line`. Optional
   hooks with defaults: `profile_writer` (managed profile), `resume_flag_available`
   and `catalog_model`.
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
- Hook commands are quoted (`"<path>" cli-hook pre`), because the binary path
  may contain spaces and the CLI runs the command through a shell.

## Endpoints

- `GET /cli/backends` — registered backends, whether the binary was found, its
  version, capabilities and default models. Cached ~60 s.
- `POST /cli/backends/{id}/probe` — runs `--version` on demand for the settings
  "Test" button and returns stdout/stderr.
- `GET /cli/profile/{provider}?directory=` — the managed profile the next turn
  would use, without tokens (see [Managed profile](#managed-profile)).
- `POST /cli-hook/{session}/{pre|post|stop}` — internal. Called by
  `bebok-server cli-hook`; authenticated by the per-turn token, not the engine's
  capability token.
