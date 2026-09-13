import {
  Component,
  ElementRef,
  Injector,
  OnDestroy,
  computed,
  effect,
  inject,
  input,
  signal,
  viewChild,
} from '@angular/core';

import { EngineClient } from '../../core/engine-client.service';
import { EngineEvent, PermissionAsked } from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { prettyJson } from '../../core/format';
import { OfflineQueue, isNetworkError } from '../../core/remote/offline-cache';
import { I18nService } from '../../i18n/i18n.service';
import { ToastStore } from '../toast/toast.store';

export interface PendingAsk {
  /** Session that raised the ask (the sub-agent's child session for `task`). */
  sessionID: string;
  requestID: string;
  messageIndex: number;
  toolName: string;
  agent: string;
  pattern: string;
  input: unknown;
  /** F9-5: alias of the asking session (sub-agent name), absent for a main session. */
  sessionAlias?: string;
  /** F9-5: present when the asking session is a child (sub-agent). */
  parentSessionID?: string;
  /** F9-5: the rule "Always allow" will write (e.g. `write_file(*)`). */
  suggestedRule?: string;
  /** Short display of the arguments. */
  inputText: string;
  askedAt: number;
  busy: boolean;
}

function optionalString(value: unknown): string | undefined {
  return typeof value === 'string' && value.length > 0 ? value : undefined;
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
    sessionAlias: optionalString(properties['sessionAlias']),
    parentSessionID: optionalString(properties['parentSessionID']),
    suggestedRule: optionalString(properties['suggestedRule']),
  };
}

let dialogSeq = 0;

/**
 * Permission prompt (SPEC §7 / M3, redesigned in F2-8, fixes B15).
 *
 * Rendered **inline in the transcript**, attached below the tool call that
 * raised it: prompt text plus Allow (success fill) / Always allow (outline) /
 * Deny (danger outline, right-aligned). It still behaves as a dialog for
 * assistive tech - `role="dialog"`, `aria-modal`, initial focus on Allow, a
 * Tab focus trap and Escape (= Deny) - and the chat composer refuses to send
 * while it is open.
 *
 * The decision is sent to `POST /session/{id}/permission/{requestID}` - the
 * engine (not the GUI) decides what happens next.
 */
