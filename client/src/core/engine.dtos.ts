/**
 * Typed DTOs mirroring the engine JSON API (bebok-core session/part model).
 *
 * Wire naming follows the Rust `serde` output exactly (snake_case fields,
 * tagged enums), so these types can be used 1:1 against the REST + SSE API.
 */

export type Role = 'user' | 'assistant';

export interface TextPart {
  type: 'text';
  text: string;
}

export interface ThinkingPart {
  type: 'thinking';
  text: string;
}

export interface ToolStatePending {
  state: 'pending';
  input: unknown;
}

export interface ToolStateRunning {
  state: 'running';
  input: unknown;
  started_at: number;
}

export interface TaskLink {
  taskID: string;
  name: string;
  agent: string;
  childSessionID: string;
  /** F9-9: effective model of the child turn (`task`/`fleet` structured output). */
  model?: string;
}

export interface ToolStateCompleted {
  state: 'completed';
  input: unknown;
  output: string;
  title: string;
  structured?: TaskLink;
}

export interface ToolStateError {
  state: 'error';
  input: unknown;
  error: string;
}

export type ToolState = ToolStatePending | ToolStateRunning | ToolStateCompleted | ToolStateError;

/**
 * Safety tier the engine's permission gate applied to a call (F7-1).
 * `undefined` for a call whose permission has not been resolved yet, or for
 * a session persisted before this field existed.
 */
export type PermissionLevel = 'allow' | 'ask' | 'deny';

/**
 * Explicit safety category of a tool (F7-7): `safe` (green) / `caution`
 * (yellow) / `dangerous` (orange) / `uncategorized` (gray). Resolved by the
 * engine from its built-in default table plus the `tool_safety` config map;
 * purely informational (never changes an allow/ask/deny verdict).
 */
export type SafetyCategory = 'safe' | 'caution' | 'dangerous' | 'uncategorized';

export const SAFETY_CATEGORIES: readonly SafetyCategory[] = [
  'safe',
  'caution',
  'dangerous',
  'uncategorized',
];

export function isSafetyCategory(value: unknown): value is SafetyCategory {
  return typeof value === 'string' && (SAFETY_CATEGORIES as readonly string[]).includes(value);
}

export interface ToolPart {
  type: 'tool';
  id: string;
  name: string;
  state: ToolState;
  /** Verdict the permission gate applied to this call (F7-1, kept for
   *  backward compatibility; no longer drives the dot colour). */
  permission?: PermissionLevel;
  /** Whether the call is mutating/dangerous, independent of the verdict
   *  actually applied (F7-1, kept for backward compatibility). */
  mutating?: boolean;
  /** Explicit safety category stamped by the engine (F7-7). Absent on
   *  parts persisted before this field existed - derive it from the tool
   *  name via `GET /tools/safety` then (`ToolSafetyStore.categoryOf`). */
  safety?: SafetyCategory;
}

export interface UsagePart {
  type: 'usage';
  input_tokens: number;
  output_tokens: number;
  cost?: number | null;
  cache_read_input_tokens?: number | null;
  cache_creation_input_tokens?: number | null;
}

/**
 * F9-7: one progress row appended to the parent's latest assistant message
 * while a delegated child runs (`task.started` / `task.progress` /
 * `task.ended`). Display only - never sent to the LLM.
 */
export interface StatusPart {
  type: 'status';
  /** `task.started` | `task.progress` | `task.ended` (open set). */
  kind: string;
  text: string;
  /** Unix ms. */
  at: number;
  taskID?: string;
  name?: string;
  childSessionID?: string;
}

export type Part = TextPart | ThinkingPart | ToolPart | UsagePart | ImagePart | StatusPart;

/** An image attached to a message (engine `Part` union member). */
export interface ImagePart {
  type: 'image';
  media_type: string;
  /** Raw base64 payload (no `data:` prefix). */
  data: string;
  name?: string;
  bytes?: number;
}

export interface MessageMeta {
  created_at?: number;
  /** Agent preset that produced this message (assistant messages only). */
  agent?: string | null;
  /** Model that produced this message (assistant messages only). */
  model?: string | null;
}

