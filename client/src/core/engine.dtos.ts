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

export interface ToolStateCompleted {
  state: 'completed';
  input: unknown;
  output: string;
  title: string;
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

export type Part = TextPart | ThinkingPart | ToolPart | UsagePart;

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
  /** Normalized working directory the session is bound to. */
  directory: string;
  title?: string | null;
  agent: string;
  model?: string | null;
  parent?: [string, number] | null;
  created_at: number;
  updated_at: number;
  usage: UsageTotals;
  share?: unknown;
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

export interface AbortResponse {
  sessionID: string;
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
  permission: unknown;
  mcp: unknown;
  skills: unknown;
  terminal: unknown;
  runtimes: unknown;
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
  sessionID: string;
  parent: [string, number];
}

export interface DeleteSessionResponse {
  sessionID: string;
  directory: string;
  deleted: boolean;
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

export interface DebugLogResponse {
  entries: DebugEntry[];
  maxChars: number;
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