@Component({
  selector: 'app-permission-popup',
  template: `
    @if (asks().length > 0) {
      @if (mode() === 'sheet') {
        <div class="sheet-backdrop" aria-hidden="true" data-testid="perm-sheet-backdrop"></div>
      }
      <div
        class="perm"
        [class.perm-sheet]="mode() === 'sheet'"
        role="dialog"
        aria-modal="true"
        [attr.aria-labelledby]="titleId"
        [attr.aria-describedby]="bodyId"
        [attr.data-mode]="mode()"
        (keydown)="onKeydown($event)"
        #dialog
      >
        @if (mode() === 'sheet') {
          <div class="sheet-handle" aria-hidden="true"><span class="grip"></span></div>
        }
        <div class="perm-head">
          <span class="dot" aria-hidden="true"></span>
          <h3 class="perm-title" [id]="titleId">{{ t('perm.title') }}</h3>
          @if (asks().length > 1) {
            <span class="queue">{{ t('perm.queued', { n: asks().length - 1 }) }}</span>
          }
        </div>

        <p class="perm-body" [id]="bodyId" data-testid="perm-body">
          @if (current().parentSessionID) {
            {{ t('perm.subagentHint', {
              name: current().sessionAlias || current().agent,
              agent: current().agent,
              tool: current().toolName
            }) }}
          } @else {
            {{ t('perm.hint', { tool: current().toolName, agent: current().agent }) }}
          }
        </p>

        <div class="meta">
          <span class="meta-label">{{ t('perm.pattern') }}</span>
          <code data-testid="perm-pattern">{{ current().suggestedRule || current().pattern }}</code>
        </div>

        <details>
          <summary>{{ t('perm.arguments') }}</summary>
          <pre><code>{{ current().inputText }}</code></pre>
        </details>

        <div class="perm-actions">
          <button
            type="button"
            class="allow"
            #allowBtn
            (click)="allow(false)"
            [disabled]="current().busy"
          >{{ t('perm.allow') }}</button>
          <button
            type="button"
            class="always"
            (click)="allow(true)"
            [disabled]="current().busy"
            [title]="alwaysTitle()"
            data-testid="perm-always"
          >{{ t('perm.allowAlwaysTool') }}</button>
          <button
            type="button"
            class="deny"
            (click)="deny()"
            [disabled]="current().busy"
          >{{ t('perm.deny') }}</button>
        </div>
      </div>
    }
  `,
  styles: `
    :host {
      display: block;
    }

    .perm {
      border: 1px solid var(--warning);
      border-radius: var(--radius-panel);
      background: var(--surface);
      padding: var(--space-12) var(--space-14);
      display: flex;
      flex-direction: column;
      gap: var(--space-8);
    }

    .perm-head {
      display: flex;
      align-items: center;
      gap: var(--space-8);
    }

    .dot {
      width: 7px;
      height: 7px;
      border-radius: 50%;
      background: var(--warning);
      flex: none;
    }

    .perm-title {
      margin: 0;
      font-size: var(--fs-13-5);
      font-weight: 600;
      color: var(--text);
    }

    .queue {
      margin-left: auto;
      font-size: var(--fs-11);
      color: var(--text-faint);
    }

    .perm-body {
      margin: 0;
      font-size: var(--fs-13);
      line-height: 1.55;
      color: var(--text-muted);
    }

    .meta {
      display: flex;
      align-items: baseline;
      gap: var(--space-6);
      font-size: var(--fs-11-5);
    }

    .meta-label {
      text-transform: uppercase;
      letter-spacing: var(--label-tracking);
      color: var(--text-faint);
      font-size: var(--fs-11);
    }

    .meta code {
      font-family: var(--font-mono);
      color: var(--text);
      overflow-wrap: anywhere;
    }

    details {
      font-size: var(--fs-12);
    }

    summary {
      cursor: pointer;
      color: var(--text-faint);
      user-select: none;
    }

    pre {
      background: var(--bg);
      border: 1px solid var(--border);
      border-radius: var(--radius-control-sm);
      padding: 8px 10px;
      overflow: auto;
      max-height: 260px;
      font-size: var(--fs-12);
      color: var(--code-text-strong);
      margin: 6px 0 0;
    }

    .perm-actions {
      display: flex;
      align-items: center;
      gap: var(--space-8);
    }

    .perm-actions button {
      font-size: var(--fs-12-5);
      padding: 5px 14px;
      border-radius: var(--radius-control-sm);
    }

    .allow {
      background: var(--success);
      border: 1px solid var(--success);
      color: var(--bg);
      font-weight: 600;
    }

    .allow:hover:not(:disabled) {
      background: var(--success);
      filter: brightness(1.08);
    }

    .always {
      background: transparent;
      border: 1px solid var(--border-strong);
      color: var(--text-muted);
    }

    .always:hover:not(:disabled) {
      color: var(--text);
      border-color: var(--text-muted);
    }

    .deny {
      margin-left: auto;
      background: transparent;
      border: 1px solid var(--danger);
      color: var(--danger);
    }

    .deny:hover:not(:disabled) {
      background: rgba(226, 100, 95, 0.12);
    }

    /* WP-M6 (F10-25): bottom-sheet variant for the phone. Same look as
       ui/mobile-shell/sheet.ts (backdrop, grip, rounded top) without
       depending on it - the prompt stays a self-contained dialog. */
    .sheet-backdrop {
      position: fixed;
      inset: 0;
      background: rgba(0, 0, 0, 0.55);
      z-index: 60;
    }

    .perm.perm-sheet {
      position: fixed;
      left: 0;
      right: 0;
      bottom: 0;
      z-index: 61;
      max-height: min(85dvh, 85vh);
      overflow-y: auto;
      border: 0;
      border-top: 1px solid var(--border-strong);
      border-radius: 12px 12px 0 0;
      padding: 0 var(--space-16) calc(var(--space-16) + env(safe-area-inset-bottom, 0px));
      box-shadow: 0 -8px 32px rgba(0, 0, 0, 0.45);
      animation: perm-sheet-in 180ms ease-out;
    }

    @keyframes perm-sheet-in {
      from {
        transform: translateY(100%);
      }
      to {
        transform: translateY(0);
      }
    }

    .sheet-handle {
      display: flex;
      justify-content: center;
      padding: 10px 0 6px;
    }

    .grip {
      width: 36px;
      height: 4px;
      border-radius: 2px;
      background: var(--border-strong);
    }

    .perm-sheet .perm-title {
      font-size: var(--fs-14);
    }

    .perm-sheet .perm-body {
      font-size: var(--fs-13-5);
    }

    .perm-sheet .perm-actions {
      flex-wrap: wrap;
      margin-top: var(--space-4);
    }

    .perm-sheet .perm-actions button {
      flex: 1 1 45%;
      min-height: 44px;
      font-size: var(--fs-13-5);
    }

    .perm-sheet .deny {
      flex-basis: 100%;
      margin-left: 0;
    }

    .perm-sheet .queued-note {
      font-size: var(--fs-12);
      color: var(--warning);
      margin: 0;
    }
  `,
})
export class PermissionPopup implements OnDestroy {
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly i18n = inject(I18nService);
  private readonly toasts = inject(ToastStore);
  /**
   * WP-M6: the offline queue is resolved lazily (sheet mode, on a network
   * failure only) so the desktop path never instantiates it.
   */
  private readonly injector = inject(Injector);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly activeSessionID = input<string>();
  /**
   * WP-M6 (F10-25): `inline` (default, in the transcript) or `sheet` - a
   * fixed bottom sheet over the page for the phone. Same data, same
   * decisions, same events; only the chrome differs.
   */
  readonly mode = input<'inline' | 'sheet'>('inline');
  /**
   * Working directory of the open chat. Asks raised by a delegated sub-agent
   * carry the *child* session id, so the prompt also matches by directory -
   * otherwise sub-agent permission prompts would be invisible on the parent.
   */
  readonly directory = input<string>();

