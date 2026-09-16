# The Orchestrator

How the main-thread orchestrator decomposes work, spawns and supervises
sub-agents (`task` / `fleet`), and how it is planned to keep children on-task
(detect and abort children that loop or wander). Grounded in the current code
(`engine/crates/bebok-core/src/agent/`); where a part is *planned* rather than
shipped it is marked **planned**.

Companion read: `AGENTS.md` §4 "Agent loop" (the one-paragraph summary) and
§7 "Gotchas".

---

## 1. Roles

- **Main thread / orchestrator** — the session the user talks to. It owns the
  conversation, decomposes work, spawns children, supervises them, integrates
  results, and gives the user one consolidated answer. It is the only agent
  that holds the delegation/supervision tools.
- **Sub-agents (children)** — sessions spawned by `task` (one piece of work)
  or `fleet` (fan-out of one prompt to a roster of agents). They are *workers*:
  they do the hands-on work and report back. A running child is never the
  orchestrator.

The split is enforced in `request.rs`: a session with a parent never gets the
delegation/supervision tools — `DELEGATION_TOOLS = ["task", "fleet",
"task_status", "task_wait", "task_cancel"]` are withheld from every child.
So a child cannot re-delegate (no unbounded recursion); the depth guard is the
backstop. The `fleet` tool is additionally orchestrator-only (`request.rs`
withholds it from every agent except `FLEET_AGENT`).

Presets (`preset.rs`): `code`, `ask`, `plan`, `debug`, `orchestrator` (+ file
agents with hot reload). The orchestrator preset carries the "decompose and
supervise" stance; the prompt carries the `Fleet:` roster rendered from the
resolved `fleet` config.

---

## 2. Spawning children

One entry tool: **`fleet`** (`fleet_tool.rs`) — fan out one prompt to N roster
members. Orchestrator-only. Each call takes `names` (member labels copied from
the `Fleet:` roster in the prompt) plus the shared prompt, and each member
follows the same child lifecycle through the shared `delegation.rs` machinery
(`prepare_child` → `run_child`). Names outside the roster are refused — the
roster is the only source of sub-agents.

Lifecycle of one child (`delegation.rs::run_child`):

1. **Register** on the parent (`SessionState::register_child_task_full_with_prompt`):
   allocates a unique name, stores the child's `CancellationToken`, and the
   `ChildTask` metadata (`taskID`, `description`, `childSessionID`, `name`,
   `agent`, `model`, `startedAt`, `prompt_hash`, `status`, `background`).
2. **Announce** `task.started` on the *parent* session (status `queued` or
   `running`).
3. **Slot gate** — children run under a per-session `SlotGate` =
   `delegation.max_concurrent` (default 3, clamped to
   `1..=MAX_DELEGATION_MAX_CONCURRENT`). A child whose slot is taken stays
   `queued`; when a slot frees it flips to `running`. This cap is the only
   remaining `delegation` setting: the `mode` / `model_policy`
   (delegation-policy) configuration is gone.
4. **Progress** — `spawn_progress_reporter` watches the child's message events
   and republishes them as `task.progress` on the parent, throttled to at most
   one event per `PROGRESS_MIN_GAP`, each carrying a `TaskProgress` snapshot
   (`summarize_progress`). Milestones also write a token-free `Part::Status`
   row into the parent transcript (`status_rows.rs`).
5. **Run the turn** — the child runs a normal turn loop on its own model (see
   §5).
6. **End** — persist the outcome, push a `TaskResult` onto the parent
   (`completed` | `error` | `aborted`), emit `task.ended`, and unregister the
   child.

`abort` is wired end-to-end already: cancelling a child's token makes its turn
loop break, persists `[Turn aborted …]`, closes open tool calls, and the
`task.ended` lands with status `aborted` + a `task.aborted` event carrying a
`reason`. Cancelling a child also cancels the parent turn (`abort_children_and_parent`)
so the orchestrator re-plans with the abort in context. **No engine work is
needed to make abort work** — only to give the orchestrator the signals and the
instruction to act.

---

## 3. Supervision tools (held only by the main thread)

`supervision_tools.rs` gives the orchestrator three tools:

- **`task_status`** — a cheap, token-free listing of every child (live and
  finished). Live rows show `name`, `agent`, `model`, elapsed ms, `status`
  (`queued`/`running`), the last tool, and a one-line `summary` (from
  `summarize_progress`). This is the "check the children" call.
- **`task_wait`** — block (with a bounded `timeout_seconds`) until the next
  child result is pushed; on wake it returns the newly-collected `TaskResult`s
  (final text + token counts). The wake is a `Notified` future armed *before*
  the re-check, so a result landing in between is not missed.
- **`task_cancel`** — cancel a specific child by id/name with a `reason`.
  Cancels the child token, emits `task.aborted { reason }` on the parent, and
  (via the cascade) the parent turn.

---

## 4. What a child's `TaskProgress` carries

`summarize_progress(&[Message]) -> TaskProgress` (`delegation.rs`) walks a
child's transcript and is recomputed on every `task.progress` tick and on each
`task_status` call. It already exposes `last_tool`, `tool_calls`, `steps`, and
a `summary`. It is serialized into `task.progress` events, the Agents panel,
and `task_status` — so any new field added here reaches the model *and* the UI
for free.

