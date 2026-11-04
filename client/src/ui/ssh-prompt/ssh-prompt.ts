import {
  Component,
  ElementRef,
  OnDestroy,
  computed,
  effect,
  inject,
  input,
  signal,
  viewChild,
} from '@angular/core';

import { EngineClient } from '../../core/engine-client.service';
import { EngineEvent, SshPrompt } from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { I18nService } from '../../i18n/i18n.service';

/** What the engine is asking for: the SSH login password or a sudo password. */
export type SshPromptKind = 'password' | 'sudo';

export interface PendingSshPrompt {
  requestID: string;
  sessionID: string;
  host: string;
  kind: SshPromptKind;
  askedAt: number;
  busy: boolean;
  error?: string;
}

function parsePrompt(properties: Record<string, unknown> | undefined): SshPrompt | null {
  if (!properties) {
    return null;
  }
  // The engine sends `requestID`/`sessionID`; `requestId`/`sessionId` is
  // accepted too so a plain serde camelCase payload still works.
  const requestID = properties['requestID'] ?? properties['requestId'];
  if (typeof requestID !== 'string' || !requestID) {
    return null;
  }
  return {
    requestID,
    sessionID: String(properties['sessionID'] ?? properties['sessionId'] ?? ''),
    directory: String(properties['directory'] ?? ''),
    host: String(properties['host'] ?? '?'),
    prompt: String(properties['prompt'] ?? 'password'),
  };
}

/**
 * Password dialog for a persistent SSH session.
 *
 * The engine opens one `ssh` process per session and raises an `ssh.prompt`
 * event - with `prompt: "password"` when the host does not accept key auth,
 * and with `prompt: "sudo"` when a command needs root - then blocks the `ssh`
 * tool call until the password arrives. The secret is POSTed straight back to
 * that blocked call: it never enters the transcript, the model context or a
 * log, and the sudo password is remembered for the session only.
 */
@Component({
  selector: 'app-ssh-prompt',
  template: `
    @if (current(); as item) {
      <div class="ssh-backdrop" data-testid="ssh-popup">
      <div
        class="ssh"
        role="dialog"
        aria-modal="true"
        [attr.aria-label]="title(item)"
        (keydown.escape)="cancel()"
      >
        <div class="ssh-head">
          <span class="dot" aria-hidden="true"></span>
          <h2 class="ssh-title">{{ title(item) }}</h2>
          @if (queue() > 1) {
            <span class="queue">+{{ queue() - 1 }}</span>
          }
        </div>
        <p class="ssh-body">{{ hint(item) }}</p>
        <label class="ssh-field">
          <span>{{ t('ssh.password') }}</span>
          <input
            #password
            type="password"
            autocomplete="off"
            spellcheck="false"
            data-testid="ssh-password"
            [attr.aria-label]="t('ssh.password')"
            [disabled]="item.busy"
            (keydown.enter)="submit()"
          />
        </label>
        @if (item.error) {
          <p class="ssh-error">{{ item.error }}</p>
        }
        <div class="ssh-actions">
          <button
            class="go"
            type="button"
            data-testid="ssh-connect"
            [disabled]="item.busy"
            (click)="submit()"
          >
            {{ item.busy ? t('ssh.connecting') : t('ssh.connect') }}
          </button>
          <button
            class="cancel"
            type="button"
            data-testid="ssh-cancel"
            [disabled]="item.busy"
            (click)="cancel()"
          >
            {{ t('ssh.cancel') }}
          </button>
        </div>
      </div>
      </div>
    }
  `,
  styles: `
    /* The dialog is a viewport overlay, not a block in the transcript: the
       chat is one long scroll container, so an in-flow popup would appear
       below the fold and the user would never see it. */
    :host {
      position: fixed;
      inset: 0;
      z-index: 900;
      display: block;
      pointer-events: none;
    }

    :host(:empty) {
      display: none;
    }

    .ssh-backdrop {
      position: fixed;
      inset: 0;
      display: flex;
      align-items: flex-start;
      justify-content: center;
      padding: 12vh var(--space-16) var(--space-16);
      background: color-mix(in srgb, var(--bg) 72%, transparent);
      pointer-events: auto;
    }

    .ssh {
      width: min(480px, 100%);
      max-height: 80vh;
      overflow: auto;
      border: 1px solid var(--warning);
      border-radius: var(--radius-panel);
      background: var(--surface);
      padding: var(--space-12) var(--space-14);
      display: flex;
      flex-direction: column;
      gap: var(--space-8);
      box-shadow: 0 18px 48px color-mix(in srgb, var(--bg) 60%, transparent);
    }

    .ssh-head {
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

    .ssh-title {
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

    .ssh-body {
      margin: 0;
      font-size: var(--fs-13);
      line-height: 1.55;
      color: var(--text-muted);
    }

    .ssh-field {
      display: flex;
      flex-direction: column;
      gap: 4px;
      font-size: var(--fs-11-5);
      color: var(--text-faint);
    }

    .ssh-field input {
      font-family: var(--font-mono);
      font-size: var(--fs-12-5);
      padding: 6px 9px;
      border-radius: var(--radius-control-sm);
      border: 1px solid var(--border-strong);
      background: var(--bg);
      color: var(--text);
    }

    .ssh-field input:focus {
      outline: none;
      border-color: var(--accent);
    }

    .ssh-error {
      margin: 0;
      font-size: var(--fs-12);
      color: var(--danger);
    }

    .ssh-actions {
      display: flex;
      align-items: center;
      gap: var(--space-8);
    }

    .ssh-actions button {
      font-size: var(--fs-12-5);
      padding: 5px 14px;
      border-radius: var(--radius-control-sm);
    }

    .go {
      background: var(--success);
      border: 1px solid var(--success);
      color: var(--bg);
      font-weight: 600;
    }

    .cancel {
      margin-left: auto;
      background: transparent;
      border: 1px solid var(--border-strong);
      color: var(--text-muted);
    }
  `,
})
export class SshPromptPopup implements OnDestroy {
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly activeSessionID = input<string>();
  /** Project directory of the open chat; prompts are routed by it. */
  readonly directory = input<string>();

