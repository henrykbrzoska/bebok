/**
 * Right-drawer "Agents" panel (F6-13).
 *
 * Live list of the sub-agents delegated from the open session via the `task`
 * / `fleet` tools. The source of truth is `GET /session/{id}/agents`
 * (F6-12), which merges the engine's in-memory running-task map with the
 * finished child sessions; the panel refetches it on every `task.started` /
 * `task.ended` / `task.aborted` SSE event for the current session (plus when
 * the parent turn settles), so it never polls and never shows a stale
 * "running" row. Clicking a row (anywhere on the card, Enter/Space from the
 * keyboard) opens the read-only transcript overlay (`ui/agent-transcript`);
 * a small secondary "Open session" action on the card navigates to the
 * child's own chat instead (F9-4) - the same target as the underlined task
 * link in the chat transcript.
 *
 * WP-DELEGATION (F8-2): `task.progress` events (<= 1/s per child) patch the
 * matching row in place - last tool, one-line summary, tokens - without a
 * refetch, and a `queued` row (waiting for a `delegation.max_concurrent`
 * slot) is shown as such.
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  signal,
} from '@angular/core';

import { Router } from '@angular/router';

import { EngineClient } from '../../../core/engine-client.service';
import {
  AgentEntry,
  AgentStatus,
  EngineEvent,
  TaskProgress,
  TaskProgressEvent,
} from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { I18nService } from '../../../i18n/i18n.service';
import { MessageKey } from '../../../i18n';
import { ChatSessionStore } from '../../../views/chat/chat-session.store';
import { AgentTranscript } from '../../agent-transcript/agent-transcript';
import { TaskProgressLine } from '../../task-progress-line/task-progress-line';

/** SSE events that change the answer of `GET /session/{id}/agents`. */
const REFRESH_EVENTS: ReadonlySet<string> = new Set([
  'task.started',
  'task.ended',
  'task.aborted',
  'session.updated',
]);

/** Debounce for bursts of events (a `fleet` spawns N children at once). */
const REFRESH_DEBOUNCE_MS = 150;

const STATUS_LABEL: Record<AgentStatus, MessageKey> = {
  queued: 'agents.queued',
  running: 'agents.running',
  done: 'agents.done',
  failed: 'agents.failed',
  aborted: 'agents.aborted',
  unknown: 'agents.unknown',
};

