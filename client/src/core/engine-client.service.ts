/**
 * Engine REST client (SPEC §7 / §5 subset implemented in M1–M2).
 *
 * GUI only renders and sends input; every state change lives in the engine.
 * This service is a thin typed wrapper over `fetch`.
 *
 * WP-M2: the class is the HTTP implementation of `EngineApi` (F10-6) and can
 * be re-pointed at another engine at runtime through `switchTarget()` (F10-7)
 * - the phone talks to its embedded engine or to a paired desktop with the
 * same code, only base URL and token change.
 */

import { Injectable, computed, inject, signal } from '@angular/core';

import { authFetch, onEngineUnauthorized } from './auth.interceptor';
import type { EngineApi, PermissionDecisionInput } from './engine-api';
import { EngineTarget, EngineTargetStore } from './engine-target.store';
import {
  AbortResponse,
  AbortTaskResponse,
  AgentEntry,
  AgentInfo,
  AgentListResponse,
  ChangeDiffResponse,
  ChangeEntry,
  ChangesResponse,
  CompactResponse,
  ConfigResponse,
  CreatePtyResponse,
  CreateSessionResponse,
  CreateSessionResult,
  DebugLogResponse,
  DelegationModelsResponse,
  DeleteSessionResponse,
  DockerStatus,
  ExportResponse,
  FleetGenResponse,
  FsBrowseResponse,
  FsFileResponse,
  FsTreeResponse,
  McpListResponse,
  McpStatus,
  Message,
  MessageListResponse,
  ModelsResponse,
  PairWithDesktopResponse,
  PermissionResponse,
  PendingPermissionSnapshot,
  ProjectEntry,
  ProjectGitInfo,
  ProjectPatch,
  ProjectsListResponse,
  ProcessInfo,
  ProcessLogResponse,
  PromptBody,
  PtyInfo,
  PtyListResponse,
  PtyTicketResponse,
  RevertChangeResponse,
  SessionProcessesResponse,
  RemoveWorktreeResponse,
  SessionAgentsResponse,
  SessionListResponse,
  SessionMeta,
  SafetyCategory,
  StatsQuery,
  StatsResponse,
  ToolSafetyResponse,
  WorktreeSpec,
  BrowserAction,
  BrowserActionResult,
  BrowserFrame,
  BrowserState,
  RemoteApiError,
  RemoteDevice,
  RemoteDevicesResponse,
  RemotePairStart,
  RemoteStatus,
} from './engine.dtos';
import { pairWithDesktop } from './remote/pair-protocol';
import {
  EmbeddedEngineError,
  EngineConnection,
  ResolvedTarget,
  TransportStrategy,
} from './transport.strategy';

export type { PermissionDecisionInput } from './engine-api';

/** Fixed ids of the platform-resolved targets (one per platform kind). */
export const PLATFORM_TARGET_ID = {
  sidecar: 'sidecar',
  embedded: 'embedded',
  'remote-url': 'remote-url:default',
} as const;

@Injectable({ providedIn: 'root' })
export class EngineClient implements EngineApi {
  private readonly transport = new TransportStrategy();
  private readonly targets = inject(EngineTargetStore);
  /**
   * F10-7: aborts every request still in flight against the previous target
   * when `switchTarget()` runs, so a slow reply from engine A can never land
   * in a view that already shows engine B.
   */
  private inflight = new AbortController();

  /** The resolved engine connection (null until `connect()` succeeds). */
  readonly connection = signal<EngineConnection | null>(null);
  readonly connected = computed(() => this.connection() !== null);
  /**
   * True when the runtime is the Tauri desktop webview. Deliberately based on
   * the PLATFORM, not on `connection` - the connection is null until connect()
   * succeeds, so the start view must know the platform before that.
   */
  readonly isTauri = signal(this.transport.platform === 'tauri');
  /**
   * True once the engine rejected our capability token (401): the engine was
   * restarted (new per-launch token) or the address changed. Every request
   * keeps failing until the user hands us the new `BEBOK_READY` address (or,
   * on the desktop shell, we re-read it from the sidecar), so the shell shows
   * a blocking reconnect prompt while this is set. Cleared by `reconnect()`.
   */
  readonly unauthorized = signal(false);

  constructor() {
    onEngineUnauthorized(() => this.unauthorized.set(true));
  }

  get platform(): TransportStrategy['platform'] {
    return this.transport.platform;
  }