export interface Message {
  id: string;
  role: Role;
  parts: Part[];
  meta?: MessageMeta;
}

export interface UsageTotals {
  input_tokens: number;
  output_tokens: number;
  cost?: number | null;
  cache_read_input_tokens?: number | null;
  cache_creation_input_tokens?: number | null;
}

export interface SessionMeta {
  id: string;
  /** Current engine turn state, also available after a page reload. */
  running?: boolean;
  /** Normalized working directory the session is bound to. */
  directory: string;
  title?: string | null;
  alias?: string | null;
  agent: string;
  model?: string | null;
  parent?: [string, number] | null;
  created_at: number;
  updated_at: number;
  usage: UsageTotals;
  /**
   * F6-3: tokens the provider read on the *last* LLM call (input + cache
   * buckets) - the live context size, not a running total. Absent until the
   * first turn completes.
   */
  context_used?: number | null;
  /** Model that produced `context_used` (may differ from `model`). */
  context_model?: string | null;
  /** Context window of that model, resolved live from the engine's catalog. */
  context_window?: number | null;
  share?: unknown;
  /**
   * WP-GIT: branch name when the session runs in a Bebok git worktree
   * (`<root>/.bebok/worktrees/<branch>`), derived by the engine from
   * `directory` at response time. `null`/absent for ordinary sessions - the
   * client never splits paths to work this out.
   */
  worktree_branch?: string | null;
  /**
   * F9-9: engine-attached effective model (`session.model`, else the agent
   * preset model, else `models.<agent>`, else the config default), e.g.
   * `openai/gpt-5.6-luna`. Absent on older engines.
   */
  effective_model?: string;
  /** F9-9: the part of `effective_model` before the first `/` (or the config provider). */
  effective_provider?: string;
}

/**
 * F9-12: is this session a sub-agent child (spawned by the `task` / `fleet`
 * tools)? The engine sets `parent` for three kinds of derived session - a
 * fork, a compaction and a delegated child - but only the child gets an
 * `alias` (the orchestrator-assigned name, allocated at spawn time). A fork
 * or compaction copies neither, so "has a parent AND an alias" is the
 * client-side definition; no extra engine field is needed.
 */
export function isSubAgentSession(session: Pick<SessionMeta, 'parent' | 'alias'>): boolean {
  return !!session.parent && !!session.alias?.trim();
}

/** Parent session id of a derived session, or null. */
export function parentSessionId(session: Pick<SessionMeta, 'parent'>): string | null {
  return session.parent?.[0] ?? null;
}

export interface PendingPermissionSnapshot extends PermissionAsked {
  sessionID: string;
  directory: string;
}

export interface CreateSessionResponse {
  sessionID: string;
}

export interface MessageListResponse {
  messages: Message[];
}

export interface SessionListResponse {
  sessions: SessionMeta[];
}

export interface PromptResponse {
  sessionID: string;
  messageIndex: number;
  status: string;
}

/** One image attached to an outgoing prompt (raw base64, no `data:` prefix; max 5 images, 5 MB each, png/jpeg/webp/gif). */
export interface PromptImage {
  media_type: string;
  data: string;
  name?: string;
}

/** `POST /session/{id}/prompt` body: text plus optional image attachments. */
export interface PromptBody {
  message: string;
  agent?: string;
  model?: string;
  images?: PromptImage[];
}

export interface AbortResponse {
  sessionID: string;
  aborted: boolean;
}

export interface AbortTaskResponse {
  sessionID: string;
  taskID: string;
  aborted: boolean;
}

export interface PermissionResponse {
  sessionID: string;
  requestID: string;
  decision: 'allow' | 'deny';
  resolved: boolean;
}

/**
 * One event from `GET /event` (the global SSE stream). Clients route by
 * `directory`/`sessionID`; `properties` carries event-specific payloads.
 */
export interface EngineEvent {
  type: string;
  directory: string;
  sessionID: string;
  properties?: Record<string, unknown>;
}

