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
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import type { ElementRef } from '@angular/core';
import { Subscription } from 'rxjs';

import { EngineClient } from '../../core/engine-client.service';
import {
  ActiveTask,
  AgentInfo,
  EngineEvent,
  Message,
  PromptImage,
  SessionMeta,
} from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { OpenSessionsStore } from '../../core/open-sessions.store';
import { SessionActivityStore } from '../../core/session-activity.store';
import { I18nService } from '../../i18n/i18n.service';
import { PermissionPopup } from '../../ui/permission-popup/permission-popup';
import { ChatSessionStore } from './chat-session.store';
import { MessageRowComponent } from './parts/message-row';
import { ScrollMinimapComponent } from './parts/scroll-minimap';

const REFRESH_DEBOUNCE_MS = 300;
const DRAFT_KEY = 'bebok.sessionDrafts';
const MAX_IMAGES = 5;
const MAX_IMAGE_BYTES = 5 * 1024 * 1024;
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
  /** "sending" until accepted by the engine, then "sent". */
  state: 'sending' | 'sent';
  ts: number;
}

/** One queued prompt: text plus image attachments (raw base64). */
export interface QueuedPrompt {
  text: string;
  images: PromptImage[];
}

/** An image staged in the composer (dataUrl for preview, base64 for sending). */
export interface StagedAttachment {
  id: string;
  media_type: string;
  dataUrl: string;
  base64: string;
  name: string;
  size: number;
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
    FormsModule,
    RouterLink,
    PermissionPopup,
    MessageRowComponent,
    ScrollMinimapComponent,
  ],
  templateUrl: './chat.html',
  styleUrl: './chat.css',
})
export class ChatView implements OnInit, OnDestroy {
  private readonly engine = inject(EngineClient);
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
  readonly draft = signal('');
  readonly follow = signal(true);

  /** M6: agent + model switchers (change mid-chat). */
  readonly agents = signal<AgentInfo[]>([]);
  readonly selectedAgent = signal('code');
  readonly availableModels = signal<string[]>([]);
  readonly selectedModel = signal('');

  /** Reasoning/thinking effort for this directory, set in the chat header. */
  readonly thinking = signal('off');

  /** M6: filter the transcript by model (driven from the drawer's Session panel). */
  readonly filterModel = this.sessionStore.filterModel;

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

  /** Show the "jump to last user message" button when user has scrolled up
   *  and there is at least one user message in the conversation. */
  readonly showJump = computed<boolean>(() => {
    if (this.follow()) {
      return false;
    }
    return this.filteredMessages().some((m) => m.role === 'user');
  });

  readonly scrollArea = viewChild<ElementRef<HTMLElement>>('scroll');
  readonly minimap = viewChild(ScrollMinimapComponent);
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