  /** True on the Capacitor (mobile) shell: remote engine over LAN. */
  get isCapacitor(): boolean {
    return this.transport.isCapacitor;
  }

  /**
   * Resolve (and cache) the engine connection for the current platform, and
   * register it as the platform's default target (F10-7). The resolution
   * itself is unchanged: Tauri sidecar / Capacitor embedded / saved remote
   * URL. What changed: a Capacitor shell whose embedded engine fails no longer
   * silently falls back to the saved URL - the failure is exposed through
   * `EngineTargetStore.platformError`, and only a previously *active*
   * persisted target (a paired desktop) is used instead.
   */
  async connect(): Promise<EngineConnection> {
    const existing = this.connection();
    if (existing) {
      return existing;
    }
    let resolved: ResolvedTarget;
    try {
      resolved = await this.transport.resolveDefaultTarget();
      this.targets.platformError.set(null);
    } catch (err) {
      if (err instanceof EmbeddedEngineError) {
        this.targets.platformError.set(err.message);
        await this.targets.ready;
        const fallback = this.targets.active();
        if (fallback && fallback.kind === 'desktop') {
          return this.applyTarget(fallback);
        }
      }
      throw err;
    }
    const target = this.targets.upsert({
      id: PLATFORM_TARGET_ID[resolved.kind],
      kind: resolved.kind,
      label: platformLabel(resolved.kind, resolved.connection.baseUrl),
      baseUrl: resolved.connection.baseUrl,
      token: resolved.token,
      ephemeral: true,
    });
    // The platform default wins on launch (the sidecar/embedded engine is the
    // one the shell just started); a persisted choice is restored explicitly
    // by the mobile shell when the user picked a desktop last time.
    this.targets.setActive(target.id);
    this.connection.set(resolved.connection);
    return resolved.connection;
  }

  /**
   * F10-7: make `id` the active engine. Installs its token (the single
   * `adopt()` path), sets `connection`, clears `unauthorized` and aborts the
   * requests still running against the previous target. `EventsStore`
   * observes `EngineTargetStore.activeId` and restarts its stream, bumping
   * `reconnectVersion` so open views `refreshFull`.
   */
  async switchTarget(id: string): Promise<EngineTarget> {
    const target = this.targets.byId(id);
    if (!target) {
      throw new Error(`unknown engine target: ${id}`);
    }
    const token = target.token ?? (await this.targets.tokenFor(id));
    const resolved = { ...target, token };
    this.applyTarget(resolved);
    return resolved;
  }

  private applyTarget(target: EngineTarget): EngineConnection {
    this.inflight.abort();
    this.inflight = new AbortController();
    const conn = this.transport.adopt(
      target.kind === 'sidecar' ? 'tauri' : 'http',
      target.baseUrl,
      target.token,
    );
    this.connection.set(conn);
    this.unauthorized.set(false);
    this.targets.setActive(target.id);
    return conn;
  }

  /** Targets known to this client (platform default + persisted ones). */
  get targetStore(): EngineTargetStore {
    return this.targets;
  }

  /**
   * Replace the connection (used when the remote engine URL changes). The URL
   * may carry the engine capability token (`?token=…` from `BEBOK_READY`);
   * `adoptRemote` stores it and returns the connection with a clean base URL.
   */
  reconfigure(conn: EngineConnection): void {
    const adopted = this.transport.adoptRemote(conn);
    this.connection.set(adopted);
    // Keep the platform `remote-url` target in step with the typed address so
    // the target list (mobile overflow sheet) never shows a stale URL.
    const id = PLATFORM_TARGET_ID['remote-url'];
    if (this.targets.byId(id)) {
      this.targets.upsert({
        id,
        kind: 'remote-url',
        label: platformLabel('remote-url', adopted.baseUrl),
        baseUrl: adopted.baseUrl,
        token: this.transport.readRemoteToken(),
        ephemeral: true,
      });
    }
  }

  /**
   * Recover from a rejected token. Browser/remote mode: `rawUrl` is the new
   * `BEBOK_READY http://host:port/?token=…` line (a plain URL keeps the saved
   * token). Desktop shell: re-reads the sidecar's current address. Resolves
   * once a probe gets through; rejects (leaving `unauthorized` set) when the
   * engine still answers 401 or is unreachable.
   */
  async reconnect(rawUrl?: string): Promise<void> {
    if (rawUrl !== undefined) {
      const normalized = rawUrl.trim().replace(/\/+$/, '');
      if (!normalized) {
        throw new Error('engine address is empty');
      }
      this.reconfigure({ kind: 'http', baseUrl: normalized });
    } else {
      this.connection.set(null);
      await this.connect();
    }
    this.unauthorized.set(false);
    await this.ping();
    if (this.unauthorized()) {
      throw new Error('engine rejected the token (401)');
    }
    const active = this.targets.activeId();
    if (active) {
      this.targets.markOk(active);
    }
  }

