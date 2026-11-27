---
name: tool-authoring
description: Write a new built-in agent tool into the Bebok engine (Tool trait, registration, safety, presets, tests). Use when the user asks to add a new tool, expose a new capability to agents, or port a shell command to a native tool.
---

# Tool authoring — write a new tool into the system

A Bebok tool is a Rust struct implementing the `Tool` trait. Built-ins live in
`engine/crates/bebok-tools`, are registered in one list, classified once for
safety, and automatically appear in `GET /tools/list` and Settings → Tools
(enable/disable + custom description via `tool_config` — no per-tool UI work needed).

## 1. Implement the tool struct

Create `engine/crates/bebok-tools/src/<tool_name>.rs` (snake_case file, e.g. `jq_filter.rs`
for tool `jq_filter`). Copy the shape of `read_file.rs` (simple read) or `bash.rs`
(complex / background):

```rust
use async_trait::async_trait;
use serde_json::{Value, json};
use crate::tool::{Tool, ToolCtx, ToolOutput};
use crate::pathguard::resolve_in_root; // mandatory for any file path arg

pub struct MyTool;

#[async_trait]
impl Tool for MyTool {
    fn name(&self) -> &str { "my_tool" }          // snake_case, globally unique
    fn description(&self) -> &str { "..." }       // one sentence, verb first
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object",
            "properties": { "path": { "type": "string", "description": "..." } },
            "required": ["path"] })
    }
    fn is_read_only(&self) -> bool { true }       // false if it writes/executes/networks
    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        // missing-arg → ToolOutput::new("error: missing required parameter 'x'", "my_tool")
        // file args → resolve_in_root(&ctx.root, path)? (never touch paths directly)
        // success  → ToolOutput::new(text, "my_tool <summary>")
    }
}
```

Rules:

- **Trait** (`tool.rs`): `name`, `description`, `parameters_schema`, `execute` are
  required. Defaults: `is_read_only() == false`, hints `None`. Override
  `is_read_only_for(&args)` only when read-only-ness depends on args
  (`base64` without `out`, `fetch` GET/HEAD, `sed` without in-place write).
- **Path safety**: every file/dir argument goes through
  `resolve_in_root(&ctx.root, path)` — it rejects absolute paths, drive/UNC
  paths, `..`, and escaping symlinks. Never `Path::new(arg)` directly. Error
  text must start with `error: ` and never leak contents of rejected paths.
- **Output**: `ToolOutput::new(text, title)`; title is `"<name> <one-line summary>"`.
  Keep text plain (the model reads it); attach `with_structured(json)` only for
  UI-consumed data (diffs) and `with_image()` only for screenshots.
- **No `println!`** anywhere — engine stdout is the `BEBOK_READY` protocol line.
- **Cancellation**: long-running work should watch `ctx.cancel` (a
  `CancellationToken`); see `bash.rs`.
- **Tests**: add a `#[cfg(test)] mod tests` in the same file — pure-logic unit
  tests plus one `rejects_paths_outside_root`-style test for any path arg
  (see `read_file.rs` tests). Async execute tests use `#[tokio::test]` with
  `tool_ctx(temp_root, session, CancellationToken::new())`.

## 2. Register it (3 touch points, all required)

1. **`engine/crates/bebok-tools/src/lib.rs`**
   - Add `pub mod <tool_name>;` (alphabetical) and one line in `builtin_tools()`:
     `Arc::new(<module>::<Struct>)` in the right group (read / write / escape hatch).
2. **`engine/crates/bebok-core/src/tool_safety.rs` → `BUILTIN_DEFAULTS`**
   - Add `("<tool_name>", SafetyCategory::Safe|Caution|Dangerous)`.
   - `safe` = read-only inspection; `caution` = network/delegate/interactive;
     `dangerous` = writes/deletes/executes. Unknown stays gray — so classify it.
   - The `every_builtin_tool_is_categorized` test fails the build if you skip this.
3. **Presets** (`engine/crates/bebok-core/src/agent/preset.rs`) — only if the tool
   must be hidden from `ask`/`plan`/`hub` (their whitelist is read-only tools).
   Read-only tools: add the name to the `ask`/`plan`/`hub` lists. Mutating tools:
   do nothing (code/debug/orchestrator get everything).

No other registration is needed: `InstanceStore` builds the registry from
`builtin_tools()`, `tool_config` enable/disable + description overrides apply
generically, and `GET /tools/list` / Settings → Tools pick the tool up
automatically (name, effective description, read-only flag, safety, source).

## 3. What NOT to touch

- `tool_config.rs` / `registry.rs` disable + description-override machinery — generic.
- `routes/tools.rs`, `routes/config.rs`, client `tools-tab` — generic.
- `permission/` rules — users write those themselves; default verdicts
  (read-only → Allow, mutating → Ask) follow from `is_read_only`.
- MCP bridging (`mcp__*`) — only for external servers, never for a native tool.

## 4. Verify

```bash
cd engine && cargo fmt --all
cd engine && cargo clippy --workspace --all-targets -- -D warnings
cd engine && cargo test -p bebok-tools <tool_name>
npm run check:windows   # from repo root, before pushing (catches #[cfg(windows)] issues)
```

Then confirm the tool surfaces: `GET /tools/list?directory=<root>` contains the
new name with the right `is_read_only` + `safety`, and toggling it in
Settings → Tools hides it from the agent schema (disabled tools are filtered in
`agent/request.rs` via `registry.list()` and never reach the model).

## Checklist before declaring done

- [ ] file `engine/crates/bebok-tools/src/<tool>.rs` with `Tool` impl + tests
- [ ] `lib.rs`: `pub mod` + `builtin_tools()` entry
- [ ] `tool_safety.rs`: `BUILTIN_DEFAULTS` entry (matching `is_read_only` semantics)
- [ ] preset whitelist updated if read-only and ask/plan should see it
- [ ] `cargo fmt`, `clippy -D warnings`, `cargo test -p bebok-tools` green
- [ ] `CHANGELOG.md` bullet under `## Unreleased`
