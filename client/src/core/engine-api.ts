/**
 * `EngineApi` (WP-M2 / F10-6): the public surface of the engine client as an
 * interface, plus the `ENGINE_API` injection token.
 *
 * The 1.6 mobile shell talks to two engines - the one embedded on the phone
 * (mode "Chat") and a paired desktop engine (mode "Remote") - through the very
 * same HTTP + SSE implementation (`EngineClient`), which only swaps its base
 * URL and token (`EngineTargetStore`). A relay-backed transport planned for
 * 1.7 must slot in *without* touching the UI, so everything in `core/*` and
 * the `EventsStore` depends on this contract rather than on the concrete
 * class. Views/panels may keep injecting `EngineClient` directly: the token
 * resolves to the same singleton (`useExisting`), so both paths share state.
 *
 * Every method below mirrors `EngineClient` one to one - see the class for
 * the per-route documentation. Keep the two in sync (the `implements` clause
 * makes the compiler enforce it).
 */

import { InjectionToken, Signal, WritableSignal, inject } from '@angular/core';

import { EngineClient } from './engine-client.service';
import type { EngineTarget } from './engine-target.store';
import type {
  AbortResponse,
  CloudSessionList,
  CloudSessionSnapshot,
  SessionShare,
  SessionShareEntry,
  AbortTaskResponse,
  AgentEntry,
  AgentInfo,
  BrowserAction,
  BrowserActionResult,
  BrowserFrame,
  BrowserState,
  ChangeDiffResponse,
  ChangeEntry,
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
  FsBrowseResponse,
  FsFileResponse,
  FsTreeResponse,
  McpStatus,
  Message,
  ModelsResponse,
  PairWithDesktopResponse,
  PendingPermissionSnapshot,
  PermissionResponse,
  ProcessInfo,
  ProcessLogResponse,
  ProjectEntry,
  ProjectGitInfo,
  ProjectPatch,
  PromptBody,
  PtyInfo,
  RemoveWorktreeResponse,
  RevertChangeResponse,
  SafetyCategory,
  SessionMeta,
  StatsQuery,
  StatsResponse,
  ToolSafetyResponse,
  WorktreeSpec,
  RemoteStatus,
  RemoteDevice,
  RemotePairStart,
} from './engine.dtos';
import type { EngineConnection, PlatformKind } from './transport.strategy';

export interface PermissionDecisionInput {
  decision: 'allow' | 'deny';
  always?: boolean;
}

/** Connection state + lifecycle - the part every store reads. */
export interface EngineConnectionApi {
  /** The resolved engine connection (null until `connect()` succeeds). */
  readonly connection: WritableSignal<EngineConnection | null>;
  readonly connected: Signal<boolean>;
  /** True when the runtime is the Tauri desktop webview. */
  readonly isTauri: Signal<boolean>;
  /** True once the engine rejected our capability token (401). */
  readonly unauthorized: WritableSignal<boolean>;
  readonly platform: PlatformKind;
  /** True on the Capacitor (mobile) shell. */
  readonly isCapacitor: boolean;

  connect(): Promise<EngineConnection>;
  reconfigure(conn: EngineConnection): void;
  reconnect(rawUrl?: string): Promise<void>;
  ping(): Promise<void>;
  /**
   * F10-7: re-point every REST call and the SSE stream at a registered
   * target (see `EngineTargetStore`) without a page reload.
   */
  switchTarget(id: string): Promise<EngineTarget>;

  pickDirectory(title: string): Promise<string | null>;
  pickFile(title: string): Promise<string | null>;
  remoteDefaults(): { baseUrl: string };
  saveDirectory(directory: string): void;
  readLastDirectory(): string | null;
  /** `GET /workspace/chat` -> the engine-owned scratch directory of chat mode (1.8). */
  chatWorkspace(): Promise<{ directory: string }>;
  /** Absolute URL of the global SSE stream (used by `EventsStore`). */
  eventUrl(): string;
}