@Component({
  selector: 'app-agents-panel',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [AgentTranscript, TaskProgressLine],
  template: `
    @if (!session.meta()) {
      <div class="empty">{{ t('drawer.noSession') }}</div>
    } @else if (loading() && agents().length === 0) {
      <div class="empty">{{ t('agents.loading') }}</div>
    } @else if (agents().length === 0) {
      <div class="empty">{{ t('agents.none') }}</div>
    } @else {
      <ul class="agents" data-testid="agents-list">
        @for (agent of agents(); track agent.childSessionID) {
          <li class="card-wrap">
            <button
              type="button"
              class="agent"
              [class.running]="agent.status === 'running'"
              [class.queued]="agent.status === 'queued'"
              [class.failed]="agent.status === 'failed'"
              [class.aborted]="agent.status === 'aborted'"
              [attr.data-status]="agent.status"
              [attr.data-testid]="'agent-card'"
              [title]="agent.description || t('agents.openTranscript')"
              [attr.aria-label]="
                (agent.name || shortId(agent.childSessionID)) +
                ' · ' +
                statusLabel(agent.status) +
                ' · ' +
                t('agents.openTranscript')
              "
              (click)="open(agent)"
            >
              <span class="row">
                <span class="status-dot" aria-hidden="true"></span>
                <span class="name">{{ agent.name || shortId(agent.childSessionID) }}</span>
                <span class="status">{{ statusLabel(agent.status) }}</span>
                <span class="view-hint" aria-hidden="true">{{ t('agents.viewHint') }}</span>
              </span>
              <span class="row meta">
                <span class="agent-preset">{{ agent.agent }}</span>
                @if (agent.model) {
                  <span class="model" [title]="agent.model">{{ agent.model }}</span>
                }
              </span>
              <span class="row meta">
                <span class="time">{{ relative(agent.startedAt) }}</span>
                @if (agent.status !== 'running' && agent.endedAt) {
                  <span class="duration">· {{ duration(agent.startedAt, agent.endedAt) }}</span>
                }
                @if (tokens(agent) > 0) {
                  <span class="tokens">· {{ format(tokens(agent)) }} {{ t('agents.tokens') }}</span>
                }
              </span>
              @if (agent.status === 'running' || agent.status === 'queued') {
                <app-task-progress-line
                  class="progress"
                  [status]="agent.status"
                  [progress]="agent.progress ?? null"
                  [tokens]="tokens(agent)"
                />
              }
              @if (agent.error) {
                <span class="error" [title]="agent.error">{{ agent.error }}</span>
              }
            </button>
            <button
              type="button"
              class="open-session"
              data-testid="agent-open-session"
              (click)="openSession(agent, $event)"
              [title]="t('agents.openSessionHint')"
              [attr.aria-label]="
                t('agents.openSession') + ': ' + (agent.name || shortId(agent.childSessionID))
              "
            >
              <svg
                viewBox="0 0 24 24"
                width="12"
                height="12"
                aria-hidden="true"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
              >
                <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
                <polyline points="15 3 21 3 21 9" />
                <line x1="10" y1="14" x2="21" y2="3" />
              </svg>
              {{ t('agents.openSession') }}
            </button>
          </li>
        }
      </ul>
    }

    @if (selected(); as agent) {
      <app-agent-transcript
        [sessionId]="agent.childSessionID"
        [name]="agent.name || shortId(agent.childSessionID)"
        [agent]="agent.agent"
        [model]="agent.model ?? ''"
        [status]="liveStatus(agent.childSessionID)"
        [progress]="liveProgress(agent.childSessionID)"
        [progressTokens]="liveTokens(agent.childSessionID)"
        (closed)="close()"
      />
    }
  `,
  styles: [
    `
      .empty {
        padding: var(--space-16);
        font-size: var(--fs-11-5);
        color: var(--text-faint);
      }

      .agents {
        list-style: none;
        margin: 0;
        padding: var(--space-8) var(--space-10) var(--space-12);
        display: flex;
        flex-direction: column;
        gap: 4px;
      }

      .card-wrap {
        position: relative;
        min-width: 0;
      }

      .agent {
        display: flex;
        flex-direction: column;
        gap: 2px;
        width: 100%;
        /* Room for the "Open session" action pinned bottom-right (F9-4). */
        padding: var(--space-6) var(--space-8) 26px;
        border-radius: var(--radius-control-sm);
        border: 1px solid var(--border);
        background: var(--surface-2);
        color: inherit;
        text-align: left;
        cursor: pointer;
        min-width: 0;
        transition:
          border-color 120ms ease,
          background 120ms ease,
          box-shadow 120ms ease;
      }

      .agent:hover,
      .agent:focus-visible {
        border-color: var(--border-strong);
        background: var(--surface-3);
        box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent) 35%, transparent);
      }

      .agent:focus-visible {
        outline: 1px solid var(--accent);
        outline-offset: 1px;
      }

      /* "View transcript" hint - appears on hover/focus so the card reads
         as clickable (F9-4 hover affordance). */
      .view-hint {
        flex: none;
        font-size: 10px;
        color: var(--accent);
        opacity: 0;
        transition: opacity 120ms ease;
      }

      .agent:hover .view-hint,
      .agent:focus-visible .view-hint {
        opacity: 1;
      }

      .open-session {
        position: absolute;
        right: var(--space-8);
        bottom: 5px;
        display: inline-flex;
        align-items: center;
        gap: 4px;
        padding: 1px 7px;
        font-size: 10.5px;
        line-height: 1.5;
        color: var(--text-muted);
        background: var(--surface);
        border: 1px solid var(--border);
        border-radius: var(--radius-bubble);
        cursor: pointer;
      }

      .open-session:hover:not(:disabled),
      .open-session:focus-visible {
        color: var(--accent);
        border-color: var(--accent);
        background: var(--surface);
      }

      .row {
        display: flex;
        align-items: baseline;
        gap: var(--space-6);
        min-width: 0;
      }

      .status-dot {
        flex: none;
        align-self: center;
        width: 7px;
        height: 7px;
        border-radius: 50%;
        background: var(--text-faint);
      }

      .agent.running .status-dot {
        background: var(--accent);
        animation: agent-pulse 1.2s ease-in-out infinite;
      }

      .agent.queued .status-dot {
        background: var(--warning);
        animation: agent-pulse 1.2s ease-in-out infinite;
      }

      .agent.queued .status {
        color: var(--warning);
      }

      .progress {
        margin-top: 2px;
        padding-top: 3px;
        border-top: 1px dashed var(--border);
      }

      .agent[data-status='done'] .status-dot {
        background: var(--diff-add-text);
      }

      .agent.failed .status-dot {
        background: var(--danger);
      }

      .agent.aborted .status-dot {
        background: var(--warning);
      }

      @keyframes agent-pulse {
        0%,
        100% {
          opacity: 1;
        }
        50% {
          opacity: 0.3;
        }
      }

      .name {
        flex: 1 1 auto;
        min-width: 0;
        font-size: var(--fs-12-5);
        font-weight: 600;
        color: var(--text);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .status {
        flex: none;
        font-size: 10px;
        text-transform: uppercase;
        letter-spacing: var(--label-tracking);
        color: var(--text-faint);
      }

      .agent.running .status {
        color: var(--accent);
      }

      .agent.failed .status {
        color: var(--danger);
      }

      .agent.aborted .status {
        color: var(--warning);
      }

      .meta {
        font-size: var(--fs-11);
        color: var(--text-faint);
      }

      .agent-preset {
        font-family: var(--font-mono);
        color: var(--accent);
        flex: none;
      }

      .model {
        font-family: var(--font-mono);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .error {
        font-size: var(--fs-11);
        color: var(--danger);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
    `,
  ],
})
export class AgentsPanel {
  private readonly i18n = inject(I18nService);
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly destroyRef = inject(DestroyRef);
  private readonly router = inject(Router);
  readonly session = inject(ChatSessionStore);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly agents = signal<AgentEntry[]>([]);
  readonly loading = signal(false);
  /** Row whose transcript overlay is open. */
  readonly selected = signal<AgentEntry | null>(null);