  /**
   * Reachability probe. Any response proves the engine is up (the `/session`
   * route without a `directory` answers 400).
   */
  async ping(): Promise<void> {
    const conn = this.requireConnection();
    await authFetch(`${conn.baseUrl}/session`, { method: 'GET' });
  }

  /** `GET /version` -> the engine's own version (workspace version). */
  getVersion(): Promise<{ version: string }> {
    return this.request<{ version: string }>('GET', '/version');
  }

  /** Tauri native directory picker; null in browser/http mode. */
  pickDirectory(title: string): Promise<string | null> {
    return this.transport.pickDirectory(title);
  }

  /** Tauri native file picker (executable paths); null in browser/http mode. */
  pickFile(title: string): Promise<string | null> {
    return this.transport.pickFile(title);
  }

  /** Last saved remote engine location (browser/http mode). */
  remoteDefaults(): { baseUrl: string } {
    return {
      baseUrl: this.transport.readRemoteUrl(),
    };
  }

  saveDirectory(directory: string): void {
    this.transport.saveDirectory(directory);
  }

  readLastDirectory(): string | null {
    return this.transport.readLastDirectory();
  }

  // ---------------------------------------------------------------------------
  // REST endpoints
  // ---------------------------------------------------------------------------

  /**
   * Create a session in `directory`. With a `worktree` spec (WP-GIT) the
   * engine first runs `git worktree add <directory>/.bebok/worktrees/<branch>`
   * and binds the session to that worktree instead; the response then also
   * carries `directory` + `worktree` (see `CreateWorktreeSessionResponse`).
   */
  createSession(
    directory: string,
    agent?: string,
    model?: string,
    worktree?: WorktreeSpec,
  ): Promise<CreateSessionResponse> {
    return this.request<CreateSessionResponse>('POST', '/session', {
      directory,
      agent: agent ?? 'code',
      ...(model ? { model } : {}),
      ...(worktree ? { worktree } : {}),
    });
  }

  /**
   * Create an independent branch of a session, retaining messages through
   * `messageIndex`. Callers that need the state before a message pass its
   * preceding index.
   */
  forkSession(
    directory: string,
    sessionID: string,
    messageIndex: number,
  ): Promise<CreateSessionResult> {
    return this.request<CreateSessionResult>('POST', '/session', {
      directory,
      forkOf: { sessionID, messageIndex },
    });
  }

  /** Resume the most recent session for a directory (`continueLast`). */
  continueLast(directory: string): Promise<CreateSessionResult> {
    return this.request<CreateSessionResult>('POST', '/session', {
      directory,
      continueLast: true,
    });
  }

  /**
   * Rewind a session in place (`POST /session/{id}/truncate`): drop every
   * message from `keep` onwards so the transcript ends right before the point
   * being rolled back to. Returns the same session (no fork / new session).
   */
  truncateSession(sessionID: string, keep: number): Promise<CreateSessionResult> {
    return this.request<CreateSessionResult>(
      'POST',
      `/session/${encodeURIComponent(sessionID)}/truncate`,
      { keep },
    );
  }

  /**
   * Permanently delete a session (`DELETE /session/{id}`): removes the
   * in-memory state and the on-disk transcript. The engine refuses with 409
   * while a turn is running on the session.
   */
  deleteSession(sessionID: string): Promise<DeleteSessionResponse> {
    return this.request<DeleteSessionResponse>(
      'DELETE',
      `/session/${encodeURIComponent(sessionID)}`,
    );
  }

  listSessions(directory: string): Promise<SessionMeta[]> {
    const data = this.request<SessionListResponse>(
      'GET',
      `/session?directory=${encodeURIComponent(directory)}`,
    );
    return data.then((d) => d.sessions);
  }

  sessionMeta(id: string): Promise<SessionMeta> {
    return this.request<SessionMeta>('GET', `/session/${id}`);
  }