/** Every REST route the UI uses, in the order `EngineClient` declares them. */
export interface EngineRestApi {
  // sessions
  createSession(
    directory: string,
    agent?: string,
    model?: string,
    worktree?: WorktreeSpec,
  ): Promise<CreateSessionResponse>;
  forkSession(
    directory: string,
    sessionID: string,
    messageIndex: number,
  ): Promise<CreateSessionResult>;
  continueLast(directory: string): Promise<CreateSessionResult>;
  truncateSession(sessionID: string, keep: number): Promise<CreateSessionResult>;
  deleteSession(sessionID: string): Promise<DeleteSessionResponse>;
  listSessions(directory: string): Promise<SessionMeta[]>;
  sessionMeta(id: string): Promise<SessionMeta>;
  messages(id: string): Promise<Message[]>;
  sessionAgents(id: string): Promise<AgentEntry[]>;
  exportSession(id: string): Promise<ExportResponse>;
  compactSession(id: string, budget?: number): Promise<CompactResponse>;

  // changes
  sessionChanges(id: string): Promise<ChangeEntry[]>;
  sessionChangeDiff(id: string, path: string, session?: string): Promise<ChangeDiffResponse>;
  revertSessionChange(id: string, path: string, session?: string): Promise<RevertChangeResponse>;

  // turn control
  prompt(id: string, body: PromptBody | string, agent?: string, model?: string): Promise<unknown>;
  abort(id: string): Promise<AbortResponse>;
  abortTask(sessionID: string, taskID: string): Promise<AbortTaskResponse>;

  // browser viewer
  browserState(id: string): Promise<BrowserState>;
  browserFrame(id: string): Promise<BrowserFrame>;
  browserAction(
    id: string,
    action: BrowserAction,
    body?: Record<string, unknown>,
  ): Promise<BrowserActionResult>;

  // permissions
  resolvePermission(
    id: string,
    requestId: string,
    decision: PermissionDecisionInput,
  ): Promise<PermissionResponse>;
  pendingPermissions(directory: string): Promise<PendingPermissionSnapshot[]>;

  // WP-M6 (F10-23): phone-side pairing with a desktop engine
  /**
   * `POST /remote/pair` on `endpoint` (a clean base URL of the desktop's
   * remote listener, not the active target): long-polls up to 90 s for the
   * desktop's confirmation and resolves with the per-device token. Rejects
   * with `PairError` (`core/remote/pair-protocol.ts`) carrying the engine's
   * error code.
   */
  pairWithDesktop(
    endpoint: string,
    code: string,
    deviceName: string,
    extra?: { model?: string; platform?: string; signal?: AbortSignal },
  ): Promise<PairWithDesktopResponse>;

  // agents, MCP, config
  listAgents(directory: string): Promise<AgentInfo[]>;
  listMcp(directory: string): Promise<McpStatus[]>;
  toggleMcp(directory: string, name: string, enabled: boolean): Promise<McpStatus[]>;
  getConfig(directory: string): Promise<ConfigResponse>;
  putConfig(
    directory: string,
    delta: unknown,
    opts?: { scope?: 'project' | 'global'; replace?: boolean },
  ): Promise<ConfigResponse>;
  delegationModels(directory: string): Promise<DelegationModelsResponse>;
  getToolSafety(directory: string): Promise<ToolSafetyResponse>;
  putToolSafety(
    directory: string,
    overrides: Record<string, SafetyCategory | null>,
    opts?: { scope?: 'project' | 'global' },
  ): Promise<ToolSafetyResponse>;
  checkDocker(directory: string): Promise<DockerStatus>;

  // explorer + providers
  fsTree(directory: string, path?: string): Promise<FsTreeResponse>;
  fsFile(directory: string, path: string): Promise<FsFileResponse>;
  fsFileWrite(
    directory: string,
    path: string,
    content: string,
  ): Promise<{ path: string; saved: boolean }>;
  listModels(directory: string, provider: string): Promise<ModelsResponse>;

