/**
 * Right-drawer "Agents" panel (F6-13).
 *
 * Live list of the sub-agents delegated from the open session via the `task`
 * / `fleet` tools. The source of truth is `GET /session/{id}/agents`
 * (F6-12), which merges the engine's in-memory running-task map with the
 * finished child sessions; the panel refetches it on every `task.started` /
 * `task.ended` / `task.aborted` SSE event for the current session (plus when
 * the parent turn settles), so it never polls and never shows a stale
 * "running" row. Clicking a row opens the read-only transcript overlay
 * (`ui/agent-transcript`).
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

import { EngineClient } from '../../../core/engine-client.service';
import { AgentEntry, AgentStatus, EngineEvent } from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { I18nService } from '../../../i18n/i18n.service';
import { MessageKey } from '../../../i18n';
import { ChatSessionStore } from '../../../views/chat/chat-session.store';
import { AgentTranscript } from '../../agent-transcript/agent-transcript';

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
  running: 'agents.running',
  done: 'agents.done',
  failed: 'agents.failed',
  aborted: 'agents.aborted',
  unknown: 'agents.unknown',
};

@Component({
  selector: 'app-agents-panel',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [AgentTranscript],
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
          <li>
            <button
              type="button"
              class="agent"
              [class.running]="agent.status === 'running'"
              [class.failed]="agent.status === 'failed'"
              [class.aborted]="agent.status === 'aborted'"
              [attr.data-status]="agent.status"
              [title]="agent.description || t('agents.openTranscript')"
              (click)="open(agent)"
            >
              <span class="row">
                <span class="status-dot" aria-hidden="true"></span>
                <span class="name">{{ agent.name || shortId(agent.childSessionID) }}</span>
                <span class="status">{{ statusLabel(agent.status) }}</span>
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
              @if (agent.error) {
                <span class="error" [title]="agent.error">{{ agent.error }}</span>
              }
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

      .agent {
        display: flex;
        flex-direction: column;
        gap: 2px;
        width: 100%;
        padding: var(--space-6) var(--space-8);
        border-radius: var(--radius-control-sm);
        border: 1px solid var(--border);
        background: var(--surface-2);
        color: inherit;
        text-align: left;
        cursor: pointer;
        min-width: 0;
      }

      .agent:hover {
        border-color: var(--border-strong);
        background: var(--surface-3);
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
    // A transcript-level reconnect may have swallowed task events.
    effect(() => {
      this.events.reconnectVersion();
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
    if (!id || ev.sessionID !== id || !REFRESH_EVENTS.has(ev.type)) {
      return;
    }
    if (ev.type === 'session.updated' && ev.properties?.['running'] !== false) {
      // Only the "turn settled" edge matters for this panel.
      return;
    }
    this.scheduleRefresh();
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