**Planned** extension (the supervision signals):

- `looping: Option<LoopHit>` — `LoopHit { tool, repeats, sample }`.
- `wandering: Option<WanderHit>` — `WanderHit { distinct_reads, total_calls, read_ratio }`.
- `flags: Vec<&'static str>` (`["looping"]` / `["wandering"]`) and a derived
  `verdict: "ok" | "warn"`.
- All new fields are `skip_serializing_if` when clean, so a healthy child's
  payload is byte-identical to today's.

Detection (pure, unit-testable, no runtime):

- **Looping** — over the last `LOOP_WINDOW = 8` *closed* tool calls, group by a
  normalized call key (path tools → `path`; `read_file` → `path`+`offset`;
  `bash` → command; else canonical input JSON). One key ≥ `LOOP_MIN_REPEAT = 3`
  → `looping` (the child is repeating the same action).
- **Wandering** — count distinct paths read by the read/inspection tool set
  (`read_file, head, tail, wc, list_dir, tree, stat, du, sort, uniq, diff,
  find, realpath, basename, dirname, glob, grep`) and the fraction of all calls
  that are reads. Flag when distinct reads ≥ `WANDER_MIN_DISTINCT_READS = 12`
  **and** read ratio ≥ `WANDER_READ_RATIO = 0.6` **and** the last
  `WANDER_NO_ADVANCE_WINDOW = 5` calls contain no `write_file` / `edit_file` /
  `append_file` / `sed` / `bash` (i.e. it read many files and advanced nothing).

False-positive guard lives in the *policy*, not the detector: an `ask`/`plan`
child reading a lot is normal; a `code`/`debug` child that read many files but
wrote/built nothing is the one to suspect.

---

## 5. Model selection

No model policy: each fleet member runs the model from its roster entry (the
resolved `fleet` config). A per-call `model` override wins. The effective model
is recorded on the `ChildTask` so `task_status` and
`GET /session/{id}/agents` can show it while the child is running.

---

## 6. Prompt the orchestrator gets

The prompt carries the `Fleet:` roster rendered from the resolved `fleet`
config (labels the orchestrator copies into `fleet` `names`), plus the
**Supervision cadence** guidance:

- **Cadence.** Don't sit in one long `task_wait`. Loop it: bounded
  `task_wait` → `task_status` → act → repeat until every child is collected.
  This is the "check the children from time to time" behaviour.
- **Read the flags.** Each `task_status` child carries `looping` (repeating the
  same call) and `wandering` (reading many unrelated files without advancing).
  Judge by agent type (see §4).
- **Call to order.** When a child is flagged: `task_cancel` it (reason
  `looping` / `wandering`) and *immediately* re-spawn it via `fleet` with a
  corrective, tighter brief that names the failure ("you kept re-reading `x` /
  read 15 unrelated files — read only these: …, then implement"). Don't just
  discard it — re-delegate it.
- **Escalation.** If the re-spawned child is flagged again, finish that part
  yourself with the normal tools.

`preset.rs::ORCHESTRATOR_PROMPT` carries the one-line stance
("keep children on-task: periodically check, cancel a looping/wandering child,
re-delegate with a sharper brief"); the policy section carries the operational
detail.

---

## 7. Events (SSE, parent session)

Envelope `{type, directory, sessionID, properties}` at `GET /event`. The
delegation-relevant types:

- `task.started` — a child registered (`taskID`, `name`, `agent`, `model`,
  `status`).
- `task.progress` — throttled progress; `properties` carries the
  `TaskProgress` snapshot (and, **planned**, the `looping`/`wandering` flags).
- `task.ended` — a child finished (`status: completed | error | aborted`,
  `error?`).
- `task.aborted` — a child was cancelled, with `reason`.

---

## 8. Config

`delegation` section (global, merged per key over project) keeps a single key:

- `max_concurrent`: positive int, clamped to `1..=MAX_DELEGATION_MAX_CONCURRENT`
  (default 3) — the cap on concurrently running children.

The `mode` and `model_policy` keys (the delegation policy) are gone, and so is
`GET /delegation/models`. The sub-agent roster lives in the `fleet` section
(replaces wholesale, not merged).

**Planned** (only if Phase 1 proves under-reactive): `delegation.supervision.enabled`
(default `true`) to disable the flag surfacing + policy bullet, and an
engine-side **auto-watchdog** in `run_child` that cancels a child on a *hard*
loop (same call ≥ ~5) independent of the model, emitting
`task.ended {status:"aborted", reason:"auto:looping"}`. Ship behind the flag and
restricted to hard loops (not "wandering").

---

## 9. Design rationale

- **Engine computes deterministic, token-free signals; the model decides.**
  The detector never cancels on its own in Phase 1 — it only *tells* the
  orchestrator what to look at, and the orchestrator (the requested actor)
  decides whether to cancel + re-delegate. This keeps false positives a
  judgment call rather than a threshold trip.
- **Abort already works end-to-end**; the gap was purely *detect* + *decide*.
- **Signals ride `TaskProgress`** so model and UI both see them without new
  transport.
- The conservative defaults (§4) and the agent-type-aware policy mean a healthy
  child that legitimately reads a lot (research, early exploration) is not
  killed.
