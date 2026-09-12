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

export interface ToolPart {
  type: 'tool';
  id: string;
  name: string;
  state: ToolState;
}

export interface UsagePart {
  type: 'usage';
  input_tokens: number;
  output_tokens: number;
  cost?: number | null;
  cache_read_input_tokens?: number | null;
  cache_creation_input_tokens?: number | null;
}

export type Part = TextPart | ThinkingPart | ToolPart | UsagePart | ImagePart;

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
export type AgentStatus = 'running' | 'done' | 'failed' | 'aborted' | 'unknown';

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
  permission: unknown;
  mcp: unknown;
  skills: unknown;
  terminal: unknown;
  runtimes: unknown;
  /** Client-only UI overrides: custom CSS (plain text, never executed by the engine). */
  ui?: UiConfig;
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
