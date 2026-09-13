/**
 * Read-only process list + log (WP-M6 / F10-26).
 *
 * The desktop's Terminal panel drives PTYs and can kill processes; neither
 * is allow-listed for the remote scope (`POST /processes/{id}/kill` -> 403),
 * so the phone renders the `ProcessesStore` list with a status dot, uptime
 * and the detected URL, and a tap opens the log: `GET /processes/{id}/log`
 * for the tail, then live `process.output` chunks (coalesced by the engine
 * to the last 20 lines per second for remote streams) appended from the
 * store's buffer. No kill, no input, no xterm.
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  computed,
  effect,
  inject,
  signal,
  untracked,
  viewChild,
} from '@angular/core';

import { EngineClient } from '../../../core/engine-client.service';
import { ProcessInfo } from '../../../core/engine.dtos';
import { ProcessesStore, formatUptime, processTone } from '../../../core/processes.store';
import { I18nService } from '../../../i18n/i18n.service';
import { ChatSessionStore } from '../../chat/chat-session.store';

const LOG_TAIL_BYTES = 64 * 1024;

@Component({
  selector: 'app-mobile-processes',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (selected(); as proc) {
      <section class="log-view" data-testid="process-log">
        <header class="log-head">
          <button type="button" class="back" (click)="close()" data-testid="process-log-back">
            ‹ {{ t('mobile.processes.back') }}
          </button>
          <span class="log-cmd" [title]="proc.command">{{ proc.command }}</span>
          <span class="status-dot" [attr.data-tone]="tone(proc)" aria-hidden="true"></span>
        </header>
        @if (logError(); as err) {
          <p class="error">{{ err }}</p>
        }
        <pre class="log" #logEl data-testid="process-log-text">{{ logText() }}</pre>
      </section>
    } @else {
      @if (store.error(); as err) {
        <p class="error" data-testid="processes-error">{{ err }}</p>
      }
      @if (store.processes().length === 0) {
        <p class="empty" data-testid="processes-empty">
          {{ store.loading() ? t('mobile.remote.loading') : t('mobile.processes.none') }}
        </p>
      } @else {
        <ul class="procs" data-testid="processes-list">
          @for (proc of store.processes(); track proc.id) {
            <li>
              <button
                type="button"
                class="proc"
                (click)="open(proc)"
                [attr.data-testid]="'process-' + proc.id"
                [attr.data-status]="proc.status"
              >
                <span class="status-dot" [attr.data-tone]="tone(proc)" aria-hidden="true"></span>
                <span class="main">
                  <span class="cmd">{{ proc.command }}</span>
                  <span class="meta">
                    <span>{{ proc.agent }}</span>
                    <span>· {{ proc.status === 'running' ? uptime(proc) : exitLabel(proc) }}</span>
                    @if (proc.url) {
                      <span class="url">· {{ proc.url }}</span>
                    }
                  </span>
                </span>
              </button>
            </li>
          }
        </ul>
      }
    }
  `,
  styles: [
    `
      :host {
        display: flex;
        flex-direction: column;
        flex: 1 1 auto;
        min-height: 0;
      }

      .empty,
      .error {
        margin: 0;
        padding: var(--space-16);
        font-size: var(--fs-12);
        color: var(--text-muted);
        text-align: center;
      }

      .error {
        color: var(--danger);
        overflow-wrap: anywhere;
      }

      .procs {
        list-style: none;
        margin: 0;
        padding: 0;
      }

      .proc {
        display: flex;
        align-items: center;
        gap: var(--space-10);
        width: 100%;
        min-height: 56px;
        padding: var(--space-8) var(--space-16);
        border: 0;
        border-bottom: 1px solid var(--border);
        background: transparent;
        color: var(--text);
        font: inherit;
        text-align: left;
        cursor: pointer;
      }

      .status-dot {
        flex: none;
        width: 8px;
        height: 8px;
        border-radius: 50%;
        background: var(--border-strong);
      }

      .status-dot[data-tone='running'] {
        background: var(--success);
      }

      .status-dot[data-tone='failed'] {
        background: var(--danger);
      }

      .main {
        flex: 1 1 auto;
        min-width: 0;
        display: flex;
        flex-direction: column;
        gap: 2px;
      }

      .cmd {
        font-family: var(--font-mono);
        font-size: var(--fs-12-5);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .meta {
        display: flex;
        gap: var(--space-4);
        font-size: var(--fs-11-5);
        color: var(--text-faint);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .url {
        color: var(--accent);
      }

      .log-view {
        display: flex;
        flex-direction: column;
        flex: 1 1 auto;
        min-height: 0;
      }

      .log-head {
        flex: none;
        display: flex;
        align-items: center;
        gap: var(--space-8);
        padding: var(--space-6) var(--space-12);
        border-bottom: 1px solid var(--border);
        background: var(--surface);
      }

      .back {
        flex: none;
        min-height: 36px;
        padding: 0 var(--space-8);
        border: 0;
        background: transparent;
        color: var(--accent);
        font: inherit;
        cursor: pointer;
      }

      .log-cmd {
        flex: 1 1 auto;
        min-width: 0;
        font-family: var(--font-mono);
        font-size: var(--fs-12);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .log {
        flex: 1 1 auto;
        min-height: 0;
        margin: 0;
        padding: var(--space-10) var(--space-12);
        overflow: auto;
        font-family: var(--font-mono);
        font-size: var(--fs-11-5);
        line-height: 1.45;
        white-space: pre-wrap;
        overflow-wrap: anywhere;
        color: var(--code-text-strong);
        background: var(--bg);
      }
    `,
  ],
})
export class MobileProcesses {
  readonly store = inject(ProcessesStore);
  private readonly engine = inject(EngineClient);
  private readonly session = inject(ChatSessionStore);
  private readonly i18n = inject(I18nService);
  private readonly destroyRef = inject(DestroyRef);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly selectedId = signal<string | null>(null);
  readonly selected = computed<ProcessInfo | null>(() => {
    const id = this.selectedId();
    return id ? this.store.byId(id) : null;
  });
  readonly logError = signal<string | null>(null);
  /** Live buffer of the selected process (seeded from the log file). */
  readonly logText = computed(() => {
    const id = this.selectedId();
    return id ? this.store.output(id)() : '';
  });

  private readonly logEl = viewChild<ElementRef<HTMLElement>>('logEl');
  private readonly now = signal(Date.now());
  private ticker: ReturnType<typeof setInterval> | null = null;

  constructor() {
    effect(() => {
      const id = this.session.meta()?.id ?? null;
      this.session.running();
      untracked(() => {
        if (id) {
          void this.store.load(id);
        } else {
          this.store.clear();
        }
      });
    });
    // Follow the log tail.
    effect(() => {
      this.logText();
      const el = this.logEl()?.nativeElement;
      if (el) {
        requestAnimationFrame(() => (el.scrollTop = el.scrollHeight));
      }
    });
    this.ticker = setInterval(() => this.now.set(Date.now()), 1000);
    this.destroyRef.onDestroy(() => {
      if (this.ticker) {
        clearInterval(this.ticker);
      }
    });
  }

  tone(proc: ProcessInfo): string {
    return processTone(proc);
  }

  uptime(proc: ProcessInfo): string {
    return formatUptime(proc.started_at, this.now());
  }

  exitLabel(proc: ProcessInfo): string {
    return this.t('mobile.processes.exited', { code: String(proc.exit_code ?? 0) });
  }

  async open(proc: ProcessInfo): Promise<void> {
    this.selectedId.set(proc.id);
    this.logError.set(null);
    try {
      const res = await this.engine.processLog(proc.id, LOG_TAIL_BYTES);
      if (this.selectedId() === proc.id) {
        this.store.seed(proc.id, res.log);
      }
    } catch (err) {
      if (this.selectedId() === proc.id) {
        // No snapshot: fall back to what streamed in.
        this.logError.set(err instanceof Error ? err.message : String(err));
      }
    }
  }

  close(): void {
    this.selectedId.set(null);
  }
}