  messages(id: string): Promise<Message[]> {
    return this.request<MessageListResponse>('GET', `/session/${id}/message`).then(
      (d) => d.messages,
    );
  }

  /** F6-12: live + finished sub-agents delegated from `id` (`task`/`fleet`). */
  sessionAgents(id: string): Promise<AgentEntry[]> {
    return this.request<SessionAgentsResponse>('GET', `/session/${id}/agents`).then(
      (d) => d.agents,
    );
  }

  exportSession(id: string): Promise<ExportResponse> {
    return this.request<ExportResponse>('GET', `/session/${id}/export`);
  }

  compactSession(id: string, budget?: number): Promise<CompactResponse> {
    return this.request<CompactResponse>('POST', `/session/${id}/compact`, {
      ...(budget ? { budget } : {}),
    });
  }

  // ---------------------------------------------------------------------------
  // WP-CHANGES (F6-9): engine-tracked file changes
  // ---------------------------------------------------------------------------

  /** `GET /session/{id}/changes` -> tracked files with +/- line counts. */
  sessionChanges(id: string): Promise<ChangeEntry[]> {
    return this.request<ChangesResponse>('GET', `/session/${encodeURIComponent(id)}/changes`).then(
      (d) => d.changes,
    );
  }

  /**
   * `GET /session/{id}/changes/diff?path=[&session=]` -> unified diff against
   * the baseline. F9-6: `session` picks the (descendant) session whose
   * tracker holds the path; omitted, the engine searches main then children.
   */
  sessionChangeDiff(id: string, path: string, session?: string): Promise<ChangeDiffResponse> {
    const query =
      `path=${encodeURIComponent(path)}` +
      (session ? `&session=${encodeURIComponent(session)}` : '');
    return this.request<ChangeDiffResponse>(
      'GET',
      `/session/${encodeURIComponent(id)}/changes/diff?${query}`,
    );
  }

  /** `POST /session/{id}/changes/revert` -> restore the baseline (bytes or absence). */
  revertSessionChange(id: string, path: string, session?: string): Promise<RevertChangeResponse> {
    return this.request<RevertChangeResponse>(
      'POST',
      `/session/${encodeURIComponent(id)}/changes/revert`,
      session ? { path, session } : { path },
    );
  }

  prompt(id: string, body: PromptBody | string, agent?: string, model?: string, fleet?: boolean): Promise<unknown> {
    const payload: PromptBody =
      typeof body === 'string'
        ? {
            message: body,
            ...(agent ? { agent } : {}),
            ...(model ? { model } : {}),
            ...(fleet ? { fleet: true } : {}),
          }
        : {
            message: body.message,
            ...((body.agent ?? agent) ? { agent: (body.agent ?? agent) as string } : {}),
            ...((body.model ?? model) ? { model: (body.model ?? model) as string } : {}),
            ...(body.images?.length ? { images: body.images } : {}),
            ...((body.fleet ?? fleet) ? { fleet: true } : {}),
          };
    return this.request('POST', `/session/${id}/prompt`, payload);
  }

  abort(id: string): Promise<AbortResponse> {
    return this.request<AbortResponse>('POST', `/session/${id}/abort`);
  }

  // --- browser viewer (WP-BROWSER2 / F7-6) ---------------------------------

  /** State of the session's agent browser (open/headed/url/display). */
  browserState(id: string): Promise<BrowserState> {
    return this.request<BrowserState>('GET', `/session/${id}/browser`);
  }

  /** One frame right now; rejects with a 404 error when no browser is open. */
  browserFrame(id: string): Promise<BrowserFrame> {
    return this.request<BrowserFrame>('GET', `/session/${id}/browser/frame`);
  }

  /** Drive the session's browser by hand (same tools + permission rules as the model). */
  browserAction(
    id: string,
    action: BrowserAction,
    body: Record<string, unknown> = {},
  ): Promise<BrowserActionResult> {
    return this.request<BrowserActionResult>(
      'POST',
      `/session/${id}/browser/${encodeURIComponent(action)}`,
      body,
    );
  }

  /** Abort a specific child task spawned by the orchestrator. */
  abortTask(sessionID: string, taskID: string): Promise<AbortTaskResponse> {
    return this.request<AbortTaskResponse>(
      'POST',
      `/session/${encodeURIComponent(sessionID)}/task/${encodeURIComponent(taskID)}/abort`,
    );
  }