  // projects registry + git
  listProjects(): Promise<ProjectEntry[]>;
  addProject(path: string, name?: string): Promise<ProjectEntry>;
  updateProject(id: string, patch: ProjectPatch): Promise<ProjectEntry>;
  removeProject(id: string): Promise<{ removed: boolean; id: string }>;
  openProject(id: string): Promise<ProjectEntry>;
  projectGit(id: string): Promise<ProjectGitInfo>;
  removeWorktree(id: string, path: string): Promise<RemoveWorktreeResponse>;
  browseDirectory(path?: string | null, showHidden?: boolean): Promise<FsBrowseResponse>;

  // debug log
  debugLog(): Promise<DebugLogResponse>;
  clearDebugLog(): Promise<{ ok: boolean }>;

  // stats
  stats(query?: StatsQuery): Promise<StatsResponse>;

  // terminal (PTY)
  createPty(directory: string, cols?: number, rows?: number): Promise<CreatePtyResponse>;
  listPtys(): Promise<PtyInfo[]>;
  getTicket(ptyId: string): Promise<string>;
  ptyWebSocketUrl(ptyId: string): Promise<string>;

  // background processes
  sessionProcesses(sessionId: string): Promise<ProcessInfo[]>;
  processLog(id: string, tail?: number): Promise<ProcessLogResponse>;
  killProcess(id: string): Promise<ProcessInfo>;

  // remote pairing (WP-M4 / F10-13): `/remote/*` on this same engine's local
  // listener (WP-M1) - Settings -> Remote turns the second listener on/off
  // and manages paired devices; no new transport.
  getRemoteStatus(): Promise<RemoteStatus>;
  enableRemote(): Promise<RemoteStatus>;
  disableRemote(): Promise<RemoteStatus>;
  /** 1.8: `POST /remote/relay {enabled, url}` - persist + start/stop the relay. */
  setRemoteRelay(enabled: boolean, url: string): Promise<RemoteStatus>;
  /** 1.8: `POST /remote/relay/reset` - new tunnel id + secret (phones re-pair). */
  resetRemoteRelay(): Promise<RemoteStatus>;
  /** 1.8 share links (desktop only): mint / list the links of a session. */
  createSessionShare(id: string, label?: string): Promise<SessionShare>;
  listSessionShares(id: string): Promise<SessionShareEntry[]>;
  /** 1.8 cloud chats: `POST /session/{id}/cloud {enabled}` (desktop only). */
  setSessionCloud(id: string, enabled: boolean): Promise<{ id: string; cloud: boolean }>;
  /**
   * 1.8 cloud chats, phone side: the relay's own `/cloud/sessions[/<id>]`,
   * answered from the Durable Object even while the desktop is offline.
   * Rejects when the active connection is not a relay tunnel.
   */
  cloudSessions(): Promise<CloudSessionList>;
  cloudSession(id: string): Promise<CloudSessionSnapshot>;
  startPairing(): Promise<RemotePairStart>;
  confirmPairing(pairId: string): Promise<RemoteDevice>;
  rejectPairing(pairId: string): Promise<{ ok: boolean }>;
  listDevices(): Promise<RemoteDevice[]>;
  revokeDevice(id: string): Promise<{ ok: boolean }>;
}

/** The complete engine contract the client code depends on. */
export interface EngineApi extends EngineConnectionApi, EngineRestApi {}

/**
 * Injection token for the engine contract. `app.config.ts` binds it to the
 * `EngineClient` singleton (`useExisting`); the root factory below makes the
 * same binding the default so `TestBed`s and lazily created injectors that do
 * not replay the app config still resolve it - and a spec that overrides
 * `EngineClient` with a spy sees that spy through the token as well.
 */
export const ENGINE_API = new InjectionToken<EngineApi>('ENGINE_API', {
  providedIn: 'root',
  factory: () => inject<EngineApi>(EngineClient),
});