/** Shapes of `permission.asked` / `permission.resolved` properties. */
export interface PermissionAsked {
  requestID: string;
  messageIndex: number;
  toolName: string;
  agent: string;
  input: unknown;
  pattern: string;
  /** F9-5: alias of the asking session (sub-agent name); absent for a main session. */
  sessionAlias?: string;
  /** F9-5: set when the asking session is a child (sub-agent). */
  parentSessionID?: string;
  /** F9-5: the rule "Always allow" will write, e.g. `write_file(*)`. */
  suggestedRule?: string;
}

export interface PermissionResolved {
  requestID: string;
  messageIndex: number;
  toolName: string;
  agent: string;
  pattern: string;
  decision: string;
  always: boolean;
  allowed: boolean;
  /** F9-5: the rule written when `always: true` was applied. */
  rule?: string;
  /** F9-5: `"project"` when `always: true` was applied. */
  scope?: string;
}

/** Active sub-task spawned by the orchestrator (emitted via `task.started`). */
export interface ActiveTask {
  taskID: string;
  description: string;
  childSessionID: string;
  name?: string;
  agent?: string;
  /** F6-12: effective model of the child turn. */
  model?: string;
  /** F6-12: unix ms when the child turn was registered. */
  startedAt?: number;
}

/** F6-12: lifecycle of one delegated child as reported by `GET /session/{id}/agents`. */
export type AgentStatus = 'queued' | 'running' | 'done' | 'failed' | 'aborted' | 'unknown';

/**
 * WP-DELEGATION: one-line live view of what a child is doing (payload of
 * `task.progress` events and of `AgentEntry.progress`).
 */
export interface TaskProgress {
  /** Name of the most recent tool call in the child's transcript. */
  lastTool?: string;
  /** `pending` | `running` | `completed` | `error` of that call. */
  lastToolState?: string;
  /** Last non-empty line of the child's latest assistant text. */
  summary: string;
  toolCalls: number;
  steps: number;
}

/** WP-DELEGATION: properties of a `task.progress` SSE event (parent session). */
export interface TaskProgressEvent {
  taskID: string;
  childSessionID: string;
  name: string;
  agent: string;
  status: 'queued' | 'running';
  progress: TaskProgress;
  tokens: { input: number; output: number };
  at: number;
}

/** One row of `GET /session/{id}/agents` (live task map merged with finished children). */
export interface AgentEntry {
  /** Present while running only (the id is not persisted once the task ends). */
  taskID?: string;
  childSessionID: string;
  name: string;
  agent: string;
  model?: string;
  status: AgentStatus;
  description: string;
  startedAt: number;
  endedAt?: number;
  error?: string;
  usage: UsageTotals;
  /** WP-DELEGATION: live progress line (running children only). */
  progress?: TaskProgress;
  /** WP-DELEGATION: spawned with `background: true`. */
  background?: boolean;
}

export interface SessionAgentsResponse {
  agents: AgentEntry[];
}

export type ToolStateKind = 'pending' | 'running' | 'completed' | 'error';

export const TOOL_STATE_KINDS: readonly ToolStateKind[] = [
  'pending',
  'running',
  'completed',
  'error',
];

export function toolStateKind(state: ToolState): ToolStateKind {
  return state.state;
}

/**
 * Resolver used for tool parts that carry no stamped `safety` field
 * (historical sessions): maps a tool name to its current category, or
 * `null` when the name is unknown to the client.
 */
export type SafetyResolver = (toolName: string) => SafetyCategory | null;

/**
 * Safety-dot colour for a tool call (F7-7): the explicit category stamped
 * by the engine when the call was gated; for a part persisted before the
 * field existed, the category the engine reports for that tool name *now*
 * (via `resolve`, backed by `GET /tools/safety`); `uncategorized` (gray)
 * when neither is known. The old F7-1 `permission`/`mutating` fields are
 * deliberately ignored - the runtime verdict is not a safety category.
 */
export function safetyCategory(part: ToolPart, resolve?: SafetyResolver): SafetyCategory {
  if (isSafetyCategory(part.safety)) {
    return part.safety;
  }
  const derived = resolve?.(part.name) ?? null;
  return derived ?? 'uncategorized';
}