  /**
   * Resolve a pending `permission.asked` request.
   *
   * The first resolver wins; a late resolution returns 404 which we treat as
   * "already resolved" instead of an error (another client answered first).
   */
  async resolvePermission(
    id: string,
    requestId: string,
    decision: PermissionDecisionInput,
  ): Promise<PermissionResponse> {
    try {
      return await this.request<PermissionResponse>(
        'POST',
        `/session/${id}/permission/${encodeURIComponent(requestId)}`,
        { decision: decision.decision, always: decision.always ?? false },
      );
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (message.includes('404')) {
        return {
          sessionID: id,
          requestID: requestId,
          decision: decision.decision,
          resolved: false,
        };
      }
      throw err;
    }
  }

  pendingPermissions(directory: string): Promise<PendingPermissionSnapshot[]> {
    return this.request<{ asks: PendingPermissionSnapshot[] }>(
      'GET',
      `/permission?directory=${encodeURIComponent(directory)}`,
    ).then((response) => response.asks);
  }

  /**
   * WP-M6 (F10-23): `POST /remote/pair` against a desktop's remote listener.
   * Unauthenticated and addressed at `endpoint` rather than the active
   * target, so it bypasses `request()`; see `core/remote/pair-protocol.ts`.
   */
  pairWithDesktop(
    endpoint: string,
    code: string,
    deviceName: string,
    extra: { model?: string; platform?: string; signal?: AbortSignal } = {},
  ): Promise<PairWithDesktopResponse> {
    return pairWithDesktop(endpoint, code, deviceName, extra);
  }

  /** Absolute URL of the global SSE stream (used by `EventsStore`). */
  eventUrl(): string {
    const conn = this.requireConnection();
    return `${conn.baseUrl}/event`;
  }

  // ---------------------------------------------------------------------------
  // M4: agents, MCP, config
  // ---------------------------------------------------------------------------

  listAgents(directory: string): Promise<AgentInfo[]> {
    return this.request<AgentListResponse>(
      'GET',
      `/agent?directory=${encodeURIComponent(directory)}`,
    ).then((d) => d.agents);
  }

  listMcp(directory: string): Promise<McpStatus[]> {
    return this.request<McpListResponse>(
      'GET',
      `/mcp?directory=${encodeURIComponent(directory)}`,
    ).then((d) => d.servers);
  }

  toggleMcp(directory: string, name: string, enabled: boolean): Promise<McpStatus[]> {
    return this.request<{ servers: McpStatus[] }>(
      'POST',
      `/mcp/${encodeURIComponent(name)}/toggle?directory=${encodeURIComponent(directory)}`,
      { enabled },
    ).then((d) => d.servers);
  }

  getConfig(directory: string): Promise<ConfigResponse> {
    return this.request<ConfigResponse>(
      'GET',
      `/config?directory=${encodeURIComponent(directory)}`,
    );
  }

  putConfig(
    directory: string,
    delta: unknown,
    opts?: { scope?: 'project' | 'global'; replace?: boolean },
  ): Promise<ConfigResponse> {
    const query =
      `directory=${encodeURIComponent(directory)}` +
      (opts?.scope ? `&scope=${encodeURIComponent(opts.scope)}` : '') +
      (opts?.replace ? '&replace=true' : '');
    return this.request<ConfigResponse>('PUT', `/config?${query}`, delta);
  }

  /** F9-10: `GET /delegation/models?directory=` -> the sub-agent model policy resolved right now. */
  delegationModels(directory: string): Promise<DelegationModelsResponse> {
    return this.request<DelegationModelsResponse>(
      'GET',
      `/delegation/models?directory=${encodeURIComponent(directory)}`,
    );
  }

  /**
   * `POST /fleet/generate?directory=` -> the engine proposes fleet members
   * (name/agent/model) from the configured providers. Every field of `opts` is
   * optional; an empty provider pool is answered with 400 by the engine.
   */
  generateFleet(
    directory: string,
    opts?: { minPerType?: number; types?: string[] },
  ): Promise<FleetGenResponse> {
    return this.request<FleetGenResponse>(
      'POST',
      `/fleet/generate?directory=${encodeURIComponent(directory)}`,
      opts ?? {},
    );
  }

  /** F7-7: every tool the engine knows with its safety category. */
  getToolSafety(directory: string): Promise<ToolSafetyResponse> {
    return this.request<ToolSafetyResponse>(
      'GET',
      `/tools/safety?directory=${encodeURIComponent(directory)}`,
    );
  }

