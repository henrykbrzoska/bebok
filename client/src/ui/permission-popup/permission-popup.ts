import { Component, OnDestroy, effect, inject, input, signal } from '@angular/core';

import { EngineClient } from '../../core/engine-client.service';
import { EngineEvent, PermissionAsked } from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { prettyJson } from '../../core/format';
import { I18nService } from '../../i18n/i18n.service';

export interface PendingAsk {
  /** Session that raised the ask (the sub-agent's child session for `task`). */
  sessionID: string;
  requestID: string;
  messageIndex: number;
  toolName: string;
  agent: string;
  pattern: string;
  input: unknown;
  /** Short display of the arguments. */
  inputText: string;
  askedAt: number;
  busy: boolean;
}

function parseAsked(properties: Record<string, unknown> | undefined): PermissionAsked | null {
  if (!properties) {
    return null;
  }
  const requestID = properties['requestID'];
  if (typeof requestID !== 'string') {
    return null;
  }
  return {
    requestID,
    messageIndex: Number(properties['messageIndex'] ?? 0),
    toolName: String(properties['toolName'] ?? '?'),
    agent: String(properties['agent'] ?? 'code'),
    input: properties['input'] ?? {},
    pattern: String(properties['pattern'] ?? ''),
  };
}

/**
 * Permission popup (SPEC §7 / M3): consumes `permission.asked` for the active
 * session and lets the user drive the agent loop with Allow / Deny / Always.
 * The decision is sent to `POST /session/{id}/permission/{requestID}` - the
 * engine (not the GUI) decides what happens next.
 */
@Component({
  selector: 'app-permission-popup',
  template: `
    @if (asks().length > 0) {
      <div class="overlay">
        <div class="popup">
          <div class="popup-head">
            <h3>{{ t('perm.title') }}</h3>
            <p class="muted">
              {{ t('perm.hint', { tool: current().toolName, agent: current().agent }) }}
            </p>
          </div>

          <div class="meta">
            <div><span class="muted">{{ t('perm.pattern') }}</span> <code>{{ current().pattern }}</code></div>
            <div><span class="muted">{{ t('perm.message') }}</span> {{ current().messageIndex }}</div>
          </div>

          <details>
            <summary>{{ t('perm.arguments') }}</summary>
            <pre><code>{{ current().inputText }}</code></pre>
          </details>

          <div class="popup-actions">
            <button (click)="deny()" [disabled]="current().busy">{{ t('perm.deny') }}</button>
            <button (click)="allow(false)" [disabled]="current().busy">{{ t('perm.allow') }}</button>
            <button class="primary" (click)="allow(true)" [disabled]="current().busy">
              {{ t('perm.allowAlways') }}
            </button>
          </div>

          @if (asks().length > 1) {
            <div class="queue muted">{{ t('perm.queued', { n: asks().length - 1 }) }}</div>
          }
        </div>
      </div>
    }
  `,
  styles: `
    .overlay {
      position: fixed;
      inset: 0;
      background: rgba(0, 0, 0, 0.45);
      display: flex;
      align-items: center;
      justify-content: center;
      z-index: 50;
    }
    .popup {
      width: min(560px, 92vw);
      background: var(--bg-surface);
      border: 1px solid var(--border);
      border-radius: var(--radius);
      padding: 16px 18px;
      box-shadow: 0 18px 50px rgba(0, 0, 0, 0.4);
    }
    .popup-head h3 {
      margin: 0 0 2px;
    }
    .meta {
      display: flex;
      flex-direction: column;
      gap: 4px;
      margin: 10px 0;
      font-size: 13px;
    }
    details {
      font-size: 12.5px;
      margin-top: 8px;
    }
    summary {
      cursor: pointer;
      color: var(--fg-muted);
    }
    pre {
      background: var(--bg-raised);
      border: 1px solid var(--border);
      border-radius: var(--radius-sm);
      padding: 8px 10px;
      overflow: auto;
      max-height: 260px;
      font-size: 12px;
    }
    .popup-actions {
      display: flex;
      gap: 10px;
      margin-top: 14px;
      justify-content: flex-end;
    }
    .queue {
      font-size: 11.5px;
      margin-top: 8px;
      text-align: right;
    }
  `,
})
export class PermissionPopup implements OnDestroy {
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly activeSessionID = input<string>();
  /**
   * Working directory of the open chat. Asks raised by a delegated sub-agent
   * carry the *child* session id, so the popup also matches by directory -
   * otherwise sub-agent permission prompts would be invisible on the parent.
   */
  readonly directory = input<string>();

  readonly asks = signal<PendingAsk[]>([]);
  readonly current = () => this.asks()[0];
  private unsubscribe: () => void;

  constructor() {
    this.unsubscribe = this.events.onEvent((event: EngineEvent) => this.handle(event));
    // Switching to a different directory (or a new chat view) must not show
    // asks from the previous one. Within the same directory we keep asks so
    // sub-agent (child-session) prompts survive parent tab switches.
    effect(() => {
      this.directory();
      this.asks.set([]);
    });
  }

  ngOnDestroy(): void {
    this.unsubscribe();
  }

  private handle(event: EngineEvent): void {
    const active = this.activeSessionID();
    const dir = this.directory();
    const matches =
      (!!active && event.sessionID === active) || (!!dir && event.directory === dir);
    if (!matches) {
      return;
    }
    if (event.type === 'permission.asked') {
      const asked = parseAsked(event.properties);
      if (!asked) {
        return;
      }
      this.asks.update((list) => {
        if (list.some((a) => a.requestID === asked.requestID)) {
          return list;
        }
        return [
          ...list,
          {
            ...asked,
            sessionID: event.sessionID,
            inputText: prettyJson(asked.input),
            askedAt: Date.now(),
            busy: false,
          },
        ];
      });
    } else if (event.type === 'permission.resolved') {
      const requestID = event.properties?.['requestID'];
      if (typeof requestID === 'string') {
        this.asks.update((list) => list.filter((a) => a.requestID !== requestID));
      }
    }
  }

  private async resolve(decision: 'allow' | 'deny', always: boolean): Promise<void> {
    const item = this.current();
    if (!item) {
      return;
    }
    // Resolve against the session that asked (child session for sub-agents).
    const sessionID = item.sessionID;
    this.asks.update((list) =>
      list.map((a) => (a.requestID === item.requestID ? { ...a, busy: true } : a)),
    );
    try {
      await this.engine.resolvePermission(sessionID, item.requestID, { decision, always });
    } catch (err) {
      console.error('permission decision failed', err);
    } finally {
      this.asks.update((list) => list.filter((a) => a.requestID !== item.requestID));
    }
  }

  allow(always: boolean): void {
    void this.resolve('allow', always);
  }

  deny(): void {
    void this.resolve('deny', false);
  }
}
