import {
  Component,
  OnDestroy,
  OnInit,
  computed,
  effect,
  inject,
  signal,
  viewChild,
} from '@angular/core';
import { CdkScrollable } from '@angular/cdk/scrolling';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import type { ElementRef } from '@angular/core';
import { Subscription } from 'rxjs';

import { EngineClient } from '../../core/engine-client.service';
import { selectableModels } from '../../core/model-list';
import {
  ActiveTask,
  AgentInfo,
  EngineEvent,
  Message,
  PromptBody,
  PromptImage,
  PromptFile,
  SessionMeta,
} from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { OpenSessionsStore } from '../../core/open-sessions.store';
import { SessionActivityStore } from '../../core/session-activity.store';
import { TaskProgressStore } from '../../core/task-progress.store';
import { ToolSafetyStore } from '../../core/tool-safety.store';
import { I18nService } from '../../i18n/i18n.service';
import { ModelSelect } from '../../ui/model-select/model-select';
import { PermissionPopup } from '../../ui/permission-popup/permission-popup';
import { TaskProgressLine } from '../../ui/task-progress-line/task-progress-line';
import { ToastHost } from '../../ui/toast/toast-host';
import { ChatSessionStore } from './chat-session.store';
import { resolveEffectiveModel } from './effective-model';
import { MessageRowComponent } from './parts/message-row';
import { ToolRunRowComponent } from './parts/tool-run-row';

const REFRESH_DEBOUNCE_MS = 300;
/**
 * F6-2: how many of the newest messages the transcript renders initially, and
 * how many more each "Load earlier messages" click reveals. `messages` always
 * holds the full history (index-based SSE patches depend on it); only the
 * rendered slice is windowed.
 */
export const MESSAGE_WINDOW = 60;
const DRAFT_KEY = 'bebok.sessionDrafts';
const MAX_IMAGES = 5;
const MAX_IMAGE_BYTES = 5 * 1024 * 1024;
const MAX_FILES = 10;
const MAX_FILE_BYTES = 20 * 1024 * 1024;
const TEXT_FILE_TYPES = new Set([
  'application/json', 'application/xml', 'application/javascript',
  'application/x-yaml', 'application/yaml', 'application/x-sh',
]);
const TEXT_FILE_EXTENSIONS = /\.(txt|md|markdown|json|jsonc|csv|tsv|log|ya?ml|toml|xml|html?|css|scss|js|jsx|ts|tsx|py|rs|go|java|c|cc|cpp|h|hpp|cs|sh|bash|zsh|ps1|sql|ini|conf|env|gitignore)$/i;
const ACCEPTED_IMAGE_TYPES = new Set([
  'image/png',
  'image/jpeg',
  'image/webp',
  'image/gif',
]);
/** Longest edge kept when downscaling a staged image. */
const IMAGE_MAX_EDGE = 2048;
/** Staged files above this are downscaled/re-encoded before sending (~1 MiB). */
const IMAGE_DOWNSCALE_THRESHOLD = 1 * 1024 * 1024;
/** Decoded-byte target after downscaling (~2 MiB); the engine's 5 MiB is the hard cap. */
const IMAGE_TARGET_BYTES = 2 * 1024 * 1024;
/** Tallest the auto-growing composer gets before it scrolls (F2-10). */
const COMPOSER_MAX_HEIGHT = 200;
let pendingSeq = 0;
let attachmentSeq = 0;

/** Minimal shape of the (non-standard) Web Speech API we rely on. */
interface SpeechRecognitionResultLike {
  readonly length: number;
  [index: number]: { transcript: string };
}
interface SpeechRecognitionEventLike {
  resultIndex: number;
  results: { length: number; [index: number]: SpeechRecognitionResultLike };
}
interface SpeechRecognitionLike {
  lang: string;
  interimResults: boolean;
  continuous: boolean;
  onresult: ((event: SpeechRecognitionEventLike) => void) | null;
  onend: (() => void) | null;
  onerror: (() => void) | null;
  start(): void;
  stop(): void;
}

/** `SpeechRecognition` / `webkitSpeechRecognition`, when the host has it. */
function speechRecognitionCtor(): (new () => SpeechRecognitionLike) | null {
  const scope = globalThis as unknown as Record<string, unknown>;
  const ctor = scope['SpeechRecognition'] ?? scope['webkitSpeechRecognition'];
  return typeof ctor === 'function' ? (ctor as new () => SpeechRecognitionLike) : null;
}

/** A user prompt echoed locally while waiting for the engine to reflect it. */
interface PendingPrompt {
  id: string;
  text: string;
  images?: PromptImage[];
  files?: PromptFile[];
  /** "sending" until accepted by the engine, then "sent". */
  state: 'sending' | 'sent';
  ts: number;
}

/** One queued prompt: text plus image attachments (raw base64). */
export interface QueuedPrompt {
  text: string;
  images: PromptImage[];
  files: PromptFile[];
  /** Agent selected when this message was queued. */
  agent: string;
  /** Explicit model selected when this message was queued, if any. */
  model?: string;
  /** Per-prompt fleet fan-out requested when this message was queued. */
  fleet?: boolean;
}

/** Freeze the composer settings together with the message that will use them. */
export function queuePrompt(
  text: string,
  images: PromptImage[],
  agent: string,
  selectedModel: string,
  fleet = false,
  files: PromptFile[] = [],
): QueuedPrompt {
  const model = selectedModel.trim();
  return {
    text,
    images,
    files,
    agent,
    ...(model ? { model } : {}),
    ...(fleet ? { fleet: true } : {}),
  };
}

/** Build the exact engine request from the settings frozen at queue time. */
export function queuedPromptBody(prompt: QueuedPrompt): PromptBody {
  return {
    message: prompt.text,
    agent: prompt.agent,
    ...(prompt.model ? { model: prompt.model } : {}),
    ...(prompt.images.length ? { images: prompt.images } : {}),
    ...(prompt.files.length ? { files: prompt.files } : {}),
    ...(prompt.fleet ? { fleet: true } : {}),
  };
}

/** An image staged in the composer (dataUrl for preview, base64 for sending). */
export interface StagedAttachment {
  id: string;
  media_type: string;
  dataUrl: string;
  base64: string;
  name: string;
  size: number;
  /** Present for text-file attachments; images use dataUrl/base64 instead. */
  fileBase64?: string;
}