  /**
   * F7-7: merge category overrides into a config layer's `tool_safety` map
   * (`null` removes an override = reset to default). Global by default - a
   * category is a user-level judgement shared by every project.
   */
  putToolSafety(
    directory: string,
    overrides: Record<string, SafetyCategory | null>,
    opts?: { scope?: 'project' | 'global' },
  ): Promise<ToolSafetyResponse> {
    const query =
      `directory=${encodeURIComponent(directory)}` +
      (opts?.scope ? `&scope=${encodeURIComponent(opts.scope)}` : '');
    return this.request<ToolSafetyResponse>('PUT', `/tools/safety?${query}`, { overrides });
  }

  /** Probe Docker access for a directory (resolves `runtimes.docker`). */
  checkDocker(directory: string): Promise<DockerStatus> {
    return this.request<{ docker: DockerStatus }>(
      'GET',
      `/docker?directory=${encodeURIComponent(directory)}`,
    ).then((d) => d.docker);
  }

  // ---------------------------------------------------------------------------
  // M6: explorer (/fs/*), providers (/models)
  // ---------------------------------------------------------------------------

  fsTree(directory: string, path?: string): Promise<FsTreeResponse> {
    const query = `directory=${encodeURIComponent(directory)}${
      path ? `&path=${encodeURIComponent(path)}` : ''
    }`;
    return this.request<FsTreeResponse>('GET', `/fs/tree?${query}`);
  }

  fsFile(directory: string, path: string): Promise<FsFileResponse> {
    return this.request<FsFileResponse>(
      'GET',
      `/fs/file?directory=${encodeURIComponent(directory)}&path=${encodeURIComponent(path)}`,
    );
  }

  fsFileBinary(directory: string, path: string): Promise<FsFileResponse> {
    return this.request<FsFileResponse>(
      'GET',
      `/fs/file?directory=${encodeURIComponent(directory)}&path=${encodeURIComponent(path)}&binary=true`,
    );
  }

  fsFileWrite(
    directory: string,
    path: string,
    content: string,
  ): Promise<{ path: string; saved: boolean }> {
    return this.request<{ path: string; saved: boolean }>(
      'PUT',
      `/fs/file?directory=${encodeURIComponent(directory)}&path=${encodeURIComponent(path)}`,
      { content },
    );
  }

  /** List available models for a provider ("check available models"). */
  listModels(directory: string, provider: string): Promise<ModelsResponse> {
    return this.request<ModelsResponse>(
      'GET',
      `/models?directory=${encodeURIComponent(directory)}&provider=${encodeURIComponent(provider)}`,
    );
  }

  // ---------------------------------------------------------------------------
  // F5: projects registry (/projects) + directory picker (/fs/browse)
  // ---------------------------------------------------------------------------

  /** Registered projects, already in display order (pinned, recent, name). */
  listProjects(): Promise<ProjectEntry[]> {
    return this.request<ProjectsListResponse>('GET', '/projects').then((d) => d.projects);
  }

  /** Register a directory. Re-adding a known path returns the existing entry. */
  addProject(path: string, name?: string): Promise<ProjectEntry> {
    return this.request<ProjectEntry>('POST', '/projects', {
      path,
      ...(name ? { name } : {}),
    });
  }

  updateProject(id: string, patch: ProjectPatch): Promise<ProjectEntry> {
    return this.request<ProjectEntry>('PATCH', `/projects/${encodeURIComponent(id)}`, patch);
  }

  /** Forget a project. The directory itself is never touched. */
  removeProject(id: string): Promise<{ removed: boolean; id: string }> {
    return this.request<{ removed: boolean; id: string }>(
      'DELETE',
      `/projects/${encodeURIComponent(id)}`,
    );
  }

  /** Stamp `last_opened_at` and get back the normalised path to switch to. */
  openProject(id: string): Promise<ProjectEntry> {
    return this.request<ProjectEntry>('POST', `/projects/${encodeURIComponent(id)}/open`);
  }

  // ---------------------------------------------------------------------------
  // WP-GIT: git probe + worktree removal (/projects/{id}/git*)
  // ---------------------------------------------------------------------------

  /**
   * Probe the registered project's git state. Never fails for a non-repo:
   * `is_repo` is false and the other fields null (missing `git` included).
   */
  projectGit(id: string): Promise<ProjectGitInfo> {
    return this.request<ProjectGitInfo>('GET', `/projects/${encodeURIComponent(id)}/git`);
  }