  /** Ticks every 30 s so the relative "started" labels stay honest. */
  private readonly clock = signal(Date.now());
  private readonly sessionId = computed(() => this.session.meta()?.id ?? null);
  private refreshTimer: number | undefined;
  private loadedFor: string | null = null;
  private lastReconnectVersion = this.events.reconnectVersion();

  constructor() {
    effect(() => {
      const id = this.sessionId();
      if (id !== this.loadedFor) {
        this.loadedFor = id;
        this.agents.set([]);
        this.selected.set(null);
      }
      if (id) {
        void this.refresh(id);
      }
    });
    // A transcript-level reconnect may have swallowed task events. Skip the
    // initial run: the effect above already fetched for the current session.
    effect(() => {
      const version = this.events.reconnectVersion();
      if (version === this.lastReconnectVersion) {
        return;
      }
      this.lastReconnectVersion = version;
      const id = this.sessionId();
      if (id) {
        this.scheduleRefresh();
      }
    });
    const unsubscribe = this.events.onEvent((ev) => this.handleEvent(ev));
    this.destroyRef.onDestroy(unsubscribe);
    const tick = window.setInterval(() => this.clock.set(Date.now()), 30_000);
    this.destroyRef.onDestroy(() => {
      window.clearInterval(tick);
      if (this.refreshTimer !== undefined) {
        window.clearTimeout(this.refreshTimer);
      }
    });
  }

  open(agent: AgentEntry): void {
    this.selected.set(agent);
  }

  /** F9-4: secondary action - go to the child's own chat (what the
   *  underlined task link in the transcript does). Never opens the overlay. */
  openSession(agent: AgentEntry, event: Event): void {
    event.stopPropagation();
    void this.router.navigate(['/chat', agent.childSessionID]);
  }

  close(): void {
    this.selected.set(null);
  }

  statusLabel(status: AgentStatus): string {
    return this.t(STATUS_LABEL[status] ?? 'agents.unknown');
  }

  /** Status of the selected row, tracked live so the overlay header updates. */
  liveStatus(childId: string): AgentStatus {
    return this.agents().find((a) => a.childSessionID === childId)?.status ?? 'unknown';
  }

  /** WP-DELEGATION: progress of the selected row, tracked live. */
  liveProgress(childId: string): TaskProgress | null {
    return this.agents().find((a) => a.childSessionID === childId)?.progress ?? null;
  }

  liveTokens(childId: string): number {
    const row = this.agents().find((a) => a.childSessionID === childId);
    return row ? this.tokens(row) : 0;
  }

  shortId(id: string): string {
    return id.slice(0, 8);
  }