  readonly asks = signal<PendingAsk[]>([]);
  readonly current = () => this.asks()[0];
  /** True while a decision is outstanding: the composer must not send. */
  readonly blocking = computed(() => this.asks().length > 0);

  /** Tooltip of "Always allow": the rule the engine will write (F9-5). */
  readonly alwaysTitle = computed(() => {
    const item = this.current();
    if (!item) {
      return '';
    }
    const rule = item.suggestedRule || item.pattern;
    const title = this.t('perm.allowAlwaysToolTitle', { tool: item.toolName });
    return rule ? `${title}\n${rule}` : title;
  });

  readonly titleId = `perm-title-${++dialogSeq}`;
  readonly bodyId = `perm-body-${dialogSeq}`;

  private readonly dialog = viewChild<ElementRef<HTMLElement>>('dialog');
  private readonly allowBtn = viewChild<ElementRef<HTMLButtonElement>>('allowBtn');

  private unsubscribe: () => void;
  private readonly resolvedIds = new Set<string>();

  constructor() {
    this.unsubscribe = this.events.onEvent((event: EngineEvent) => this.handle(event));
    // Switching to a different directory (or a new chat view) must not show
    // asks from the previous one. Within the same directory we keep asks so
    // sub-agent (child-session) prompts survive parent tab switches.
    effect(() => {
      const directory = this.directory();
      this.asks.set([]);
      this.resolvedIds.clear();
      if (directory) {
        void this.restorePending(directory);
      }
    });
    // A new prompt takes focus so keyboard users land on Allow (B15).
    effect(() => {
      const id = this.current()?.requestID;
      if (!id) {
        return;
      }
      const button = this.allowBtn()?.nativeElement;
      if (button) {
        queueMicrotask(() => button.focus());
      }
    });
  }

  ngOnDestroy(): void {
    this.unsubscribe();
  }

  private async restorePending(directory: string): Promise<void> {
    try {
      const pending = await this.engine.pendingPermissions(directory);
      if (this.directory() !== directory) {
        return;
      }
      this.asks.update((list) => {
        const seen = new Set(list.map((item) => item.requestID));
        const restored = [...list];
        for (const raw of pending) {
          const asked = parseAsked(raw as unknown as Record<string, unknown>);
          if (!asked || !raw.sessionID || seen.has(asked.requestID)
              || this.resolvedIds.has(asked.requestID)) {
            continue;
          }
          seen.add(asked.requestID);
          restored.push({
            ...asked,
            sessionID: raw.sessionID,
            inputText: prettyJson(asked.input),
            askedAt: Date.now(),
            busy: false,
          });
        }
        return restored;
      });
    } catch {
      // The live SSE path remains usable if a reconnect snapshot fails.
    }
  }

  /** Focus trap + Escape (= Deny), keeping the prompt keyboard-complete. */
  onKeydown(event: KeyboardEvent): void {
    if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      this.deny();
      return;
    }
    if (event.key !== 'Tab') {
      return;
    }
    const root = this.dialog()?.nativeElement;
    if (!root) {
      return;
    }
    const focusable = [
      ...root.querySelectorAll<HTMLElement>('button:not([disabled]), summary, [tabindex]:not([tabindex="-1"])'),
    ].filter((el) => el.offsetParent !== null);
    if (focusable.length === 0) {
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const active = document.activeElement as HTMLElement | null;
    if (event.shiftKey && (active === first || !root.contains(active))) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
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
        this.resolvedIds.add(requestID);
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
      if (always && decision === 'allow') {
        this.toasts.show(this.t('perm.allowedToast', { tool: item.toolName }), { kind: 'success' });
      }
    } catch (err) {
      if (this.mode() === 'sheet' && isNetworkError(err)) {
        // WP-M6 (F10-27): the desktop is unreachable right now - queue the
        // decision (10 min TTL) instead of losing it.
        this.injector.get(OfflineQueue).enqueue('permission', sessionID, {
          requestID: item.requestID,
          decision,
          always,
        });
        this.toasts.show(this.t('mobile.remote.queuedDecision'), { kind: 'info' });
      } else {
        console.error('permission decision failed', err);
      }
    } finally {
      this.resolvedIds.add(item.requestID);
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