    // Follow the stream unless the user scrolled up.
    effect(() => {
      this.messages();
      this.running();
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
      if (!running && this.queue().length > 0) {
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
  }

  ngOnDestroy(): void {
    this.sessionStore.clear();
    this.unsubscribeEvents();
    this.routeSub?.unsubscribe();
    this.routeSub = null;
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
    // Reset minimap geometry for the new session.
    this.minimap()?.refresh();
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
      this.selectedAgent.set(meta.agent);
      this.selectedModel.set(meta.model ?? '');
      // Restore this session's draft (per-session input, survives tab switches).
      this.draft.set(this.drafts[sessionID] ?? '');
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
        const models: string[] = [];
        for (const provider of cfg.providers ?? []) {
          // Only show models whose provider has a resolvable API key.
          if (!provider.has_key) {
            continue;
          }
          for (const model of provider.models ?? []) {
            models.push(`${provider.name}/${model}`);
          }
        }
        this.availableModels.set(models);
      } catch {
        /* switcher lists are non-critical */
      }
      // Recompute minimap after messages load + render.
      requestAnimationFrame(() => this.minimap()?.refresh());
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
      // Refresh minimap after transcript sync.
      requestAnimationFrame(() => this.minimap()?.refresh());
    } catch (err) {
      if (sessionID !== this.sessionID()) {
        return;
      }
      this.error.set(this.describe(err));
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
        const running = event.properties?.['running'] === true;
        this.running.set(running);
        if (!running) {
          this.scheduleRefresh();
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
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    this.follow.set(distance < 90);
  }

  /** Scroll to the last user message in the current (filtered) transcript. */
  scrollToLastUser(): void {
    const msgs = this.filteredMessages();
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
    this.draft.set('');
    this.saveDraft('');
    const images: PromptImage[] = staged.map((a) => ({
      media_type: a.media_type,
      data: a.base64,
      ...(a.name ? { name: a.name } : {}),
    }));
    this.attachments.set([]);
    this.attachError.set(null);
    // Always enqueue; sends immediately when idle, otherwise waits for the turn.
    this.queue.update((q) => [...q, { text, images }]);
    this.pending.update((p) => [
      ...p,
      {
        id: `pending-${++pendingSeq}`,
        text,
        ...(images.length ? { images } : {}),
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
    if (this.running() || this.sending() || this.queue().length === 0) {
      return;
    }
    const sessionID = this.sessionID();
    if (!sessionID) {
      return;
    }
    const head = this.queue()[0];
    this.sending.set(true);
    this.error.set(null);
    try {
      await this.engine.prompt(
        sessionID,
        {
          message: head.text,
          agent: this.selectedAgent(),
          ...(this.selectedModel() ? { model: this.selectedModel() } : {}),
          ...(head.images.length ? { images: head.images } : {}),
        },
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
    if (this.queue().length === 0) {
      return;
    }
    const sessionID = this.sessionID();
    try {
      await this.engine.abort(sessionID);
    } catch {
      /* abort is best-effort */
    }
    if (sessionID !== this.sessionID()) {
      return;
    }
    this.running.set(false);
    await this.drainQueue();
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

  /** Save the draft for this session (called on every input change). */
  saveDraft(text: string): void {
    const sessionID = this.sessionID();
    if (!sessionID) {
      return;
    }
    this.drafts[sessionID] = text;
    persistDrafts(this.drafts);
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

  async abortTurn(): Promise<void> {
    const sessionID = this.sessionID();
    this.running.set(false);
    this.error.set(null);
    try {
      await this.engine.abort(sessionID);
    } catch (err) {
      this.error.set(this.describe(err));
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
        this.draft.set(next);
        this.saveDraft(next);
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
    if (this.hasImageDrag(event)) {
      event.preventDefault();
    }
  }

  onComposerDrop(event: DragEvent): void {
    if (!this.hasImageDrag(event)) {
      return;
    }
    event.preventDefault();
    const files = [...(event.dataTransfer?.files ?? [])];
    if (files.length > 0) {
      void this.addFiles(files);
    }
  }

  onComposerPaste(event: ClipboardEvent): void {
    const files = [...(event.clipboardData?.files ?? [])].filter((f) =>
      f.type.startsWith('image/'),
    );
    if (files.length > 0) {
      void this.addFiles(files);
    }
  }

  /** Only intercept drags that actually carry image files (keeps any
   *  drag-to-resize/scroll behaviors on other drags untouched). */
  private hasImageDrag(event: DragEvent): boolean {
    const items = event.dataTransfer?.items;
    if (!items || items.length === 0) {
      return false;
    }
    return [...items].some(
      (item) => item.kind === 'file' && item.type.startsWith('image/'),
    );
  }

  /** Validate + stage image files (reads them as base64 data URLs). */
  async addFiles(files: File[]): Promise<void> {
    for (const file of files) {
      if (this.attachments().length >= MAX_IMAGES) {
        this.attachError.set(
          this.t('chat.tooManyImages').replace('{n}', String(MAX_IMAGES)),
        );
        break;
      }
      const mime = file.type || 'image/png';
      if (!ACCEPTED_IMAGE_TYPES.has(mime)) {
        this.attachError.set(
          this.t('chat.unsupportedImageType').replace('{name}', file.name || mime),
        );
        continue;
      }
      if (file.size > MAX_IMAGE_BYTES) {
        this.attachError.set(
          this.t('chat.imageTooLarge').replace('{name}', file.name || mime),
        );
        continue;
      }
      try {
        const prepared = await prepareImage(file, mime);
        if (prepared.size > MAX_IMAGE_BYTES) {
          this.attachError.set(
            this.t('chat.imageTooLarge').replace('{name}', file.name || mime),
          );
          continue;
        }
        // Split `data:<mime>;base64,<payload>`: send raw base64 only.
        const comma = prepared.dataUrl.indexOf(',');
        const base64 =
          comma >= 0 ? prepared.dataUrl.slice(comma + 1) : prepared.dataUrl;
        // The engine validates and trusts the *payload bytes*; label the
        // attachment with what the bytes actually are so a mislabelled file
        // (e.g. a JPEG named .png) is not rejected for a MIME mismatch.
        const media_type = sniffMediaType(base64) ?? prepared.media_type;
        this.attachError.set(null);
        this.attachments.update((list) => [
          ...list,
          {
            id: `attach-${++attachmentSeq}`,
            media_type,
            dataUrl: prepared.dataUrl,
            base64,
            name: file.name || `image-${attachmentSeq}`,
            size: prepared.size,
          },
        ]);
      } catch {
        this.attachError.set(
          this.t('chat.unsupportedImageType').replace('{name}', file.name || mime),
        );
      }
    }
  }

  removeAttachment(id: string): void {
    this.attachments.update((list) => list.filter((a) => a.id !== id));
  }
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