  tokens(agent: AgentEntry): number {
    return (agent.usage?.input_tokens ?? 0) + (agent.usage?.output_tokens ?? 0);
  }

  format(value: number): string {
    return value.toLocaleString();
  }

  /** "3 minutes ago" in the UI language (`Intl.RelativeTimeFormat`). */
  relative(startedAt: number): string {
    const now = this.clock();
    const seconds = Math.round((startedAt - now) / 1000);
    const abs = Math.abs(seconds);
    const unit: Intl.RelativeTimeFormatUnit =
      abs < 60 ? 'second' : abs < 3600 ? 'minute' : abs < 86400 ? 'hour' : 'day';
    const divisor = unit === 'second' ? 1 : unit === 'minute' ? 60 : unit === 'hour' ? 3600 : 86400;
    try {
      return new Intl.RelativeTimeFormat(this.i18n.lang(), { numeric: 'auto' }).format(
        Math.round(seconds / divisor),
        unit,
      );
    } catch {
      return new Date(startedAt).toLocaleTimeString();
    }
  }

  /** Compact wall-clock duration: `12s`, `3m 05s`, `1h 02m`. */
  duration(startedAt: number, endedAt: number): string {
    const total = Math.max(0, Math.round((endedAt - startedAt) / 1000));
    if (total < 60) {
      return `${total}s`;
    }
    const minutes = Math.floor(total / 60);
    const seconds = total % 60;
    if (minutes < 60) {
      return `${minutes}m ${String(seconds).padStart(2, '0')}s`;
    }
    const hours = Math.floor(minutes / 60);
    return `${hours}h ${String(minutes % 60).padStart(2, '0')}m`;
  }

  private handleEvent(ev: EngineEvent): void {
    const id = this.sessionId();
    if (!id || ev.sessionID !== id) {
      return;
    }
    if (ev.type === 'task.progress') {
      this.applyProgress(ev.properties as unknown as TaskProgressEvent | undefined);
      return;
    }
    if (!REFRESH_EVENTS.has(ev.type)) {
      return;
    }
    if (ev.type === 'session.updated' && ev.properties?.['running'] !== false) {
      // Only the "turn settled" edge matters for this panel.
      return;
    }
    this.scheduleRefresh();
  }

  /** WP-DELEGATION: patch one running row from a `task.progress` event. */
  applyProgress(p: TaskProgressEvent | undefined): void {
    if (!p || !p.taskID) {
      return;
    }
    let found = false;
    this.agents.update((list) => {
      const idx = list.findIndex(
        (a) => a.taskID === p.taskID || a.childSessionID === p.childSessionID,
      );
      if (idx < 0) {
        return list;
      }
      found = true;
      const prev = list[idx];
      const next: AgentEntry = {
        ...prev,
        status: p.status === 'queued' ? 'queued' : 'running',
        progress: p.progress ?? prev.progress,
        usage: {
          ...prev.usage,
          input_tokens: p.tokens?.input ?? prev.usage.input_tokens,
          output_tokens: p.tokens?.output ?? prev.usage.output_tokens,
        },
      };
      const out = [...list];
      out[idx] = next;
      return out;
    });
    if (!found) {
      // A child we have not fetched yet (the event raced the refetch).
      this.scheduleRefresh();
      return;
    }
    const open = this.selected();
    if (open && open.childSessionID === p.childSessionID) {
      const next = this.agents().find((a) => a.childSessionID === open.childSessionID);
      if (next) {
        this.selected.set(next);
      }
    }
  }

  private scheduleRefresh(): void {
    if (this.refreshTimer !== undefined) {
      return;
    }
    this.refreshTimer = window.setTimeout(() => {
      this.refreshTimer = undefined;
      const id = this.sessionId();
      if (id) {
        void this.refresh(id);
      }
    }, REFRESH_DEBOUNCE_MS);
  }

  private async refresh(id: string): Promise<void> {
    this.loading.set(true);
    try {
      const agents = await this.engine.sessionAgents(id);
      // Session switched mid-fetch: never paint A's children into B.
      if (this.sessionId() !== id) {
        return;
      }
      this.agents.set(agents);
      const open = this.selected();
      if (open) {
        const next = agents.find((a) => a.childSessionID === open.childSessionID);
        if (next && next !== open) {
          this.selected.set(next);
        }
      }
    } catch {
      /* the agents list is non-critical; the next event retries */
    } finally {
      if (this.sessionId() === id) {
        this.loading.set(false);
      }
    }
  }
}