/** One row of `GET /tools/safety` (F7-7). */
export interface ToolSafetyEntry {
  name: string;
  /** `built-in`, `mcp:<server>` or `plugin`. */
  source: string;
  category: SafetyCategory;
  default_category: SafetyCategory;
  is_override: boolean;
  /** The `tool_safety` key that produced the override (name or glob). */
  override_pattern?: string;
}

/** `GET /tools/safety` / `PUT /tools/safety` payload (F7-7). */
export interface ToolSafetyResponse {
  tools: ToolSafetyEntry[];
  /** Number of tools currently `uncategorized`. */
  uncategorized: number;
  categories: SafetyCategory[];
  /** Raw `tool_safety` maps of each config layer. */
  overrides: { global: Record<string, string>; project: Record<string, string> };
}

// ---------------------------------------------------------------------------
// M4: agents, skills, MCP, config
// ---------------------------------------------------------------------------

export interface AgentInfo {
  name: string;
  description?: string | null;
  /**
   * Effective model for this agent: the preset's own model if it pins one,
   * else `config.models.<name>`, else the global `config.model`.
   */
  model?: string | null;
  builtin: boolean;
  source?: string | null;
}

export interface AgentListResponse {
  agents: AgentInfo[];
}

export interface McpStatus {
  name: string;
  enabled: boolean;
  connected: boolean;
  tool_count: number;
  error?: string | null;
}

export interface McpListResponse {
  servers: McpStatus[];
}

export interface ResolvedSkill {
  name: string;
  description?: string | null;
  content: string;
  source: 'global' | 'project';
  path: string;
  enabled: boolean;
}

/** One member of the parallel-agents fleet (`fleet.members` in config). */
export interface FleetMember {
  name: string;
  agent: string;
  model: string;
}

/**
 * `POST /fleet/generate?directory=` - fleet members proposed by the engine from
 * the configured providers. `fallback` is set when the LLM call was not usable
 * and the engine produced a deterministic list instead; `warning` carries the
 * provider error (or `null` when the generation went through the LLM).
 */
export interface FleetGenResponse {
  members: FleetMember[];
  generationModel: string;
  fallback: boolean;
  warning: string | null;
}

/** WP-DELEGATION (F8-2): `delegation.mode`. */
export type DelegationMode = 'off' | 'auto' | 'always';

/** WP-DELEGATION (F8-2): `delegation` section of the config. */
export interface DelegationConfig {
  mode: DelegationMode;
  max_concurrent: number;
  /** Legacy optional model override for every sub-agent (`provider/model`); == explicit policy. */
  model?: string | null;
  /**
   * F9-10: `"inherit"` | `"cheaper"` (default) | `"<provider/model>"` (explicit).
   * Absent on older engines (treat as `cheaper` unless `model` is set).
   */
  model_policy?: string;
}

/** F9-10: one row of `GET /delegation/models` `mappings`. */
export interface DelegationModelMapping {
  provider: string;
  model: string;
  /** Cheaper sibling from the catalog; `null` = none (falls back to inherit). */
  cheaper: string | null;
}

/** F9-10: `GET /delegation/models?directory=`. */
export interface DelegationModelsResponse {
  policy: string;
  parent_model: string;
  /** The model a sub-agent would get right now. */
  resolved: string;
  mappings: DelegationModelMapping[];
}

/** Parallel-agents fleet config (`fleet` section of the config). */
export interface FleetConfig {
  enabled: boolean;
  members: FleetMember[];
}