  readonly prompts = signal<PendingSshPrompt[]>([]);
  readonly current = () => this.prompts()[0];
  readonly queue = computed(() => this.prompts().length);
  /** True while a password is still expected: the composer must not send. */
  readonly blocking = computed(() => this.prompts().length > 0);

  private readonly passwordInput = viewChild<ElementRef<HTMLInputElement>>('password');
  private readonly unsubscribe: () => void;

  constructor() {
    this.unsubscribe = this.events.onEvent((event: EngineEvent) => this.handle(event));
    effect(() => {
      const id = this.current()?.requestID;
      if (!id) {
        return;
      }
      const input = this.passwordInput()?.nativeElement;
      if (input) {
        input.value = '';
        queueMicrotask(() => input.focus());
      }
    });
  }

  ngOnDestroy(): void {
    this.unsubscribe();
  }

  private handle(event: EngineEvent): void {
    if (event.type !== 'ssh.prompt') {
      return;
    }
    const active = this.activeSessionID();
    const dir = this.directory();
    const matches =
      (!!active && event.sessionID === active) || (!!dir && event.directory === dir);
    if (!matches) {
      return;
    }
    const parsed = parsePrompt(event.properties);
    if (!parsed) {
      return;
    }
    this.prompts.update((list) =>
      list.some((item) => item.requestID === parsed.requestID)
        ? list
        : [
            ...list,
            {
              requestID: parsed.requestID,
              sessionID: parsed.sessionID || event.sessionID,
              host: parsed.host,
              kind: parsed.prompt === 'sudo' ? 'sudo' : 'password',
              askedAt: Date.now(),
              busy: false,
            },
          ],
    );
  }

  /** Dialog title for the current prompt. */
  title(item: PendingSshPrompt): string {
    return item.kind === 'sudo' ? this.t('ssh.sudoTitle') : this.t('ssh.title');
  }

  /** One-line explanation of why the password is needed. */
  hint(item: PendingSshPrompt): string {
    return item.kind === 'sudo'
      ? this.t('ssh.sudoHint', { host: item.host })
      : this.t('ssh.hint', { host: item.host });
  }

  submit(): void {
    const item = this.current();
    if (!item || item.busy) {
      return;
    }
    const input = this.passwordInput()?.nativeElement;
    const password = input?.value ?? '';
    if (!password) {
      input?.focus();
      return;
    }
    void this.send(item, { answer: password });
    if (input) {
      input.value = '';
    }
  }

  cancel(): void {
    const item = this.current();
    if (!item || item.busy) {
      return;
    }
    void this.send(item, { cancel: true });
  }

  private async send(
    item: PendingSshPrompt,
    body: { answer?: string; cancel?: boolean },
  ): Promise<void> {
    this.prompts.update((list) =>
      list.map((entry) =>
        entry.requestID === item.requestID ? { ...entry, busy: true, error: undefined } : entry,
      ),
    );
    try {
      await this.engine.answerSshPrompt(item.requestID, body);
      this.prompts.update((list) => list.filter((entry) => entry.requestID !== item.requestID));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.prompts.update((list) =>
        list.map((entry) =>
          entry.requestID === item.requestID
            ? { ...entry, busy: false, error: this.t('ssh.failed', { error: message }) }
            : entry,
        ),
      );
    }
  }
}
