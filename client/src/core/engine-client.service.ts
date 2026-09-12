/**
 * Engine REST client (SPEC §7 / §5 subset implemented in M1–M2).
 *
 * GUI only renders and sends input; every state change lives in the engine.
 * This service is a thin typed wrapper over `fetch`.
 */

import { Injectable, computed, signal } from '@angular/core';

import { authFetch } from './auth.interceptor';
import {
  AbortResponse,
  AbortTaskResponse,
  AgentInfo,
  AgentListResponse,
  CompactResponse,
  ConfigResponse,
  CreatePtyResponse,
  CreateSessionResponse,
  CreateSessionResult,
  DebugLogResponse,
  DeleteSessionResponse,
  DockerStatus,
  ExportResponse,
  FsBrowseResponse,
  FsFileResponse,
  FsTreeResponse,
  McpListResponse,
  McpStatus,
  Message,
  MessageListResponse,
  ModelsResponse,
  PermissionResponse,
  ProjectEntry,
  ProjectPatch,
  ProjectsListResponse,
  PromptBody,
  PtyInfo,
  PtyListResponse,
  PtyTicketResponse,
  SessionListResponse,
  SessionMeta,
} from './engine.dtos';
import { EngineConnection, TransportStrategy } from './transport.strategy';

export interface PermissionDecisionInput {
  decision: 'allow' | 'deny';
  always?: boolean;
}

@Injectable({ providedIn: 'root' })
export class EngineClient {
  private readonly transport = new TransportStrategy();

  /** The resolved engine connection (null until `connect()` succeeds). */
  readonly connection = signal<EngineConnection | null>(null);
  readonly connected = computed(() => this.connection() !== null);
  /**
   * True when the runtime is the Tauri desktop webview. Deliberately based on
   * the PLATFORM, not on `connection` - the connection is null until connect()
   * succeeds, so the start view must know the platform before that.
   */
  readonly isTauri = signal(this.transport.platform === 'tauri');

  get platform(): TransportStrategy['platform'] {
    return this.transport.platform;
  }

  /** True on the Capacitor (mobile) shell: remote engine over LAN. */
  get isCapacitor(): boolean {
    return this.transport.isCapacitor;
  }

  /** Resolve (and cache) the engine connection for the current platform. */
  async connect(): Promise<EngineConnection> {
    const existing = this.connection();
    if (existing) {
      return existing;
    }
    const conn = await this.transport.connect();
    this.connection.set(conn);
    return conn;
  }

  /**
   * Replace the connection (used when the remote engine URL changes). The URL
   * may carry the engine capability token (`?token=…` from `BEBOK_READY`);
   * `adoptRemote` stores it and returns the connection with a clean base URL.
   */
  reconfigure(conn: EngineConnection): void {
    this.connection.set(this.transport.adoptRemote(conn));
  }

  /**
   * Reachability probe. Any response proves the engine is up (the `/session`
   * route without a `directory` answers 400).
   */
  async ping(): Promise<void> {
    const conn = this.requireConnection();
    await authFetch(`${conn.baseUrl}/session`, { method: 'GET' });
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

  createSession(directory: string, agent?: string, model?: string): Promise<CreateSessionResponse> {
    return this.request<CreateSessionResponse>('POST', '/session', {
      directory,
      agent: agent ?? 'code',
      ...(model ? { model } : {}),
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

  exportSession(id: string): Promise<ExportResponse> {
    return this.request<ExportResponse>('GET', `/session/${id}/export`);
  }

  compactSession(id: string, budget?: number): Promise<CompactResponse> {
    return this.request<CompactResponse>('POST', `/session/${id}/compact`, {
      ...(budget ? { budget } : {}),
    });
  }

  prompt(
    id: string,
    body: PromptBody | string,
    agent?: string,
    model?: string,
  ): Promise<unknown> {
    const payload: PromptBody =
      typeof body === 'string'
        ? {
            message: body,
            ...(agent ? { agent } : {}),
            ...(model ? { model } : {}),
          }
        : {
            message: body.message,
            ...(body.agent ?? agent ? { agent: (body.agent ?? agent) as string } : {}),
            ...(body.model ?? model ? { model: (body.model ?? model) as string } : {}),
            ...(body.images?.length ? { images: body.images } : {}),
          };
    return this.request('POST', `/session/${id}/prompt`, payload);
  }

  abort(id: string): Promise<AbortResponse> {
    return this.request<AbortResponse>('POST', `/session/${id}/abort`);
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

  fsFileWrite(directory: string, path: string, content: string): Promise<{ path: string; saved: boolean }> {
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

  /**
   * List the subdirectories of `path`, or the host's roots when `path` is
   * omitted. Directory names only - this endpoint never returns file contents.
   */
  browseDirectory(path?: string | null, showHidden = false): Promise<FsBrowseResponse> {
    const query =
      (path ? `path=${encodeURIComponent(path)}&` : '') + `show_hidden=${showHidden ? 'true' : 'false'}`;
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
  // M5: terminal (PTY)
  // ---------------------------------------------------------------------------

  createPty(
    directory: string,
    cols?: number,
    rows?: number,
  ): Promise<CreatePtyResponse> {
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
    const init: RequestInit = { method };
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
      throw new Error(
        `engine ${method} ${path} -> ${res.status}${detail ? `: ${detail}` : ''}`,
      );
    }
    if (res.status === 204) {
      return undefined as T;
    }
    return (await res.json()) as T;
  }
}