export interface ResolvedConfig {
  model: string;
  provider: string;
  max_tokens: number;
  api_key?: string | null;
  /** Per-agent-type model overrides (`{ code, ask, plan, debug, orchestrator }`). */
  models?: Record<string, string>;
  /** Raw provider overrides from config (resolved view is `ConfigResponse.providers`). */
  providers?: ProviderSpec[];
  context_budget?: number;
  tool_output_cap?: number;
  /** YOLO mode: auto-allow every tool call without asking. */
  yolo?: boolean;
  /** Reasoning/thinking effort: `off` | `low` | `medium` | `high` | `max`. */
  thinking?: string;
  /** Parallel-agents fleet (absent = disabled with no members). */
  fleet?: FleetConfig;
  /** WP-DELEGATION: sub-agent delegation policy (absent on older engines = defaults). */
  delegation?: DelegationConfig;
  permission: unknown;
  mcp: unknown;
  skills: unknown;
  terminal: unknown;
  runtimes: unknown;
  /** Client-only UI overrides: custom CSS (plain text, never executed by the engine). */
  ui?: UiConfig;
  /** WP-BROWSER2 (F7-6): how the agent's browser is shown. */
  browser?: BrowserConfig;
  /** F7-7: `{ "<tool name or glob>": SafetyCategory }` (merged global + project). */
  tool_safety?: Record<string, string>;
  /** WP-AUTOVERIFY (F8-1): autonomous frontend verification policy. */
  verify?: VerifyConfig;
}

/** `verify` config section (WP-AUTOVERIFY / F8-1). */
export type FrontendVerify = 'auto' | 'ask' | 'off';

export const FRONTEND_VERIFY_MODES: readonly FrontendVerify[] = ['auto', 'ask', 'off'];

export function isFrontendVerify(value: unknown): value is FrontendVerify {
  return (
    typeof value === 'string' && (FRONTEND_VERIFY_MODES as readonly string[]).includes(value)
  );
}

export interface VerifyConfig {
  /** `auto` (default): verify without asking; `ask`: ask once; `off`: no policy. */
  frontend?: FrontendVerify;
}

/** `browser` config section (WP-BROWSER2 / F7-6). */
export type BrowserDisplay = 'headed' | 'viewer' | 'drawer';

export const BROWSER_DISPLAYS: readonly BrowserDisplay[] = ['headed', 'viewer', 'drawer'];

export function isBrowserDisplay(value: unknown): value is BrowserDisplay {
  return typeof value === 'string' && (BROWSER_DISPLAYS as readonly string[]).includes(value);
}

export interface BrowserConfig {
  display?: BrowserDisplay;
  /** Top-left corner for the headed window (screen px), `[x, y]`. */
  windowPosition?: [number, number];
}

/** `GET /session/{id}/browser` (WP-BROWSER2 / F7-6). */
export interface BrowserState {
  sessionID: string;
  directory: string;
  display: BrowserDisplay;
  /** A browser process is live for the session. */
  open: boolean;
  /** Visible OS window (headed mode); `null` when no browser is open. */
  headed: boolean | null;
  url: string;
  title: string;
  running: boolean;
  streaming: boolean;
}

/** One frame: `GET /session/{id}/browser/frame` and `browser.frame` SSE properties. */
export interface BrowserFrame {
  sessionID: string;
  directory: string;
  url: string;
  title: string;
  media_type: string;
  /** Raw base64 (no `data:` prefix). */
  data: string;
  width: number;
  height: number;
  seq: number;
  headed: boolean;
}

/** `POST /session/{id}/browser/{action}` actions. */
export type BrowserAction =
  'navigate' | 'back' | 'forward' | 'reload' | 'click' | 'type' | 'screenshot' | 'close';

export interface BrowserActionResult {
  sessionID: string;
  action?: string;
  ok?: boolean;
  text?: string;
  url?: string;
  title?: string;
  structured?: unknown;
  image?: { media_type: string; data: string };
  closed?: boolean;
}

/** Client-only UI overrides (`ui` section of the config). */
export interface UiConfig {
  customCss?: string;
  custom_css?: string;
  customCssFiles?: string[];
  custom_css_files?: string[];
}

/** One provider (name, endpoint, API key, known models). */
export interface ProviderSpec {
  name: string;
  kind: 'openai' | 'anthropic';
  endpoint?: string | null;
  api_key?: string | null;
  models: string[];
  /** True when an API key is resolvable (explicit or env var). */
  has_key?: boolean;
  /** Provider-specific extra fields declared by the catalog (WP-LLM F3-2). */
  extra?: Record<string, string>;
}

