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
  AgentInfo,
  EngineEvent,
  Message,
  SessionMeta,
} from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { OpenSessionsStore } from '../../core/open-sessions.store';
import { I18nService } from '../../i18n/i18n.service';
import { PermissionPopup } from '../../ui/permission-popup/permission-popup';
import { SessionSidebarComponent } from '../../ui/session-sidebar/session-sidebar';
import { MessageRowComponent } from './parts/message-row';

const REFRESH_DEBOUNCE_MS = 300;
const DRAFT_KEY = 'bebok.sessionDrafts';
let pendingSeq = 0;

/** A user prompt echoed locally while waiting for the engine to reflect it. */
interface PendingPrompt {
  id: string;
  text: string;
  /** "sending" until accepted by the engine, then "sent". */
  state: 'sending' | 'sent';
  ts: number;
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
  imports: [FormsModule, RouterLink, PermissionPopup, MessageRowComponent, SessionSidebarComponent],
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

  /** M6: filter the transcript by model (driven from the sidebar). */
  readonly filterModel = signal<string | null>(null);

  /** M6: prompt queue - messages waiting to be sent while a turn runs. */
  readonly queue = signal<string[]>([]);

  /** Optimistic echo of prompts sent this session (shown immediately on the
   *  board, replaced once the engine reflects the real message). */
  readonly pending = signal<PendingPrompt[]>([]);

  readonly modelsUsed = computed<string[]>(() => {
    const set = new Set<string>();
    for (const m of this.messages()) {
      const model = m.meta?.model;
      if (model) {
        set.add(model);
      }
    }
    return [...set];
  });

  readonly filteredMessages = computed<Message[]>(() => {
    const filter = this.filterModel();
    if (!filter) {
      return this.messages();
    }
    return this.messages().filter((m) => m.meta?.model === filter);
  });

  readonly scrollArea = viewChild<ElementRef<HTMLElement>>('scroll');

  /** Per-session drafts (sessionID -> text), persisted to localStorage. */
  private readonly drafts: Record<string, string> = loadDrafts();
  private readonly unsubscribeEvents: () => void;
  private refreshTimer: number | undefined;
  private lastReconnectVersion = 0;
  private routeSub: Subscription | null = null;
  /** Monotonic load generation: stale fetches from a previous tab never win. */
  private loadSeq = 0;

  constructor() {
    this.unsubscribeEvents = this.events.onEvent((ev) => this.handleEvent(ev));

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

    // Drain the prompt queue once the agent becomes idle.
    effect(() => {
      const running = this.running();
      if (!running && this.queue().length > 0) {
        void this.drainQueue();
      }
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
    this.unsubscribeEvents();
    this.routeSub?.unsubscribe();
    this.routeSub = null;
    if (this.refreshTimer !== undefined) {
      window.clearTimeout(this.refreshTimer);
    }
  }

  /**
   * Switch the visible session (tab click, back/forward, direct link).
   * Resets all per-session UI state first so no stale transcript, queue,
   * spinner or error leaks into the new tab, then loads it from the engine.
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
    this.sessionID.set(nextID);
    this.meta.set(null);
    this.messages.set([]);
    this.pending.set([]);
    this.queue.set([]);
    this.error.set(null);
    this.running.set(false);
    this.sending.set(false);
    this.filterModel.set(null);
    this.follow.set(true);
    await this.loadAll(nextID, seq);
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
      this.directory.set(meta.directory);
      this.selectedAgent.set(meta.agent);
      this.selectedModel.set(meta.model ?? '');
      // Restore this session's draft (per-session input, survives tab switches).
      this.draft.set(this.drafts[sessionID] ?? '');
      // Nav tab: open (or refresh the title of) this session's tab.
      if (!this.tabs.get(meta.id)) {
        this.tabs.open(meta.id, meta.title ?? null);
      } else {
        this.tabs.setTitle(meta.id, meta.title ?? null);
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
    this.pending.update((p) => p.filter((x) => !texts.has(x.text)));
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
          const err = event.properties?.['error'];
          if (typeof err === 'string' && err) {
            this.error.set(err);
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

  async sendPrompt(): Promise<void> {
    const text = this.draft().trim();
    if (!text || this.loading()) {
      return;
    }
    this.draft.set('');
    this.saveDraft('');
    // Always enqueue; sends immediately when idle, otherwise waits for the turn.
    this.queue.update((q) => [...q, text]);
    this.pending.update((p) => [
      ...p,
      { id: `pending-${++pendingSeq}`, text, state: 'sending', ts: Date.now() },
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
    const text = this.queue()[0];
    this.sending.set(true);
    this.error.set(null);
    try {
      await this.engine.prompt(
        sessionID,
        text,
        this.selectedAgent(),
        this.selectedModel() || undefined,
      );
      // Tab switched while the POST was in flight: leave the new tab alone.
      if (sessionID !== this.sessionID()) {
        return;
      }
      this.markPending(text, 'sent');
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

  /**
   * Roll back to just before this user prompt: rewind the *current* session in
   * place, keeping every message before this prompt (index 0..index-1) and
   * erasing this prompt and the turn after it. Stays on the same session - no
   * fork, no new session. The composer keeps any unsent draft for re-editing.
   */
  async rollbackTo(messageID: string): Promise<void> {
    const index = this.messages().findIndex((m) => m.id === messageID);
    // Nothing meaningful before the very first prompt -> nothing to rewind to.
    if (index < 1 || this.sending()) {
      return;
    }
    const sessionID = this.sessionID();
    this.error.set(null);
    try {
      // Stop any running turn first: truncation is refused while busy.
      try {
        await this.engine.abort(sessionID);
      } catch {
        /* best-effort */
      }
      await this.engine.truncateSession(sessionID, index);
      if (sessionID !== this.sessionID()) {
        return;
      }
      this.running.set(false);
      // Re-sync the transcript (message list is shorter now).
      await this.refreshFull();
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

  onComposerKeydown(event: KeyboardEvent): void {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      void this.sendPrompt();
    }
  }

  describe(err: unknown): string {
    return err instanceof Error ? err.message : String(err);
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
