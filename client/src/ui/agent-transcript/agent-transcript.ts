/**
 * Read-only sub-agent transcript viewer (F6-13).
 *
 * An in-app overlay (dim backdrop + large card, same family as the command
 * palette / project switcher) that shows one child session's transcript
 * without any way to mutate it: no composer, no queue, no rollback. It
 * fetches `GET /session/{id}/message` once and then follows the same SSE
 * stream the chat view uses, filtered by the child session id:
 *
 * - `message.part.updated` carries the full message snapshot -> patched in
 *   place by `messageIndex` (fast path, no refetch);
 * - `message.updated` / `session.updated{running:false}` / an SSE reconnect
 *   -> debounced full refetch, so nothing that fell into a gap is lost.
 *
 * `app-message-row` (the chat's own row component) is reused as-is; it is
 * self-contained and `rollbackEnabled` stays at its default `false`.
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  effect,
  inject,
  input,
  output,
  signal,
  viewChild,
} from '@angular/core';

import { EngineClient } from '../../core/engine-client.service';
import { AgentStatus, EngineEvent, Message } from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { I18nService } from '../../i18n/i18n.service';
import { MessageKey } from '../../i18n';
import { MessageRowComponent } from '../../views/chat/parts/message-row';

const REFRESH_DEBOUNCE_MS = 120;

const STATUS_LABEL: Record<AgentStatus, MessageKey> = {
  running: 'agents.running',
  done: 'agents.done',
  failed: 'agents.failed',
  aborted: 'agents.aborted',
  unknown: 'agents.unknown',
};

@Component({
  selector: 'app-agent-transcript',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MessageRowComponent],
  host: { '(document:keydown.escape)': 'closed.emit()' },
  template: `
    <div class="backdrop" (click)="onBackdrop($event)" data-testid="agent-transcript">
      <div class="card" role="dialog" aria-modal="true" [attr.aria-label]="t('agents.transcript')">
        <header class="head">
          <div class="title">
            <span class="who">{{ name() }}</span>
            <span class="preset">{{ agent() }}</span>
            @if (model()) {
              <span class="model">{{ model() }}</span>
            }
            <span class="status" [attr.data-status]="status()">
              @if (status() === 'running') {
                <span class="pulse" aria-hidden="true"></span>
              }
              {{ statusLabel() }}
            </span>
          </div>
          <span class="readonly">{{ t('agents.readOnly') }}</span>
          <button
            type="button"
            class="close"
            (click)="closed.emit()"
            [title]="t('agents.close')"
            [attr.aria-label]="t('agents.close')"
          >
            ✕
          </button>
        </header>
        <div class="body" #scrollArea (scroll)="onScroll()">
          @if (error()) {
            <div class="notice error">{{ error() }}</div>
          } @else if (loading() && messages().length === 0) {
            <div class="notice">{{ t('chat.loading') }}</div>
          } @else if (messages().length === 0) {
            <div class="notice">{{ t('agents.empty') }}</div>
          }
          <div class="messages">
            @for (message of messages(); track message.id) {
              <app-message-row [rowId]="'agent-msg-' + message.id" [message]="message" />
            }
          </div>
        </div>
      </div>
    </div>
  `,
  styles: [
    `
      .backdrop {
        position: fixed;
        inset: 0;
        background: #00000099;
        display: flex;
        align-items: center;
        justify-content: center;
        padding: 32px;
        z-index: 50;
      }

      .card {
        display: flex;
        flex-direction: column;
        width: 920px;
        max-width: 100%;
        height: 100%;
        max-height: 88vh;
        background: var(--surface);
        border: 1px solid var(--border);
        border-radius: var(--radius-bubble);
        box-shadow: 0 20px 60px rgba(0, 0, 0, 0.5);
        overflow: hidden;
      }

      .head {
        display: flex;
        align-items: center;
        gap: var(--space-10);
        padding: var(--space-10) var(--space-14);
        border-bottom: 1px solid var(--border);
        flex: none;
      }

      .title {
        display: flex;
        align-items: baseline;
        gap: var(--space-8);
        min-width: 0;
        flex: 1 1 auto;
      }

      .who {
        font-size: var(--fs-13);
        font-weight: 600;
        color: var(--text);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .preset {
        font-family: var(--font-mono);
        font-size: var(--fs-11);
        color: var(--accent);
      }

      .model {
        font-family: var(--font-mono);
        font-size: var(--fs-11);
        color: var(--text-faint);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .status {
        display: inline-flex;
        align-items: center;
        gap: 5px;
        font-size: 10px;
        text-transform: uppercase;
        letter-spacing: var(--label-tracking);
        color: var(--text-faint);
      }

      .status[data-status='running'] {
        color: var(--accent);
      }

      .status[data-status='failed'] {
        color: var(--danger);
      }

      .status[data-status='aborted'] {
        color: var(--warning);
      }

      .pulse {
        width: 7px;
        height: 7px;
        border-radius: 50%;
        background: var(--accent);
        animation: transcript-pulse 1.2s ease-in-out infinite;
      }

      @keyframes transcript-pulse {
        0%,
        100% {
          opacity: 1;
        }
        50% {
          opacity: 0.3;
        }
      }

      .readonly {
        flex: none;
        font-size: 10px;
        text-transform: uppercase;
        letter-spacing: var(--label-tracking);
        color: var(--text-faint);
        border: 1px solid var(--border);
        border-radius: var(--radius-bubble);
        padding: 1px var(--space-8);
      }

      .close {
        flex: none;
        background: transparent;
        border: none;
        color: var(--text-faint);
        font-size: var(--fs-13);
        cursor: pointer;
        padding: 2px 6px;
        border-radius: var(--radius-control-sm);
      }

      .close:hover {
        color: var(--text);
        background: var(--surface-2);
      }

      .body {
        flex: 1 1 auto;
        min-height: 0;
        overflow-y: auto;
        padding: var(--space-14) var(--space-16) var(--space-16);
      }

      .messages {
        display: flex;
        flex-direction: column;
        gap: var(--space-12);
      }

      .notice {
        padding: var(--space-12);
        font-size: var(--fs-12-5);
        color: var(--text-faint);
      }

      .notice.error {
        color: var(--danger);
      }
    `,
  ],
})
export class AgentTranscript {
  private readonly i18n = inject(I18nService);
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly destroyRef = inject(DestroyRef);

  readonly t = this.i18n.t.bind(this.i18n);

  /** The child session to show. */
  readonly sessionId = input.required<string>();
  readonly name = input('');
  readonly agent = input('');
  readonly model = input('');
  readonly status = input<AgentStatus>('unknown');
  readonly closed = output<void>();

  readonly messages = signal<Message[]>([]);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  private readonly scrollArea = viewChild<ElementRef<HTMLElement>>('scrollArea');
  /** Auto-scroll follows the newest message until the user scrolls up. */
  private follow = true;
  private refreshTimer: number | undefined;

  constructor() {
    effect(() => {
      const id = this.sessionId();
      this.messages.set([]);
      this.error.set(null);
      this.follow = true;
      void this.load(id);
    });
    effect(() => {
      // Skip the initial value: `load()` already fetched the transcript.
      if (this.events.reconnectVersion() > 0) {
        this.scheduleRefresh();
      }
    });
    effect(() => {
      this.messages();
      if (this.follow) {
        this.scrollToBottom();
      }
    });
    const unsubscribe = this.events.onEvent((ev) => this.handleEvent(ev));
    this.destroyRef.onDestroy(() => {
      unsubscribe();
      if (this.refreshTimer !== undefined) {
        window.clearTimeout(this.refreshTimer);
      }
    });
  }

  statusLabel(): string {
    return this.t(STATUS_LABEL[this.status()] ?? 'agents.unknown');
  }

  onBackdrop(event: MouseEvent): void {
    if (event.target === event.currentTarget) {
      this.closed.emit();
    }
  }

  onScroll(): void {
    const el = this.scrollArea()?.nativeElement;
    if (!el) {
      return;
    }
    this.follow = el.scrollHeight - el.scrollTop - el.clientHeight < 90;
  }

  /** Route one SSE event: only the child session's own events matter here. */
  handleEvent(event: EngineEvent): void {
    if (event.sessionID !== this.sessionId()) {
      return;
    }
    switch (event.type) {
      case 'message.part.updated':
        this.applyPartEvent(event);
        break;
      case 'message.updated':
        this.scheduleRefresh();
        break;
      case 'session.updated':
        if (event.properties?.['running'] === false) {
          this.scheduleRefresh();
        }
        break;
      default:
        break;
    }
  }

  /** Fast path: the event carries the full message snapshot - patch by index. */
  private applyPartEvent(event: EngineEvent): void {
    const props = event.properties;
    if (!props) {
      return;
    }
    const index = props['messageIndex'];
    const raw = props['message'];
    if (typeof index !== 'number' || !raw || typeof raw !== 'object') {
      return;
    }
    const message = raw as Message;
    this.messages.update((list) => {
      if (index < 0 || index > list.length) {
        // Out of range (events from before our snapshot): resync instead.
        this.scheduleRefresh();
        return list;
      }
      const next = [...list];
      if (index === list.length) {
        // A brand-new message we have not fetched yet (the child appended
        // its assistant turn after our snapshot): append it in place.
        next.push(message);
      } else {
        next[index] = message;
      }
      return next;
    });
  }

  private scheduleRefresh(): void {
    if (this.refreshTimer !== undefined) {
      return;
    }
    this.refreshTimer = window.setTimeout(() => {
      this.refreshTimer = undefined;
      void this.load(this.sessionId());
    }, REFRESH_DEBOUNCE_MS);
  }

  private async load(id: string): Promise<void> {
    this.loading.set(true);
    try {
      const messages = await this.engine.messages(id);
      if (id !== this.sessionId()) {
        return;
      }
      this.messages.set(messages);
      this.error.set(null);
    } catch (err) {
      if (id === this.sessionId()) {
        this.error.set(err instanceof Error ? err.message : String(err));
      }
    } finally {
      if (id === this.sessionId()) {
        this.loading.set(false);
      }
    }
  }

  private scrollToBottom(): void {
    const el = this.scrollArea()?.nativeElement;
    if (!el) {
      return;
    }
    requestAnimationFrame(() => {
      el.scrollTop = el.scrollHeight;
    });
  }
}