  /**
   * Remove a Bebok worktree (`git worktree remove --force`). Explicit and
   * separate from `deleteSession` by design; the engine refuses (400) any
   * path outside the project's own `.bebok/worktrees/`.
   */
  removeWorktree(id: string, path: string): Promise<RemoveWorktreeResponse> {
    return this.request<RemoveWorktreeResponse>(
      'POST',
      `/projects/${encodeURIComponent(id)}/git/worktree/remove`,
      { path },
    );
  }

  /**
   * List the subdirectories of `path`, or the host's roots when `path` is
   * omitted. Directory names only - this endpoint never returns file contents.
   */
  browseDirectory(path?: string | null, showHidden = false): Promise<FsBrowseResponse> {
    const query =
      (path ? `path=${encodeURIComponent(path)}&` : '') +
      `show_hidden=${showHidden ? 'true' : 'false'}`;
    return this.request<FsBrowseResponse>('GET', `/fs/browse?${query}`);
  }

  // ---------------------------------------------------------------------------
  // M6: debug log
  // ---------------------------------------------------------------------------

  debugLog(): Promise<DebugLogResponse> {
    return this.request<DebugLogResponse>('GET', '/debug/log');
  }

  clearDebugLog(): Promise<{ ok: boolean }> {
    return this.request<{ ok: boolean }>('DELETE', '/debug/log');
  }

  // ---------------------------------------------------------------------------
  // F7-5: usage statistics
  // ---------------------------------------------------------------------------

  /** `GET /stats` - aggregates over every persisted session (all projects when `directory` is omitted). */
  stats(query: StatsQuery = {}): Promise<StatsResponse> {
    const params = new URLSearchParams();
    if (query.directory) {
      params.set('directory', query.directory);
    }
    if (query.from !== undefined && query.from !== null) {
      params.set('from', String(query.from));
    }
    if (query.to !== undefined && query.to !== null) {
      params.set('to', String(query.to));
    }
    const qs = params.toString();
    return this.request<StatsResponse>('GET', qs ? `/stats?${qs}` : '/stats');
  }

  // ---------------------------------------------------------------------------
  // M5: terminal (PTY)
  // ---------------------------------------------------------------------------

  createPty(directory: string, cols?: number, rows?: number): Promise<CreatePtyResponse> {
    return this.request<CreatePtyResponse>('POST', '/pty', {
      ...(directory ? { directory } : {}),
      ...(cols ? { cols } : {}),
      ...(rows ? { rows } : {}),
    });
  }

  listPtys(): Promise<PtyInfo[]> {
    return this.request<PtyListResponse>('GET', '/pty').then((d) => d.ptys);
  }

  /** One-time, short-lived connect ticket for a terminal session. */
  async getTicket(ptyId: string): Promise<string> {
    const res = await this.request<PtyTicketResponse>(
      'POST',
      `/pty/${encodeURIComponent(ptyId)}/ticket`,
    );
    return res.ticket;
  }

  /**
   * Absolute WebSocket URL of a terminal connect endpoint, including the
   * one-time `?ticket=`. Browsers cannot set custom headers on a WS upgrade,
   * hence the ticket in the query string.
   */
  async ptyWebSocketUrl(ptyId: string): Promise<string> {
    const conn = this.requireConnection();
    const ticket = await this.getTicket(ptyId);
    const wsBase = conn.baseUrl.replace(/^http/, 'ws');
    return `${wsBase}/pty/${encodeURIComponent(ptyId)}/connect?ticket=${encodeURIComponent(ticket)}`;
  }

  // ---------------------------------------------------------------------------
  // F9-14: background process registry
  // ---------------------------------------------------------------------------

  /** `GET /session/{id}/processes` -> processes of the session and its descendants. */
  sessionProcesses(sessionId: string): Promise<ProcessInfo[]> {
    return this.request<SessionProcessesResponse>(
      'GET',
      `/session/${encodeURIComponent(sessionId)}/processes`,
    ).then((d) => d.processes ?? []);
  }

  /** `GET /processes/{id}/log?tail=<bytes>` -> the (tail of the) log file. */
  processLog(id: string, tail?: number): Promise<ProcessLogResponse> {
    const query = tail !== undefined ? `?tail=${encodeURIComponent(String(tail))}` : '';
    return this.request<ProcessLogResponse>(
      'GET',
      `/processes/${encodeURIComponent(id)}/log${query}`,
    );
  }