/** Executable paths for language runtimes (resolved with defaults). */
export interface Runtimes {
  python: string;
  python3: string;
  node: string;
  php: string;
  docker: string;
  git: string;
}

export interface DockerStatus {
  executable: string;
  available: boolean;
  version?: string | null;
  error?: string | null;
}

/** One raw config layer file (global `~/.config/bebok/config.json` or project `<dir>/.bebok/config.json`). */
export interface ConfigLayerFile {
  exists: boolean;
  path: string;
  /** Raw file text (`{}` when the file does not exist yet). */
  content: string;
}

/** Which layer `PUT /config` writes to. */
export type ConfigScope = 'project' | 'global';

/** Shape of `GET /config` / `PUT /config` (resolved view + raw layer files). */
export interface ConfigResponse {
  config: ResolvedConfig;
  /** Effective provider registry (built-ins + config overrides). */
  providers: ProviderSpec[];
  skills: ResolvedSkill[];
  mcp: McpStatus[];
  agents: AgentInfo[];
  runtimes: Runtimes;
  /** Untouched layer contents for the raw JSON editor (`config.json` tab). */
  files: { global: ConfigLayerFile; project: ConfigLayerFile };
}

// ---------------------------------------------------------------------------
// M6: explorer (/fs/*), providers (/models), session lifecycle
// ---------------------------------------------------------------------------

/** One directory entry from `GET /fs/tree`. */
export interface FsEntry {
  name: string;
  path: string;
  is_dir: boolean;
}

export interface FsTreeResponse {
  path: string;
  entries: FsEntry[];
}

export interface FsFileResponse {
  path: string;
  content: string;
  /** Present when `?binary=true` – the file was read as raw bytes. */
  binary?: boolean;
  /** MIME type detected from the file extension (only when `binary: true`). */
  media_type?: string;
}

export interface ModelsResponse {
  provider: string;
  models: string[];
}

export interface CreateSessionResult {
  sessionID: string;
  parent?: [string, number] | null;
}

export interface ExportResponse {
  session: SessionMeta;
  messages: Message[];
}

export interface CompactResponse {
  /** The *new* (forked) session holding the summary + tail. */
  sessionID: string;
  parent: [string, number];
  /** Context size before compaction (last-call gauge, else an estimate). */
  before?: number;
  /** Estimated context size of the forked transcript. */
  after?: number;
}

export interface DeleteSessionResponse {
  sessionID: string;
  directory: string;
  deleted: boolean;
  /**
   * WP-GIT: the deleted session's directory was a Bebok git worktree. The
   * engine never removes it as a side effect - the client may *offer* removal
   * through `EngineClient.removeWorktree` (explicit, separate call).
   */
  is_worktree?: boolean;
  /** Worktree path (same as `directory`) when `is_worktree`. */
  worktree_path?: string | null;
  worktree_branch?: string | null;
  /** Project root that owns the worktree (`<root>/.bebok/worktrees/...`). */
  project_root?: string | null;
}

// ---------------------------------------------------------------------------
// WP-GIT: git probe + worktree-backed sessions
// ---------------------------------------------------------------------------

/** `GET /projects/{id}/git` (mirrors `bebok_core::git::GitInfo` + project fields). */
export interface ProjectGitInfo {
  project_id: string;
  /** Registered, engine-normalised project path. */
  path: string;
  /** `<path>/.bebok/worktrees`, engine-built. */
  worktrees_dir: string;
  /** False for a non-repo directory or a host without `git`; other fields are then null. */
  is_repo: boolean;
  root: string | null;
  /** Current branch; null on a detached HEAD. */
  branch: string | null;
  remote_url: string | null;
  is_github: boolean;
  /** `git status --porcelain` line count (staged + unstaged + untracked). */
  dirty_count: number | null;
}

/** `POST /session` `worktree` field: run the session in a fresh git worktree. */
export interface WorktreeSpec {
  /** Branch to check out (created from `base` when new); also the path below `.bebok/worktrees`. */
  branch: string;
  /** Start point for a new branch; engine default is the current HEAD. */
  base?: string;
}