/** Per-session drafts persisted to localStorage (survive tab switches). */
function loadDrafts(): Record<string, string> {
  try {
    const raw = localStorage.getItem(DRAFT_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : {};
    return parsed && typeof parsed === 'object' ? (parsed as Record<string, string>) : {};
  } catch {
    return {};
  }
}

function persistDrafts(drafts: Record<string, string>): void {
  try {
    localStorage.setItem(DRAFT_KEY, JSON.stringify(drafts));
  } catch {
    /* storage full/unavailable - drafts stay in memory only */
  }
}

@Component({
  selector: 'app-chat',
  imports: [
    CdkScrollable,
    FormsModule,
    RouterLink,
    ModelSelect,
    PermissionPopup,
    MessageRowComponent,
    ToolRunRowComponent,
    TaskProgressLine,
    ToastHost,
  ],
  templateUrl: './chat.html',
  styleUrl: './chat.css',
})
export class ChatView implements OnInit, OnDestroy {
  private readonly engine = inject(EngineClient);
  private readonly toolSafety = inject(ToolSafetyStore);
  /** WP-DELEGATION: live progress of the children listed in the active-tasks block. */
  readonly liveTasks = inject(TaskProgressStore);
  private readonly events = inject(EventsStore);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  private readonly i18n = inject(I18nService);
  private readonly tabs = inject(OpenSessionsStore);
  private readonly activity = inject(SessionActivityStore);
  private readonly sessionStore = inject(ChatSessionStore);

  readonly t = this.i18n.t.bind(this.i18n);

  /**
   * Active session id, driven by the route (`/chat/:sessionID`). A signal (not
   * a snapshot): Angular reuses this component instance when navigating
   * between tabs (`/chat/A` -> `/chat/B`), so the id must be re-read on every
   * `paramMap` emission and the whole view reloaded (see `switchSession`).
   */
  readonly sessionID = signal<string>('');

  readonly meta = signal<SessionMeta | null>(null);
  private readonly directory = signal<string | null>(null);
  readonly messages = signal<Message[]>([]);
  readonly loading = signal(true);
  readonly error = signal<string | null>(null);
  readonly running = signal(false);
  readonly sending = signal(false);
  /** An abort request is waiting for the engine to release the turn slot. */
  readonly aborting = signal(false);
  /** Force-send is stopping the current turn and dispatching the queue. */
  readonly forceSending = signal(false);
  readonly draft = signal('');
  readonly follow = signal(true);

  /** M6: agent + model switchers (change mid-chat). */
  readonly agents = signal<AgentInfo[]>([]);
  readonly selectedAgent = signal('code');
  readonly availableModels = signal<string[]>([]);
  readonly selectedModel = signal('');
  /** F9-9: `config.model` (provider-qualified when the engine gives one) as the last fallback. */
  readonly configDefaultModel = signal<string | null>(null);
  /**
   * F9-9: the model a prompt will actually run on - the explicit selection,
   * else the engine's `effective_model` (never "(default)"). Empty when
   * nothing is known yet, which hides the toolbar badge.
   */
  readonly effectiveModel = computed(() =>
    resolveEffectiveModel(
      this.selectedModel(),
      this.meta(),
      this.agents(),
      this.configDefaultModel(),
    ),
  );

  /** Reasoning/thinking effort for this directory, set in the chat header. */
  readonly thinking = signal('off');

  /** "Run as fleet" per-prompt toggle (fleet-first when available; sticky). */
  readonly runAsFleet = signal(false);
  /** True when the directory config enables the fleet with at least one member. */
  readonly fleetAvailable = signal(false);

  /** M6: filter the transcript by model (driven from the drawer's Session panel). */
  readonly filterModel = this.sessionStore.filterModel;

  /** F6-3: context meter in the toolbar (`42k / 200k · 21%`), from the store. */
  readonly contextLabel = this.sessionStore.contextLabel;
  readonly contextLevel = this.sessionStore.contextLevel;
  readonly contextFill = computed(() =>
    Math.min(100, Math.max(0, this.sessionStore.contextPercent() ?? 0)),
  );

  /** F6-4: a compaction request is in flight (manual or automatic). */
  readonly compacting = signal(false);
  /** "Compact now" is offered once the engine has enough to summarize. */
  readonly canCompact = computed(
    () =>
      this.sessionStore.canCompact() &&
      !this.running() &&
      !this.sending() &&
      !this.loading() &&
      !this.compacting(),
  );
  /** Sessions already auto-compacted once (never loop on a stubborn gauge). */
  private readonly autoCompacted = new Set<string>();

  /** M6: prompt queue - messages waiting to be sent while a turn runs. */
  readonly queue = signal<QueuedPrompt[]>([]);

  /** Images staged in the composer (previews; sent as raw base64). */
  readonly attachments = signal<StagedAttachment[]>([]);
  /** Last attachment validation error (shown under the composer). */
  readonly attachError = signal<string | null>(null);
  /** Staged attachment count (n/MAX_IMAGES shown above the composer). */
  readonly attachCount = computed(() => this.attachments().length);

  /** Optimistic echo of prompts sent this session (shown immediately on the
   *  board, replaced once the engine reflects the real message). */
  readonly pending = signal<PendingPrompt[]>([]);

  /** Active sub-tasks spawned by the orchestrator (from task.started events). */
  readonly activeTasks = signal<ActiveTask[]>([]);

  /**
   * Durable map of task name/ID → child session ID, fed by task.started
   * events. NOT cleared on task.ended/aborted/turn end so that completed
   * tool-part links remain resolvable.
   */
  readonly taskLinks = signal<Map<string, string>>(new Map());

  /** Resolve a task name or ID to its child session ID (if known). */
  resolveTaskLink(nameOrId: string): string | null {
    return this.taskLinks().get(nameOrId) ?? null;
  }

  readonly filteredMessages = computed<Message[]>(() => {
    const filter = this.filterModel();
    if (!filter) {
      return this.messages();
    }
    return this.messages().filter((m) => m.meta?.model === filter);
  });

  /**
   * F6-2: number of newest (filtered) messages actually rendered. Starts at
   * `MESSAGE_WINDOW`, grows by `loadEarlier()`, resets on session switch.
   */
  readonly visibleCount = signal(MESSAGE_WINDOW);

  /** The rendered slice: the last `visibleCount()` of `filteredMessages()`. */
  readonly windowedMessages = computed<Message[]>(() =>
    windowMessages(this.filteredMessages(), this.visibleCount()),
  );

  /** Older messages kept out of the DOM (drives the "Load earlier" control). */
  readonly hiddenCount = computed(() =>
    Math.max(0, this.filteredMessages().length - this.visibleCount()),
  );

  /**
   * F6-1c: `windowedMessages()` with runs of >= 2 consecutive tool-only
   * assistant turns folded into one `RunRow` (rendered as `<app-tool-run-row>`
   * instead of one `<app-message-row>` per turn). Computed from the already
   * windowed slice, so a run can span - and be split by - the window edge;
   * that is expected, not a bug (see `groupMessageRuns`).
   */
  readonly transcriptRows = computed<TranscriptRow[]>(() =>
    groupMessageRuns(this.windowedMessages()),
  );

  /** Show the "jump to last user message" button when user has scrolled up
   *  and there is at least one user message in the conversation. */
  readonly showJump = computed<boolean>(() => {
    if (this.follow()) {
      return false;
    }
    return this.filteredMessages().some((m) => m.role === 'user');
  });

  readonly scrollArea = viewChild<ElementRef<HTMLElement>>('scroll');
  /** CDK handle on the same element (`cdkScrollable`) for offset measuring. */
  private readonly scrollable = viewChild(CdkScrollable);
  /** Inline permission prompt (F2-8) - blocks sending while unresolved. */
  readonly permission = viewChild(PermissionPopup);

  /** True while a permission decision is outstanding (F2-8, bug B15). */
  readonly permissionBlocked = computed(
    () => (this.permission()?.asks().length ?? 0) > 0,
  );

  /** Per-session drafts (sessionID -> text), persisted to localStorage. */
  private readonly drafts: Record<string, string> = loadDrafts();
  /** Per-session queued prompts + optimistic echoes (survive tab switches). */
  private readonly queuedBySession = new Map<string, QueuedPrompt[]>();
  private readonly pendingBySession = new Map<string, PendingPrompt[]>();
  /** Attachments restored only when retrying a prompt in a newly forked session. */
  private readonly retryAttachmentsBySession = new Map<string, StagedAttachment[]>();
  /** Local running flag per session (fallback when activity store missed it). */
  private readonly runningBySession = new Map<string, boolean>();
  private readonly unsubscribeEvents: () => void;
  private refreshTimer: number | undefined;
  private lastReconnectVersion = 0;
  private routeSub: Subscription | null = null;
  /** Unlisten for the Tauri window `drag-drop` event (desktop shell only). */
  private tauriDropUnlisten: (() => void) | null = null;
  /** Monotonic load generation: stale fetches from a previous tab never win. */
  private loadSeq = 0;

  constructor() {
    this.unsubscribeEvents = this.events.onEvent((ev) => this.handleEvent(ev));

    // Publish the visible session to the shared store so the right drawer's
    // Session panel derives tokens/cost/files from the same data (F2-12).
    effect(() => this.sessionStore.meta.set(this.meta()));
    effect(() => this.sessionStore.messages.set(this.messages()));
    effect(() => this.sessionStore.running.set(this.running()));

    // Full transcript sync whenever the SSE stream (re)connects: events that
    // fell into the reconnect gap are recovered from the engine, not guessed.
    effect(() => {
      const version = this.events.reconnectVersion();
      if (version !== this.lastReconnectVersion) {
        this.lastReconnectVersion = version;
        void this.refreshFull();
      }
    });

    // Follow the stream unless the user scrolled up. `loading` and the
    // rendered window are read too: the rows only enter the DOM once the
    // loading placeholder goes away, and the initial scroll-to-bottom must
    // happen after that, not while the placeholder is still showing (F6-2).
    effect(() => {
      this.messages();
      this.windowedMessages();
      this.running();
      this.loading();
      const el = this.scrollArea();
      if (!el || !this.follow()) {
        return;
      }
      requestAnimationFrame(() => {
        const container = el.nativeElement;
        container.scrollTop = container.scrollHeight;
      });
    });

    // Drain the prompt queue once the agent becomes idle. Skipped while a
    // session is (re)loading so a restored queue is sent with that session's
    // own agent/model (loaded in loadAll), not the previous tab's.
    effect(() => {
      const running = this.running();
      if (this.loading()) {
        return;
      }
      if (!running && this.queue().length > 0 && !this.aborting() && !this.forceSending()) {
        void this.drainQueue();
      }
    });

    // Keep the per-session running cache live: the activity store only sees
    // transitions that arrive as events, while local POST/abort flows set the
    // signal directly (e.g. 409 race, in-flight prompt before any event).
    effect(() => {
      const id = this.sessionID();
      if (!id) {
        return;
      }
      this.runningBySession.set(id, this.running());
    });

    // Persist the visible tab's queue/pending echoes as they change so a
    // later switchSession() restores them (switch saves once more, cheap).
    effect(() => {
      const id = this.sessionID();
      if (!id) {
        return;
      }
      this.queuedBySession.set(id, this.queue());
    });
    effect(() => {
      const id = this.sessionID();
      if (!id) {
        return;
      }
      this.pendingBySession.set(id, this.pending());
    });
  }

  async ngOnInit(): Promise<void> {
    if (!this.engine.connected()) {
      try {
        await this.engine.connect();
      } catch (err) {
        this.error.set(this.describe(err));
        this.loading.set(false);
        return;
      }
    }
    this.events.start();
    // React to tab switches: the router reuses this component for
    // `/chat/A` -> `/chat/B`, so `ngOnInit` runs once and every later
    // navigation arrives here as a new `paramMap` emission.
    this.routeSub = this.route.paramMap.subscribe((params) => {
      void this.switchSession(params.get('sessionID') ?? '');
    });
    // Tauri desktop: the webview does not populate HTML `dataTransfer.files`
    // for OS-level drops, so listen to the window's native `drag-drop` event
    // (file paths) and read the files via the fs plugin. One subscription per
    // component lifetime; the handler no-ops when the drop lands outside the
    // composer or no session is open.
    void this.listenTauriDrop();
  }

  /**
   * Subscribe to the Tauri window `drag-drop` event (desktop shell only).
   * In the Tauri webview an OS file drop never reaches the DOM `drop`
   * handler with populated `dataTransfer.files`, so dropped files would be
   * silently ignored without this native listener.
   */
  private async listenTauriDrop(): Promise<void> {
    if (!this.engine.isTauri()) {
      return;
    }
    try {
      const { getCurrentWindow } = await import('@tauri-apps/api/window');
      this.tauriDropUnlisten = await getCurrentWindow().onDragDropEvent((event) => {
        if (event.payload.type !== 'drop' || event.payload.paths.length === 0) {
          return;
        }
        if (!this.sessionID()) {
          return;
        }
        void this.addTauriPaths(event.payload.paths);
      });
    } catch {
      // Tauri API unavailable (browser mode, tests): HTML handlers cover it.
      this.tauriDropUnlisten = null;
    }
  }

  /** Read OS paths from a Tauri `drag-drop` event into staged attachments. */
  private async addTauriPaths(paths: string[]): Promise<void> {
    try {
      const { readFile, stat } = await import('@tauri-apps/plugin-fs');
      const files: File[] = [];
      for (const path of paths) {
        try {
          const info = await stat(path);
          if (info.isDirectory) {
            continue;
          }
          const bytes = await readFile(path);
          const name = path.split(/[/\\]/).pop() ?? path;
          files.push(new File([bytes as unknown as BlobPart], name));
        } catch {
          // Unreadable entry: skip, keep the readable ones.
        }
      }
      if (files.length > 0) {
        await this.addFiles(files);
      }
    } catch {
      this.attachError.set(this.t('chat.dropReadFailed'));
    }
  }

  ngOnDestroy(): void {
    this.sessionStore.clear();
    this.unsubscribeEvents();
    this.routeSub?.unsubscribe();
    this.routeSub = null;
    this.tauriDropUnlisten?.();
    this.tauriDropUnlisten = null;
    if (this.refreshTimer !== undefined) {
      window.clearTimeout(this.refreshTimer);
    }
  }

  /**
   * Switch the visible session (tab click, back/forward, direct link).
   * Queue/pending/running are per-session: the previous tab's state is saved
   * and the next tab's state is restored, so queued prompts are never lost.
   * The previous session keeps running on the engine untouched.
   */
  private async switchSession(nextID: string): Promise<void> {
    if (!nextID) {
      void this.router.navigate(['/']);
      return;
    }
    if (nextID === this.sessionID() && this.meta() !== null) {
      return;
    }
    const seq = ++this.loadSeq;
    if (this.refreshTimer !== undefined) {
      window.clearTimeout(this.refreshTimer);
      this.refreshTimer = undefined;
    }
    // Persist the outgoing tab's queue/pending/running before resetting.
    const prevID = this.sessionID();
    if (prevID) {
      this.queuedBySession.set(prevID, this.queue());
      this.pendingBySession.set(prevID, this.pending());
      this.runningBySession.set(prevID, this.running());
    }
    this.sessionID.set(nextID);
    this.meta.set(null);
    this.messages.set([]);
    // Restore this tab's queued prompts + optimistic echoes (not cleared).
    this.pending.set(this.pendingBySession.get(nextID) ?? []);
    this.queue.set(this.queuedBySession.get(nextID) ?? []);
    this.attachments.set(this.retryAttachmentsBySession.get(nextID) ?? []);
    this.retryAttachmentsBySession.delete(nextID);
    this.attachError.set(null);
    this.activeTasks.set([]);
    this.error.set(null);
    // Restore running state: the activity store (SSE `session.updated`) is
    // authoritative; fall back to the locally cached flag for turns started
    // here that haven't produced an event yet.
    this.running.set(
      this.activity.isRunning(nextID) || this.runningBySession.get(nextID) === true,
    );
    this.sending.set(false);
    this.filterModel.set(null);
    this.follow.set(true);
    this.visibleCount.set(MESSAGE_WINDOW);
    await this.loadAll(nextID, seq);
    // Stale navigation already moved on: leave the new tab alone.
    if (seq !== this.loadSeq || nextID !== this.sessionID()) {
      return;
    }
    // Re-sync running (events may have arrived during load) and drain any
    // restored queue now that agent/model for this session are loaded.
    this.running.set(
      this.activity.isRunning(nextID) || this.running(),
    );
    if (!this.running() && this.queue().length > 0) {
      void this.drainQueue();
    }
  }

  private async loadAll(sessionID: string, seq: number): Promise<void> {
    this.loading.set(true);
    this.error.set(null);
    try {
      const [meta, messages] = await Promise.all([
        this.engine.sessionMeta(sessionID),
        this.engine.messages(sessionID),
      ]);
      // A faster tab switch already moved on: drop this stale response.
      if (seq !== this.loadSeq || sessionID !== this.sessionID()) {
        return;
      }
      this.meta.set(meta);
      this.messages.set(messages);
      // SSE events are ephemeral. The metadata snapshot restores Abort after
      // a reload or tab switch while a tool call is still in progress.
      this.running.set(meta.running === true || this.activity.isRunning(sessionID));
      this.directory.set(meta.directory);
      // F7-7: the tool safety map colours historical tool calls' dots.
      void this.toolSafety.ensure(meta.directory);
      this.selectedAgent.set(meta.agent);
      this.selectedModel.set(meta.model ?? '');
      // Restore this session's draft (per-session input, survives tab switches).
      this.setComposerText(this.drafts[sessionID] ?? '');
      // `autofocus` only fires on a full page load; after SPA navigation
      // (start -> chat, tab switch) focus the composer explicitly so
      // keyboard shortcuts (incl. native Ctrl/Cmd+Z) work right away.
      requestAnimationFrame(() => {
        if (sessionID === this.sessionID()) {
          this.composerInput()?.nativeElement.focus({ preventScroll: true });
        }
      });
      // Nav tab: open (or refresh the title of) this session's tab.
      if (!this.tabs.get(meta.id)) {
        this.tabs.open(meta.id, meta.title ?? null, meta.alias ?? null);
      } else {
        this.tabs.setTitle(meta.id, meta.title ?? null);
        this.tabs.setAlias(meta.id, meta.alias ?? null);
      }
      // Load the agent presets + provider model list for the switchers.
      try {
        const [agents, cfg] = await Promise.all([
          this.engine.listAgents(meta.directory),
          this.engine.getConfig(meta.directory),
        ]);
        if (seq !== this.loadSeq || sessionID !== this.sessionID()) {
          return;
        }
        this.agents.set(agents);
        this.thinking.set(cfg.config.thinking ?? 'off');
        const fleet = cfg.config.fleet;
        const available = !!fleet?.enabled && (fleet?.members?.length ?? 0) > 0;
        const wasAvailable = this.fleetAvailable();
        this.fleetAvailable.set(available);
        if (!available) {
          this.runAsFleet.set(false);
        } else if (!wasAvailable) {
          // Fleet just became available: prefer fleet over solo.
          this.runAsFleet.set(true);
        }
        const defaultModel = (cfg.config.model ?? '').trim();
        this.configDefaultModel.set(
          !defaultModel
            ? null
            : defaultModel.includes('/') || !cfg.config.provider
              ? defaultModel
              : `${cfg.config.provider}/${defaultModel}`,
        );
        const models = selectableModels(cfg.providers);
        this.availableModels.set(models);
      } catch {
        /* switcher lists are non-critical */
      }
    } catch (err) {
      if (seq !== this.loadSeq || sessionID !== this.sessionID()) {
        return;
      }
      const message = this.describe(err);
      if (message.includes('404')) {
        // Session is gone (engine restart wiped it / bad link): drop the tab.
        this.tabs.close(sessionID);
        void this.router.navigate(['/']);
        return;
      }
      this.error.set(message);
    } finally {
      if (seq === this.loadSeq && sessionID === this.sessionID()) {
        this.loading.set(false);
      }
    }
  }

  /** Drop pending echoes whose text is now reflected by a real message. */
  private reconcilePending(): void {
    const texts = new Set(
      this.messages()
        .filter((m) => m.role === 'user')
        .map((m) => this.userText(m)),
    );
    if (texts.size === 0) {
      return;
    }
    this.pending.update((p) =>
      p.filter((x) => {
        if (!texts.has(x.text)) {
          return true;
        }
        // Keep echoes that carried images until an image part is reflected.
        if (x.images?.length) {
          return !this.messages().some(
            (m) =>
              m.role === 'user' &&
              this.userText(m) === x.text &&
              m.parts.some((part) => part.type === 'image'),
          );
        }
        return false;
      }),
    );
  }

  private userText(m: Message): string {
    for (const part of m.parts) {
      if (part.type === 'text') {
        return part.text;
      }
    }
    return '';
  }

  private async refreshFull(): Promise<void> {
    if (this.loading()) {
      return;
    }
    const sessionID = this.sessionID();
    if (!sessionID) {
      return;
    }
    try {
      const messages = await this.engine.messages(sessionID);
      // Tab switched mid-fetch: never paint session A into session B.
      if (sessionID !== this.sessionID()) {
        return;
      }
      this.messages.set(messages);
      this.reconcilePending();
    } catch (err) {
      if (sessionID !== this.sessionID()) {
        return;
      }
      this.error.set(this.describe(err));
    }
  }

  /** Re-read session metadata (usage, context gauge) once a turn settles. */
  private async refreshMeta(): Promise<void> {
    const sessionID = this.sessionID();
    if (!sessionID || this.loading()) {
      return;
    }
    try {
      const meta = await this.engine.sessionMeta(sessionID);
      if (sessionID === this.sessionID()) {
        this.meta.set(meta);
      }
    } catch {
      /* metadata refresh is best-effort; the next load re-reads it */
    }
  }

  private scheduleRefresh(): void {
    if (this.refreshTimer !== undefined) {
      return;
    }
    this.refreshTimer = window.setTimeout(() => {
      this.refreshTimer = undefined;
      void this.refreshFull();
    }, REFRESH_DEBOUNCE_MS);
  }

  private handleEvent(event: EngineEvent): void {
    if (event.sessionID !== this.sessionID()) {
      return;
    }
    switch (event.type) {
      case 'message.part.updated': {
        this.applyPartEvent(event);
        break;
      }
      case 'message.updated': {
        this.scheduleRefresh();
        break;
      }
      case 'session.updated': {
        // Bare session updates are metadata changes, not turn transitions.
        // Only an explicit `running` property may change the live state.
        const running = event.properties?.['running'];
        if (typeof running !== 'boolean') {
          break;
        }
        this.running.set(running);
        if (!running) {
          this.aborting.set(false);
          this.forceSending.set(false);
          this.scheduleRefresh();
          void this.refreshMeta();
          this.activeTasks.set([]);
          const err = event.properties?.['error'];
          if (typeof err === 'string' && err) {
            this.error.set(err);
          }
        }
        break;
      }
      case 'task.started': {
        const props = event.properties;
        if (props) {
          const taskID = String(props['taskID'] ?? props['task_id'] ?? '');
          const description = String(props['description'] ?? '');
          const childSessionID = String(props['childSessionID'] ?? props['child_session_id'] ?? '');
          const name = String(props['name'] ?? '');
          const agent = String(props['agent'] ?? '');
          const task: ActiveTask = { taskID, description, childSessionID, name, agent };
          if (taskID) {
            this.activeTasks.update((tasks) => [...tasks, task]);
          }
          // Feed the durable link map (name → childSessionID, taskID → childSessionID).
          if (childSessionID) {
            this.taskLinks.update((m) => {
              const next = new Map(m);
              if (name) {
                next.set(name, childSessionID);
              }
              if (taskID) {
                next.set(taskID, childSessionID);
              }
              return next;
            });
          }
        }
        break;
      }
      case 'task.ended': {
        const props = event.properties;
        if (props) {
          const taskID = String(props['taskID'] ?? '');
          if (taskID) {
            this.activeTasks.update((tasks) => tasks.filter((t) => t.taskID !== taskID));
          }
        }
        break;
      }
      case 'task.aborted': {
        const props = event.properties;
        if (props) {
          const taskID = String(props['taskID'] ?? '');
          if (taskID) {
            this.activeTasks.update((tasks) => tasks.filter((t) => t.taskID !== taskID));
          }
        }
        break;
      }
      default:
        break;
    }
  }

  /** Fast path: the event carries the full message snapshot - no refetch. */
  private applyPartEvent(event: EngineEvent): void {
    const props = event.properties;
    if (!props) {
      return;
    }
    const index = props['messageIndex'];
    const rawMessage = properties_message(props);
    if (typeof index !== 'number' || !rawMessage) {
      return;
    }
    this.messages.update((list) => {
      if (index < 0 || index >= list.length) {
        return list;
      }
      const next = [...list];
      next[index] = rawMessage;
      return next;
    });
  }

  onScroll(): void {
    const el = this.scrollArea()?.nativeElement;
    if (!el) {
      return;
    }
    const distance =
      this.scrollable()?.measureScrollOffset('bottom') ??
      el.scrollHeight - el.scrollTop - el.clientHeight;
    this.follow.set(distance < 90);
  }

  /**
   * F6-2: reveal the next `MESSAGE_WINDOW` older messages. The viewport is
   * kept anchored on the row the user was looking at: the newly rendered rows
   * grow the content above it, so `scrollTop` is advanced by that growth.
   */
  loadEarlier(): void {
    if (this.hiddenCount() === 0) {
      return;
    }
    const el = this.scrollArea()?.nativeElement;
    const heightBefore = el?.scrollHeight ?? 0;
    const topBefore = el?.scrollTop ?? 0;
    this.visibleCount.update((count) => count + MESSAGE_WINDOW);
    if (!el) {
      return;
    }
    requestAnimationFrame(() => {
      el.scrollTop = topBefore + (el.scrollHeight - heightBefore);
    });
  }

  /** Scroll to the last user message among the rendered (windowed) rows. */
  scrollToLastUser(): void {
    const msgs = this.windowedMessages();
    for (let i = msgs.length - 1; i >= 0; i--) {
      if (msgs[i].role === 'user') {
        const el = this.scrollArea()?.nativeElement;
        if (!el) {
          return;
        }
        const target = el.querySelector<HTMLElement>(
          '#' + CSS.escape('msg-' + msgs[i].id),
        );
        if (target) {
          target.scrollIntoView({ behavior: 'smooth', block: 'start' });
          this.follow.set(false);
        }
        return;
      }
    }
  }

  async sendPrompt(): Promise<void> {
    const text = this.draft().trim();
    const staged = this.attachments();
    if ((!text && staged.length === 0) || this.loading()) {
      return;
    }
    // A pending permission prompt blocks turn progress (F2-8).
    if (this.permissionBlocked()) {
      return;
    }
    this.setComposerText('');
    const images: PromptImage[] = staged.filter((a) => !a.fileBase64).map((a) => ({
      media_type: a.media_type,
      data: a.base64,
      ...(a.name ? { name: a.name } : {}),
    }));
    const files: PromptFile[] = staged.filter((a) => a.fileBase64).map((a) => ({
      media_type: a.media_type,
      data: a.fileBase64!,
      name: a.name,
    }));
    this.attachments.set([]);
    this.attachError.set(null);
    // Per-prompt fleet fan-out: frozen with the message. The toggle stays
    // sticky (fleet-first when available); only force solo when unavailable.
    const fleet = this.fleetAvailable() && this.runAsFleet();
    // Always enqueue; sends immediately when idle, otherwise waits for the turn.
    // Preserve the dispatch settings with the message. A queued prompt may
    // wait for a running turn, and reading these controls in `drainQueue()`
    // would otherwise send it using whichever model happens to be selected
    // later rather than the model the user chose before pressing Send.
    this.queue.update((q) => [
      ...q,
      queuePrompt(text, images, this.selectedAgent(), this.selectedModel(), fleet, files),
    ]);
    this.pending.update((p) => [
      ...p,
      {
        id: `pending-${++pendingSeq}`,
        text,
        ...(images.length ? { images } : {}),
        ...(files.length ? { files } : {}),
        state: 'sending',
        ts: Date.now(),
      },
    ]);
    await this.drainQueue();
  }

  private markPending(text: string, state: PendingPrompt['state']): void {
    this.pending.update((p) => {
      const copy = [...p];
      const match = copy.find((x) => x.text === text && x.state === 'sending');
      if (match) {
        match.state = state;
      }
      return copy;
    });
  }

  /** Send the first queued message when the agent is idle. */
  private async drainQueue(): Promise<void> {
    if (this.running() || this.sending() || this.compacting() || this.queue().length === 0) {
      return;
    }
    const sessionID = this.sessionID();
    if (!sessionID) {
      return;
    }
    // F6-4: the meter crossed the auto-compact threshold - compact first and
    // carry the queue over to the fork, which drains it once loaded.
    if (
      this.sessionStore.needsAutoCompact() &&
      this.sessionStore.canCompact() &&
      !this.autoCompacted.has(sessionID)
    ) {
      this.autoCompacted.add(sessionID);
      const forked = await this.compactInto(sessionID);
      if (forked) {
        this.queuedBySession.set(forked, this.queue());
        this.pendingBySession.set(forked, this.pending());
        this.queue.set([]);
        this.pending.set([]);
        await this.router.navigate(['/chat', forked]);
        return;
      }
      if (sessionID !== this.sessionID()) {
        return;
      }
      // Compaction failed: fall through and send anyway (error is shown).
    }
    const head = this.queue()[0];
    this.sending.set(true);
    this.error.set(null);
    try {
      await this.engine.prompt(
        sessionID,
        queuedPromptBody(head),
      );
      // Tab switched while the POST was in flight: leave the new tab alone.
      if (sessionID !== this.sessionID()) {
        return;
      }
      this.markPending(head.text, 'sent');
      this.queue.update((q) => q.slice(1));
      this.running.set(true);
    } catch (err) {
      if (sessionID !== this.sessionID()) {
        return;
      }
      const message = this.describe(err);
      if (message.includes('409')) {
        // Already running (rare race): keep it queued, the turn will drain it.
        this.running.set(true);
      } else {
        this.error.set(message);
        this.queue.update((q) => q.slice(1));
      }
    } finally {
      if (sessionID === this.sessionID()) {
        this.sending.set(false);
      }
    }
  }

  /** Force-send: abort the running turn and send the queued message now. */
  async forceSend(): Promise<void> {
    if (this.queue().length === 0 || this.forceSending() || this.aborting()) {
      return;
    }
    const sessionID = this.sessionID();
    this.forceSending.set(true);
    this.error.set(null);
    try {
      const result = await this.engine.abort(sessionID);
      if (sessionID !== this.sessionID()) {
        return;
      }
      this.running.set(result.stopped ? false : this.running());
      // The engine normally waits for the old slot before returning. If a
      // tool overran its grace period, retry through the short 409 window.
      for (let attempt = 0; result.timedOut && attempt < 12; attempt++) {
        await new Promise((resolve) => setTimeout(resolve, 250));
        if (sessionID !== this.sessionID()) {
          return;
        }
        await this.drainQueue();
        if (!this.running() && !this.sending()) {
          return;
        }
      }
      this.running.set(false);
      await this.drainQueue();
    } catch (err) {
      if (sessionID === this.sessionID()) {
        this.error.set(this.describe(err));
      }
    } finally {
      if (sessionID === this.sessionID()) {
        this.forceSending.set(false);
      }
    }
  }

  /** Remove a single message from the queue by index. */
  removeFromQueue(index: number): void {
    const removed = this.queue()[index];
    this.queue.update((q) => q.filter((_, i) => i !== index));
    // Drop the matching optimistic echo as well.
    if (removed) {
      this.pending.update((p) => {
        const at = p.findIndex(
          (x) => x.text === removed.text && x.state === 'sending',
        );
        if (at < 0) {
          return p;
        }
        return p.filter((_, i) => i !== at);
      });
    }
  }

  /** Remove all messages from the queue. */
  clearQueue(): void {
    this.queue.set([]);
    this.pending.update((p) => p.filter((x) => x.state !== 'sending'));
  }

  /**
   * Retry a user prompt from an independent branch. The original transcript
   * remains intact, while the branch opens with the prompt's text and images
   * restored in the composer for editing.
   */
  async rollbackTo(messageID: string): Promise<void> {
    const index = this.messages().findIndex((m) => m.id === messageID);
    if (index < 0 || this.sending() || this.loading()) {
      return;
    }
    const sessionID = this.sessionID();
    const source = this.messages()[index];
    const directory = this.directory();
    if (!source || source.role !== 'user' || !directory) {
      return;
    }
    this.error.set(null);
    try {
      // The fork endpoint is inclusive. A first prompt has no prior message,
      // so start an equivalent empty session instead.
      const created = index === 0
        ? await this.engine.createSession(
            directory,
            this.meta()?.agent ?? this.selectedAgent(),
            this.meta()?.model ?? undefined,
          )
        : await this.engine.forkSession(directory, sessionID, index - 1);
      if (sessionID !== this.sessionID()) {
        return;
      }
      const draft = retryDraft(source);
      this.drafts[created.sessionID] = draft.text;
      persistDrafts(this.drafts);
      if (draft.attachments.length > 0) {
        this.retryAttachmentsBySession.set(created.sessionID, draft.attachments);
      }
      await this.router.navigate(['/chat', created.sessionID]);
    } catch (err) {
      if (sessionID !== this.sessionID()) {
        return;
      }
      this.error.set(this.describe(err));
    }
  }

  readonly composerInput = viewChild<ElementRef<HTMLTextAreaElement>>('composerInput');

  /** Save the draft for this session (called on every input change). */
  saveDraft(text: string): void {
    const sessionID = this.sessionID();
    if (!sessionID) {
      return;
    }
    this.drafts[sessionID] = text;
    persistDrafts(this.drafts);
  }

  /**
   * Uncontrolled composer input: the textarea owns its value so the browser
   * keeps its native undo stack (Ctrl/Cmd+Z). The signal only mirrors the
   * text for send/queue logic — it never writes back into the DOM, so typing
   * and undo are never clobbered by a re-render.
   */
  onComposerInput(event: Event, el: HTMLTextAreaElement): void {
    const value = (event.target as HTMLTextAreaElement | null)?.value ?? el.value;
    this.draft.set(value);
    this.saveDraft(value);
    this.autoGrow(el);
  }

  /** Write a value into the composer without breaking the native undo stack. */
  private setComposerText(text: string): void {
    this.draft.set(text);
    this.saveDraft(text);
    const el = this.composerInput()?.nativeElement;
    if (el && el.value !== text) {
      el.value = text;
      this.autoGrow(el);
    }
  }

  /** Persist a thinking change for the session's directory; applies on save. */
  onThinkingChange(value: string): void {
    this.thinking.set(value);
    const dir = this.directory();
    if (!dir) {
      return;
    }
    void this.engine.putConfig(dir, { thinking: value }).catch(() => {
      /* non-critical: next full reload reads the engine value */
    });
  }

  /**
   * Agent switched in the chat header. Remember the new agent and clear the
   * model override so the engine resolves the default model for this agent
   * type (config.models.<agent> -> preset -> config) on the next prompt.
   * The agent's display model must NOT be copied into `selectedModel`: that
   * signal is sent as an explicit override, which would freeze the choice.
   */
  onAgentChange(value: string): void {
    this.selectedAgent.set(value);
    this.selectedModel.set('');
  }

  /**
   * "Compact now" (toolbar + palette, F6-4): summarize the older messages into
   * a fresh forked session and open it. Compaction is fork-based, so the
   * caller navigates to the new id rather than expecting an in-place change.
   */
  async compactNow(): Promise<void> {
    if (!this.canCompact()) {
      return;
    }
    const sessionID = this.sessionID();
    const forked = await this.compactInto(sessionID);
    if (forked && sessionID === this.sessionID()) {
      await this.router.navigate(['/chat', forked]);
    }
  }

  /** Run one compaction of `sessionID`; the forked session id, or null on error. */
  private async compactInto(sessionID: string): Promise<string | null> {
    this.compacting.set(true);
    this.error.set(null);
    try {
      const result = await this.engine.compactSession(
        sessionID,
        this.sessionStore.compactBudget(),
      );
      return result.sessionID;
    } catch (err) {
      if (sessionID === this.sessionID()) {
        this.error.set(this.describe(err));
      }
      return null;
    } finally {
      this.compacting.set(false);
    }
  }

  async abortTurn(): Promise<void> {
    if (!this.running() || this.aborting() || this.forceSending()) {
      return;
    }
    const sessionID = this.sessionID();
    this.aborting.set(true);
    this.error.set(null);
    try {
      const result = await this.engine.abort(sessionID);
      if (sessionID === this.sessionID()) {
        this.running.set(result.stopped ? false : this.running());
      }
    } catch (err) {
      if (sessionID === this.sessionID()) {
        this.error.set(this.describe(err));
      }
    } finally {
      if (sessionID === this.sessionID()) {
        this.aborting.set(false);
      }
    }
  }

  /** Abort a specific orchestrator sub-task (cancels the child + parent turn). */
  async abortTask(taskID: string): Promise<void> {
    const sessionID = this.sessionID();
    this.error.set(null);
    try {
      await this.engine.abortTask(sessionID, taskID);
    } catch (err) {
      this.error.set(this.describe(err));
    }
  }

  onComposerKeydown(event: KeyboardEvent): void {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      void this.sendPrompt();
    }
  }

  // ---------------------------------------------------------------------------
  // Composer (F2-10): auto-grow textarea + optional dictation.
  // ---------------------------------------------------------------------------

  /** Grow the textarea with its content, up to `COMPOSER_MAX_HEIGHT`. */
  autoGrow(el: HTMLTextAreaElement): void {
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, COMPOSER_MAX_HEIGHT)}px`;
  }

  /** Speech recognition is a browser extra: absent in most webviews. */
  readonly micSupported = signal(speechRecognitionCtor() !== null);
  readonly dictating = signal(false);
  private recognition: SpeechRecognitionLike | null = null;

  /** Toggle dictation; recognized text is appended to the current draft. */
  toggleDictation(): void {
    if (this.dictating()) {
      this.recognition?.stop();
      this.dictating.set(false);
      return;
    }
    const Ctor = speechRecognitionCtor();
    if (!Ctor) {
      return;
    }
    try {
      const recognition = new Ctor();
      recognition.lang = navigator.language || 'en-US';
      recognition.interimResults = false;
      recognition.continuous = false;
      recognition.onresult = (event: SpeechRecognitionEventLike) => {
        let text = '';
        for (let i = event.resultIndex; i < event.results.length; i++) {
          text += event.results[i][0]?.transcript ?? '';
        }
        if (!text) {
          return;
        }
        const next = this.draft() ? `${this.draft()} ${text.trim()}` : text.trim();
        this.setComposerText(next);
      };
      recognition.onend = () => this.dictating.set(false);
      recognition.onerror = () => this.dictating.set(false);
      recognition.start();
      this.recognition = recognition;
      this.dictating.set(true);
    } catch {
      this.micSupported.set(false);
      this.dictating.set(false);
    }
  }

  describe(err: unknown): string {
    return err instanceof Error ? err.message : String(err);
  }

  /** Truncate a queued message for display (keeps first 60 chars). */
  truncateQueueText(text: string): string {
    return text.length > 60 ? text.slice(0, 60) + '\u2026' : text;
  }

  /** Preview data URL for a queued prompt's images. */
  queuedImageUrl(image: PromptImage): string {
    return `data:${image.media_type};base64,${image.data}`;
  }

  /** Preview data URL for an optimistic pending echo's images. */
  pendingImageUrl(image: PromptImage): string {
    return `data:${image.media_type};base64,${image.data}`;
  }

  // ---------------------------------------------------------------------------
  // Image attachments (composer): file picker + drag&drop + paste.
  // ---------------------------------------------------------------------------

  onFilesPicked(event: Event): void {
    const input = event.target as HTMLInputElement | null;
    if (input?.files) {
      void this.addFiles([...input.files]);
      // Reset so picking the same file twice still fires `change`.
      input.value = '';
    }
  }

  onComposerDragOver(event: DragEvent): void {
    event.preventDefault();
    if (event.dataTransfer) {
      event.dataTransfer.dropEffect = 'copy';
    }
  }

  onComposerDrop(event: DragEvent): void {
    event.preventDefault();
    event.stopPropagation();
    const dt = event.dataTransfer;
    const files = [...(dt?.files ?? [])];
    if (files.length > 0) {
      void this.addFiles(files);
      return;
    }
    // Folder drop (or files without File entries): read entries via webkitGetAsEntry.
    const entries = [...(dt?.items ?? [])]
      .map((item) => (typeof item.webkitGetAsEntry === 'function' ? item.webkitGetAsEntry() : null))
      .filter((e): e is FileSystemEntry => e !== null && e.isFile);
    if (entries.length > 0) {
      void this.addEntryFiles(entries as FileSystemFileEntry[]);
    }
  }

  onComposerPaste(event: ClipboardEvent): void {
    const cd = event.clipboardData;
    if (!cd) {
      return;
    }
    const files = [...(cd.files ?? [])];
    if (files.length > 0) {
      event.preventDefault();
      void this.addFiles(files);
      return;
    }
    // Chrome/Electron expose pasted images only via items (files is empty).
    const itemFiles = [...(cd.items ?? [])]
      .filter((item) => item.kind === 'file')
      .map((item) => item.getAsFile())
      .filter((f): f is File => f !== null);
    if (itemFiles.length > 0) {
      event.preventDefault();
      void this.addFiles(itemFiles);
      return;
    }
    // Plain-text paste stays native (keeps the undo stack intact).
    // Tauri webview fallback: the sync clipboardData is often empty for
    // images, so try the async Clipboard API (needs clipboard-read
    // permission / focus; failure simply keeps the native paste).
    void this.pasteFromAsyncClipboard();
  }

  /**
   * Tauri webview fallback for image paste: `clipboardData` arrives empty,
   * but `navigator.clipboard.read()` can still see the image. Only stages
   * files when something readable is found; otherwise leaves the native
   * (text) paste untouched.
   */
  private async pasteFromAsyncClipboard(): Promise<void> {
    const clipboard = navigator.clipboard as unknown as {
      read?: () => Promise<Array<{ types: string[]; getType: (t: string) => Promise<Blob> }>>;
    } | undefined;
    if (typeof clipboard?.read !== 'function') {
      return;
    }
    try {
      const items = await clipboard.read();
      const files: File[] = [];
      for (const item of items ?? []) {
        for (const type of item.types ?? []) {
          if (!type.startsWith('image/')) {
            continue;
          }
          try {
            const blob = await item.getType(type);
            const ext = type.split('/')[1] ?? 'png';
            files.push(new File([blob], `pasted-image.${ext}`, { type }));
          } catch {
            // Unreadable type: try the next one.
          }
        }
      }
      if (files.length > 0) {
        await this.addFiles(files);
      }
    } catch {
      // Denied/unsupported: the native paste already handled text.
    }
  }

  /** Read dropped FileSystemFileEntry objects into File attachments. */
  private async addEntryFiles(entries: FileSystemFileEntry[]): Promise<void> {
    const files = await Promise.all(
      entries.map(
        (entry) =>
          new Promise<File | null>((resolve) => {
            entry.file(
              (f) => resolve(f),
              () => resolve(null),
            );
          }),
      ),
    );
    const valid = files.filter((f): f is File => f !== null);
    if (valid.length > 0) {
      await this.addFiles(valid);
    }
  }

  /** Validate and stage image or UTF-8 text-file attachments. */
  async addFiles(files: File[]): Promise<void> {
    for (const file of files) {
      const mime = (file.type || '').toLowerCase() || 'application/octet-stream';
      if (file.size === 0) {
        // Zero-byte File with a name usually means an unsupported drop shape
        // (e.g. a dropped folder entry); skip silently instead of an error.
        if (!file.name) {
          this.attachError.set(this.t('chat.emptyAttachment'));
        }
        continue;
      }
      const isImage = mime.startsWith('image/') || ACCEPTED_IMAGE_TYPES.has(mime);
      if (!isImage && !TEXT_FILE_TYPES.has(mime) && !TEXT_FILE_EXTENSIONS.test(file.name)) {
        // MIME allow-list by prefix: many editors report text/*, extension-less
        // files (LICENSE, Dockerfile) still pass via the text/* prefix.
        if (!mime.startsWith('text/')) {
          this.attachError.set(this.t('chat.unsupportedFileType').replace('{name}', file.name));
          continue;
        }
      }
      if (!isImage && file.size > MAX_FILE_BYTES) {
        this.attachError.set(this.t('chat.fileTooLarge').replace('{name}', file.name));
        continue;
      }
      if (isImage && this.attachments().filter((a) => !a.fileBase64).length >= MAX_IMAGES) {
        this.attachError.set(this.t('chat.tooManyImages').replace('{n}', String(MAX_IMAGES)));
        continue;
      }
      if (!isImage && this.attachments().filter((a) => a.fileBase64).length >= MAX_FILES) {
        this.attachError.set(this.t('chat.tooManyFiles').replace('{n}', String(MAX_FILES)));
        continue;
      }
      try {
        let dataUrl: string;
        let fileData: string;
        let size: number;
        if (isImage) {
          const prepared = await prepareImage(file, mime);
          if (prepared.size > MAX_IMAGE_BYTES) {
            this.attachError.set(this.t('chat.imageTooLarge').replace('{name}', file.name));
            continue;
          }
          dataUrl = prepared.dataUrl;
          size = prepared.size;
        } else {
          fileData = await readAsDataUrl(file);
          try {
            const comma = fileData.indexOf(',');
            const bytes = Uint8Array.from(atob(fileData.slice(comma + 1)), (c) => c.charCodeAt(0));
            new TextDecoder('utf-8', { fatal: true }).decode(bytes);
          } catch {
            this.attachError.set(this.t('chat.invalidTextFile').replace('{name}', file.name));
            continue;
          }
          dataUrl = fileData;
          size = file.size;
        }
        const comma = dataUrl.indexOf(',');
        const base64 = comma >= 0 ? dataUrl.slice(comma + 1) : dataUrl;
        this.attachError.set(null);
        this.attachments.update((list) => [
          ...list,
          {
            id: `attach-${++attachmentSeq}`,
            media_type: isImage ? (sniffMediaType(base64) ?? mime) : mime,
            dataUrl,
            base64,
            name: file.name || `attachment-${attachmentSeq}`,
            size,
            ...(isImage ? {} : { fileBase64: base64 }),
          },
        ]);
      } catch {
        this.attachError.set(this.t('chat.unsupportedFileType').replace('{name}', file.name));
      }
    }
  }

  removeAttachment(id: string): void {
    this.attachments.update((list) => list.filter((a) => a.id !== id));
  }
}

/** F6-2: the newest `count` entries of `messages` (the whole list if shorter). */
export function windowMessages<T>(messages: readonly T[], count: number): T[] {
  if (count <= 0) {
    return [];
  }
  return messages.length > count ? messages.slice(-count) : [...messages];
}

/**
 * F6-1c: a "tool-only" turn - the unit `groupMessageRuns` merges - is an
 * assistant message with no `text` part: only tool calls/results, and
 * optionally thinking/usage/image parts. A user message, or any assistant
 * message that *does* carry a text part, is never tool-only and always ends
 * a run.
 */
export function isToolOnlyTurn(message: Message): boolean {
  return message.role === 'assistant' && !message.parts.some((part) => part.type === 'text');
}

/** One rendered transcript row: an ordinary message, or a merged run. */
export interface SingleMessageRow {
  kind: 'single';
  message: Message;
}

/**
 * A run of >= 2 consecutive tool-only turns (F6-1c), rendered as one
 * `<app-tool-run-row>`. `key` is the first message's id: stable for the
 * run's lifetime (it never changes as later turns join the same run), used
 * both as the `@for` track expression and, inside `ToolRunRowComponent`, to
 * decide whether an expand/collapse override still applies.
 */
export interface ToolRunRow {
  kind: 'run';
  key: string;
  messages: Message[];
}

export type TranscriptRow = SingleMessageRow | ToolRunRow;

/**
 * F6-1c: walk a (already windowed/filtered) message list and fold every run
 * of >= 2 consecutive tool-only assistant turns into one `ToolRunRow`. A
 * lone tool-only turn - one with no tool-only neighbour - stays a
 * `SingleMessageRow`, rendered exactly as before merging existed; so does
 * every user message and every assistant turn that carries a text part.
 */
export function groupMessageRuns(messages: readonly Message[]): TranscriptRow[] {
  const rows: TranscriptRow[] = [];
  let run: Message[] = [];

  const flush = (): void => {
    if (run.length >= 2) {
      rows.push({ kind: 'run', key: run[0].id, messages: run });
    } else if (run.length === 1) {
      rows.push({ kind: 'single', message: run[0] });
    }
    run = [];
  };

  for (const message of messages) {
    if (isToolOnlyTurn(message)) {
      run.push(message);
      continue;
    }
    flush();
    rows.push({ kind: 'single', message });
  }
  flush();
  return rows;
}

/** Recover a user prompt into the composer when opening its retry branch. */
export function retryDraft(message: Message): {
  text: string;
  attachments: StagedAttachment[];
} {
  const text = message.parts.find((part) => part.type === 'text');
  const attachments = message.parts.flatMap((part) => {
    if (part.type !== 'image') {
      return [];
    }
    const base64 = part.data;
    return [{
      id: `retry-attachment-${++attachmentSeq}`,
      media_type: part.media_type,
      dataUrl: `data:${part.media_type};base64,${base64}`,
      base64,
      name: part.name ?? '',
      size: part.bytes ?? Math.floor((base64.length * 3) / 4),
    }];
  });
  return { text: text?.type === 'text' ? text.text : '', attachments };
}

/** Read a file as a `data:<mime>;base64,...` URL. */
function readAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result ?? ''));
    reader.onerror = () => reject(reader.error ?? new Error('read failed'));
    reader.readAsDataURL(file);
  });
}

/** Decoded byte size of a `data:<mime>;base64,<payload>` URL. */
function dataUrlBytes(dataUrl: string): number {
  const comma = dataUrl.indexOf(',');
  const payload = comma >= 0 ? dataUrl.length - comma - 1 : dataUrl.length;
  return Math.floor((payload * 3) / 4);
}

/** Detect an image media type from the payload's magic bytes (client-side labelling). */
function sniffMediaType(base64: string): string | null {
  const head = base64.slice(0, 24);
  if (head.length < 4) return null;
  let raw: string;
  try {
    raw = atob(head.slice(0, Math.floor(head.length / 4) * 4));
  } catch {
    return null;
  }
  const bytes = [...raw].map((c) => c.charCodeAt(0));
  const ascii = (from: number, to: number) =>
    String.fromCharCode(...bytes.slice(from, to));
  if (bytes[0] === 0x89 && ascii(1, 4) === 'PNG') return 'image/png';
  if (bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) return 'image/jpeg';
  if (ascii(0, 4) === 'GIF8') return 'image/gif';
  if (bytes.length >= 12 && ascii(0, 4) === 'RIFF' && ascii(8, 12) === 'WEBP') {
    return 'image/webp';
  }
  return null;
}

/**
 * Read + normalize a staged image file.
 *
 * Files above ~1 MiB are downscaled to at most `IMAGE_MAX_EDGE` on the longest
 * edge and re-encoded (PNG keeps PNG/alpha, everything else becomes JPEG) with
 * a quality step-down until the payload is near `IMAGE_TARGET_BYTES`. This
 * keeps a large photo from becoming ~6.7 MB of base64 in every request and in
 * the on-disk transcript. The engine's 5 MB / 5-image limits and payload
 * validation remain authoritative.
 *
 * Animated GIFs are passed through untouched (canvas would keep only frame 1).
 */
async function prepareImage(
  file: File,
  declared: string,
): Promise<{ dataUrl: string; media_type: string; size: number }> {
  const original = await readAsDataUrl(file);
  if (file.size <= IMAGE_DOWNSCALE_THRESHOLD || declared === 'image/gif') {
    return { dataUrl: original, media_type: declared, size: file.size };
  }
  try {
    const bitmap = await createImageBitmap(file);
    const longest = Math.max(bitmap.width, bitmap.height) || 1;
    const scale = Math.min(1, IMAGE_MAX_EDGE / longest);
    const canvas = document.createElement('canvas');
    canvas.width = Math.max(1, Math.round(bitmap.width * scale));
    canvas.height = Math.max(1, Math.round(bitmap.height * scale));
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('no 2d context');
    ctx.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
    bitmap.close?.();
    let outType = declared === 'image/png' ? 'image/png' : 'image/jpeg';
    let dataUrl = canvas.toDataURL(outType, 0.85);
    let quality = 0.85;
    while (dataUrlBytes(dataUrl) > IMAGE_TARGET_BYTES && quality > 0.4) {
      quality -= 0.15;
      outType = 'image/jpeg';
      dataUrl = canvas.toDataURL('image/jpeg', quality);
    }
    if (dataUrlBytes(dataUrl) > MAX_IMAGE_BYTES) {
      throw new Error('still too large after downscale');
    }
    return { dataUrl, media_type: outType, size: dataUrlBytes(dataUrl) };
  } catch {
    // Downscaling unavailable (no createImageBitmap/canvas): keep the original
    // bytes; the engine's size check stays authoritative.
    return { dataUrl: original, media_type: declared, size: file.size };
  }
}

/** Extract a full message snapshot from event properties. */
function properties_message(props: Record<string, unknown>): Message | null {
  const raw = props['message'];
  if (raw && typeof raw === 'object') {
    return raw as Message;
  }
  return null;
}