  /** `POST /processes/{id}/kill` -> the process row after the kill (whole tree). */
  killProcess(id: string): Promise<ProcessInfo> {
    return this.request<ProcessInfo>('POST', `/processes/${encodeURIComponent(id)}/kill`);
  }

  // ---------------------------------------------------------------------------
  // WP-M4 (F10-13): remote pairing - plain REST calls against this same
  // engine's local listener; WP-M1 already exposes `/remote/*` on the router
  // Tauri talks to, so there is no second transport or new crypto here.
  // ---------------------------------------------------------------------------

  getRemoteStatus(): Promise<RemoteStatus> {
    return this.remoteRequest<RemoteStatus>('GET', '/remote/status');
  }

  enableRemote(): Promise<RemoteStatus> {
    return this.remoteRequest<RemoteStatus>('POST', '/remote/enable');
  }

  disableRemote(): Promise<RemoteStatus> {
    return this.remoteRequest<RemoteStatus>('POST', '/remote/disable');
  }

  startPairing(): Promise<RemotePairStart> {
    return this.remoteRequest<RemotePairStart>('POST', '/remote/pair/start');
  }

  confirmPairing(pairId: string): Promise<RemoteDevice> {
    return this.remoteRequest<RemoteDevice>(
      'POST',
      `/remote/pair/confirm/${encodeURIComponent(pairId)}`,
    );
  }

  rejectPairing(pairId: string): Promise<{ ok: boolean }> {
    return this.remoteRequest<{ ok: boolean }>(
      'POST',
      `/remote/pair/reject/${encodeURIComponent(pairId)}`,
    );
  }

  listDevices(): Promise<RemoteDevice[]> {
    return this.remoteRequest<RemoteDevicesResponse>('GET', '/remote/devices').then(
      (d) => d.devices ?? [],
    );
  }

  revokeDevice(id: string): Promise<{ ok: boolean }> {
    return this.remoteRequest<{ ok: boolean }>(
      'DELETE',
      `/remote/devices/${encodeURIComponent(id)}`,
    );
  }

  // ---------------------------------------------------------------------------
  // helpers
  // ---------------------------------------------------------------------------

  private requireConnection(): EngineConnection {
    const conn = this.connection();
    if (!conn) {
      throw new Error('engine not connected');
    }
    return conn;
  }

  private async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const conn = this.requireConnection();
    const init: RequestInit = { method, signal: this.inflight.signal };
    if (body !== undefined) {
      init.body = JSON.stringify(body);
    }
    const res = await authFetch(`${conn.baseUrl}${path}`, init);
    if (!res.ok) {
      let detail = '';
      try {
        detail = (await res.text()).trim();
      } catch {
        /* keep status only */
      }
      throw new Error(`engine ${method} ${path} -> ${res.status}${detail ? `: ${detail}` : ''}`);
    }
    if (res.status === 204) {
      return undefined as T;
    }
    return (await res.json()) as T;
  }

  /**
   * Like `request()`, but on a non-2xx response parses the engine's
   * `{ error, message }` body into a `RemoteApiError` with a stable `.code`
   * - the Remote panel branches on codes (`remote_disabled`,
   * `pair_not_requested`, `pair_rejected`, ...) rather than status text.
   */
  private async remoteRequest<T>(method: string, path: string, body?: unknown): Promise<T> {
    const conn = this.requireConnection();
    const init: RequestInit = { method, signal: this.inflight.signal };
    if (body !== undefined) {
      init.body = JSON.stringify(body);
    }
    const res = await authFetch(`${conn.baseUrl}${path}`, init);
    if (!res.ok) {
      let code = 'unknown';
      let message = `engine ${method} ${path} -> ${res.status}`;
      try {
        const parsed = (await res.json()) as { error?: string; message?: string };
        if (parsed.error) {
          code = parsed.error;
        }
        if (parsed.message) {
          message = parsed.message;
        }
      } catch {
        /* keep the generic message */
      }
      throw new RemoteApiError(code, message, res.status);
    }
    if (res.status === 204) {
      return undefined as T;
    }
    return (await res.json()) as T;
  }
}

/** Default label of a platform-resolved target (the UI may re-label later). */
function platformLabel(kind: EngineTarget['kind'], baseUrl: string): string {
  switch (kind) {
    case 'sidecar':
      return 'Desktop engine';
    case 'embedded':
      return 'This device';
    default:
      return baseUrl;
  }
}