/** `POST /session` answer when a `worktree` spec was sent. */
export interface CreateWorktreeSessionResponse extends CreateSessionResponse {
  /** Normalised worktree path the session is bound to. */
  directory: string;
  worktree: { path: string; branch: string };
}

/** `POST /projects/{id}/git/worktree/remove` answer. */
export interface RemoveWorktreeResponse {
  removed: boolean;
  path: string;
}

// ---------------------------------------------------------------------------
// M6: debug log
// ---------------------------------------------------------------------------

export interface DebugEntry {
  ts: number;
  source: 'llm' | 'http';
  kind: 'request' | 'response' | 'error';
  title: string;
  detail: string;
}

/** One captured LLM call: full wire request + assembled response (memory-only, last 2). */
export interface DebugLlmCall {
  id: number;
  ts: number;
  model: string;
  request: unknown;
  response: unknown;
}

export interface DebugLogResponse {
  entries: DebugEntry[];
  maxChars: number;
  /** Last 2 full LLM request/response payloads (absent on old engines). */
  calls?: DebugLlmCall[];
}

// ---------------------------------------------------------------------------
// M5: terminal (PTY)
// ---------------------------------------------------------------------------

/** One terminal session returned by `GET /pty`. */
export interface PtyInfo {
  pty_id: string;
  cwd?: string | null;
  command: string;
  title?: string | null;
  exited: boolean;
  exit_code?: number | null;
}

export interface PtyListResponse {
  ptys: PtyInfo[];
}

export interface CreatePtyResponse {
  ptyId: string;
}

export interface PtyTicketResponse {
  ptyId: string;
  ticket: string;
}

// ---------------------------------------------------------------------------
// F5: projects registry (`/projects`) + directory picker (`/fs/browse`)
// ---------------------------------------------------------------------------

/** One registered project directory (mirrors `bebok_core::config::projects::ProjectEntry`). */
export interface ProjectEntry {
  id: string;
  name: string;
  /** Absolute, engine-normalised path - never split it client-side. */
  path: string;
  added_at: number;
  last_opened_at: number | null;
  pinned: boolean;
  /**
   * Free-form group name for the project switcher's collapsible sections
   * (F6-7). `null` (or missing, for entries from an older engine) means
   * "ungrouped".
   */
  group?: string | null;
}

export interface ProjectsListResponse {
  projects: ProjectEntry[];
}

/**
 * `PATCH /projects/{id}` body.
 *
 * Wire contract for `group` (mirrors `bebok_core::config::projects::ProjectPatch`):
 * omit the field to leave the group unchanged; send `''` (or whitespace-only)
 * to ungroup; send a non-empty name to set/move the group. There is no
 * separate "create group" call - a project's `group` value *is* the group.
 */
export interface ProjectPatch {
  name?: string;
  pinned?: boolean;
  group?: string;
}

/** WP-CHANGES (F6-9): which baseline a change diff / revert is computed against. */
export type ChangeBaseline = 'git' | 'snapshot';

/** One engine-tracked file change (`GET /session/{id}/changes`). */
export interface ChangeEntry {
  /** Project-relative path, forward slashes. */
  path: string;
  added: number;
  removed: number;
  baseline: ChangeBaseline;
  /** Whether the file currently exists on disk. */
  exists: boolean;
  /** F9-6: session that made the change (the main session or a descendant). */
  sessionID?: string;
  /** F9-6: label - the child's alias, or the session agent name for the main session (e.g. `main`). */
  agent?: string;
  /** F9-6: the change was made by a child (sub-agent) session. */
  isChild?: boolean;
}

export interface ChangesResponse {
  changes: ChangeEntry[];
}

/** `GET /session/{id}/changes/diff?path=`: a plain unified diff string. */
export interface ChangeDiffResponse {
  path: string;
  diff: string;
  baseline: ChangeBaseline;
  added: number;
  removed: number;
}

