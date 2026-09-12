/**
 * Right-drawer "Terminal" panel (F2-14).
 *
 * A compact PTY view on the `--terminal-bg` block. The PTY plumbing is not
 * re-implemented: this renders the full-screen Terminal's `TerminalTab`
 * (one xterm instance bound to one engine PTY, blinking cursor included) at
 * drawer size. Closing the panel only detaches the client - the PTY keeps
 * running in the engine, exactly as the full-screen view behaves.
 */

import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from '@angular/core';

import { EngineClient } from '../../../core/engine-client.service';
import { I18nService } from '../../../i18n/i18n.service';
import { ChatSessionStore } from '../../../views/chat/chat-session.store';
import { TerminalTab, type TerminalTabStatus } from '../../../views/terminal/terminal-tab';

interface PtyOption {
  ptyId: string;
  title: string;
  exited: boolean;
}

@Component({
  selector: 'app-terminal-panel',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [TerminalTab],
  template: `
    <div class="panel-body">
      <div class="head">
        @if (ptys().length > 1) {
          <select
            class="pty-select"
            [value]="activePtyId() ?? ''"
            (change)="select($any($event.target).value)"
            [attr.aria-label]="t('drawer.terminalSession')"
          >
            @for (pty of ptys(); track pty.ptyId) {
              <option [value]="pty.ptyId">{{ pty.title }}</option>
            }
          </select>
        } @else if (activePtyId()) {
          <span class="pty-name">{{ activeTitle() }}</span>
        }
        <span class="status">
          <span class="dot status-{{ status() }}" aria-hidden="true"></span>{{ status() }}
        </span>
        <button
          type="button"
          class="new"
          (click)="createTerminal()"
          [disabled]="!directory() || creating()"
          [title]="t('drawer.newTerminal')"
        >+</button>
      </div>

      @if (error()) {
        <div class="error">{{ error() }}</div>
      }

      @if (activePtyId(); as ptyId) {
        <div class="screen">
          <app-terminal-tab [ptyId]="ptyId" (status)="status.set($event)" />
        </div>
      } @else {
        <div class="empty">{{ t('drawer.noTerminal') }}</div>
      }
    </div>
  `,
  styles: [
    `
      .panel-body {
        display: flex;
        flex-direction: column;
      }

      .head {
        display: flex;
        align-items: center;
        gap: var(--space-6);
        padding: var(--space-6) var(--space-10);
        border-bottom: 1px solid var(--border);
      }

      .pty-select,
      .pty-name {
        flex: 1 1 auto;
        min-width: 0;
        font-family: var(--font-mono);
        font-size: var(--fs-11);
        color: var(--text-muted);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .pty-select {
        background: var(--surface-2);
        border: 1px solid var(--border);
        border-radius: var(--radius-control-sm);
        padding: 2px 4px;
      }

      .status {
        display: inline-flex;
        align-items: center;
        gap: 4px;
        flex: none;
        font-size: 10px;
        color: var(--text-faint);
      }

      .dot {
        width: 6px;
        height: 6px;
        border-radius: 50%;
        background: var(--text-faint);
      }
      .dot.status-live {
        background: var(--success);
      }
      .dot.status-connecting {
        background: var(--warning);
      }
      .dot.status-error {
        background: var(--danger);
      }

      .new {
        flex: none;
        padding: 1px 7px;
        font-size: var(--fs-12);
        line-height: 1.3;
        background: transparent;
        border: 1px solid var(--border);
        border-radius: var(--radius-control-sm);
        color: var(--text-muted);
      }

      .new:hover:not(:disabled) {
        color: var(--accent);
        border-color: var(--accent);
      }

      .screen {
        height: 200px;
        margin: var(--space-8) var(--space-10) var(--space-10);
        padding: var(--space-6);
        background: var(--terminal-bg);
        border: 1px solid var(--border);
        border-radius: var(--radius-control-sm);
        overflow: hidden;
      }

      .empty,
      .error {
        padding: var(--space-12) var(--space-14);
        font-size: var(--fs-11-5);
        color: var(--text-faint);
      }

      .error {
        color: var(--danger);
        overflow-wrap: anywhere;
      }
    `,
  ],
})
export class TerminalPanel {
  private readonly i18n = inject(I18nService);
  private readonly engine = inject(EngineClient);
  private readonly session = inject(ChatSessionStore);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly directory = computed(
    () => this.session.directory() ?? this.engine.readLastDirectory(),
  );

  readonly ptys = signal<PtyOption[]>([]);
  readonly activePtyId = signal<string | null>(null);
  readonly status = signal<TerminalTabStatus>('connecting');
  readonly creating = signal(false);
  readonly error = signal<string | null>(null);

  readonly activeTitle = computed(
    () => this.ptys().find((p) => p.ptyId === this.activePtyId())?.title ?? '',
  );

  constructor() {
    // The panel is only instantiated while it is open, so a single refresh on
    // creation is enough to attach to whatever the engine already runs.
    effect(() => {
      this.directory();
      void this.refresh();
    });
  }

  select(ptyId: string): void {
    this.status.set('connecting');
    this.activePtyId.set(ptyId);
  }

  async refresh(): Promise<void> {
    try {
      const list = await this.engine.listPtys();
      const options = list.map((p) => ({
        ptyId: p.pty_id,
        title: p.title ?? p.command,
        exited: p.exited,
      }));
      this.ptys.set(options);
      if (!this.activePtyId() || !options.some((p) => p.ptyId === this.activePtyId())) {
        const live = options.find((p) => !p.exited) ?? options[0];
        this.activePtyId.set(live?.ptyId ?? null);
      }
    } catch (err) {
      this.error.set(describe(err));
    }
  }

  async createTerminal(): Promise<void> {
    const dir = this.directory();
    if (!dir || this.creating()) {
      return;
    }
    this.creating.set(true);
    this.error.set(null);
    try {
      const created = await this.engine.createPty(dir);
      await this.refresh();
      this.select(created.ptyId);
    } catch (err) {
      this.error.set(describe(err));
    } finally {
      this.creating.set(false);
    }
  }
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