/** `POST /session/{id}/changes/revert`. */
export interface RevertChangeResponse {
  path: string;
  baseline: ChangeBaseline;
  exists: boolean;
}

/** One row of the directory picker: always a directory, never a file. */
export interface FsBrowseEntry {
  name: string;
  path: string;
  hidden: boolean;
  readable: boolean;
}

export interface FsBrowseResponse {
  /** The listed directory, or null when the response carries the host roots. */
  path: string | null;
  entries: FsBrowseEntry[];
}

// ---------------------------------------------------------------------------
// F7-5: usage statistics (`GET /stats`)
// ---------------------------------------------------------------------------

/** Token/cost totals shared by every stats row (`bebok_core::stats::Totals`). */
export interface StatsTotals {
  sessions: number;
  /** User prompts. */
  turns: number;
  /** LLM round-trips. */
  llm_calls: number;
  tool_calls: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  /** Sum over calls with known pricing; null when none had a price. */
  cost: number | null;
  /** Calls whose model had no pricing entry. */
  cost_unknown_calls: number;
}

/** One breakdown row (model / provider / agent / project): `key` + totals. */
export interface StatsBucket extends StatsTotals {
  key: string;
}

export interface StatsDay {
  /** `YYYY-MM-DD` (UTC). */
  day: string;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  cost: number | null;
  llm_calls: number;
  tool_calls: number;
}

export interface StatsSessionRow extends StatsTotals {
  id: string;
  title: string | null;
  directory: string;
  agent: string;
  updated_at: number;
}

export interface StatsToolRow {
  name: string;
  calls: number;
  errors: number;
}

export interface StatsResponse {
  range: {
    directory: string | null;
    from: number | null;
    to: number | null;
    scanned_sessions: number;
  };
  totals: StatsTotals;
  by_model: StatsBucket[];
  by_provider: StatsBucket[];
  by_agent: StatsBucket[];
  by_project: StatsBucket[];
  /** 30 contiguous days, oldest first. */
  by_day: StatsDay[];
  top_sessions: StatsSessionRow[];
  tools: StatsToolRow[];
  compaction: { count: number; avg_context_before: number | null };
}

export interface StatsQuery {
  directory?: string | null;
  /** Epoch ms or ISO date. */
  from?: number | string | null;
  to?: number | string | null;
}

// ---------------------------------------------------------------------------
// F9-14: background process registry (`bash background:true` / `bash_kill`)
// ---------------------------------------------------------------------------

/**
 * One registry row as serialised by the engine (snake_case struct fields) plus
 * the per-request decorations of `GET /session/{id}/processes` (`agent`, and
 * `port`/`url` when detected in the log).
 */
export interface ProcessInfo {
  /** uuid */
  id: string;
  /** camelCase alias some payloads carry; `session_id` is the canonical field. */
  sessionID?: string;
  session_id: string;
  command: string;
  cwd: string;
  pid: number;
  /** Unix ms. */
  started_at: number;
  status: 'running' | 'exited';
  exit_code?: number | null;
  /** Unix ms, set once exited. */
  ended_at?: number | null;
  /** Absolute path of `<root>/.bebok/run/<id>.log`. */
  log_path: string;
  /** Child alias or the owning session's agent name. */
  agent: string;
  /** First `http://localhost:<port>` / `127.0.0.1:<port>` / "port <n>" seen in the log. */
  port?: number;
  url?: string;
}

/** `GET /session/{id}/processes` (includes descendant sessions). */
export interface SessionProcessesResponse {
  processes: ProcessInfo[];
}

/** `GET /processes/{id}/log?tail=<bytes>`. */
export interface ProcessLogResponse {
  id: string;
  log: string;
  /** Total size of the log file in bytes (before the tail cut). */
  size: number;
}

/** `process.output` event properties (coalesced, at most ~3/s per process). */
export interface ProcessOutputEvent {
  id: string;
  sessionID: string;
  chunk: string;
  /** Unix ms. */
  at: number;
}

/** `process.exited` event properties. */
export interface ProcessExitedEvent {
  id: string;
  sessionID: string;
  code: number | null;
}
