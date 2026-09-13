/**
 * Read-only live log of one background process (F9-14).
 *
 * Same xterm surface as the PTY tab (palette, font, scrollback) but with
 * input disabled: the body is fed from `GET /processes/{id}/log` (last 64 KB)
 * and then from the `process.output` SSE chunks the `ProcessesStore` relays.
 * xterm keeps the viewport pinned to the bottom while the user has not
 * scrolled up, and leaves it alone once they did - the "follow" behaviour
 * comes for free. The header carries the status, "Stop" (kill the process
 * tree; disabled once exited) and "Open URL" when a local url was detected.
 */

import {
  AfterViewInit,
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  OnDestroy,
  computed,
  effect,
  inject,
  input,
  signal,
  viewChild,
} from '@angular/core';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { Subscription } from 'rxjs';

import { EngineClient } from '../../core/engine-client.service';
import { ProcessInfo } from '../../core/engine.dtos';
import { ProcessesStore, formatUptime, processTone } from '../../core/processes.store';
import { I18nService } from '../../i18n/i18n.service';
import { createXterm } from './terminal-tab';

/** Bytes of log requested when a tab opens. */
export const LOG_TAIL_BYTES = 64 * 1024;

@Component({
  selector: 'app-process-log-tab',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="log-head">
      <span class="dot" [class]="'dot tone-' + tone()" aria-hidden="true"></span>
      <span class="log-cmd" [title]="process()?.command ?? ''">{{ process()?.command ?? processId() }}</span>
      @if (process(); as p) {
        <span class="log-meta">
          <span class="agent">{{ p.agent }}</span>
          @if (p.status === 'running') {
            <span>{{ t('processes.running') }} · {{ uptime() }}</span>
          } @else if (p.exit_code !== null && p.exit_code !== undefined) {
            <span>{{ t('processes.exited') }} · {{ t('processes.exitCode', { code: p.exit_code }) }}</span>
          } @else {
            <span>{{ t('processes.exited') }}</span>
          }
        </span>
      }
      <span class="spacer"></span>
      @if (process()?.url; as url) {
        <button type="button" class="head-btn" (click)="openUrl(url)" [title]="url">
          {{ t('processes.openUrl') }}
        </button>
      }
      <button
        type="button"
        class="head-btn stop"
        (click)="stop()"
        [disabled]="!process() || process()!.status !== 'running' || stopping()"
        [title]="t('processes.stop')"
      >
        {{ t('processes.stop') }}
      </button>
    </div>
    @if (error()) {
      <div class="log-error">{{ error() }}</div>
    }
    <div class="xterm-screen" #screen></div>
  `,
  styles: [
    `
      :host {
        display: flex;
        flex-direction: column;
        height: 100%;
        min-height: 0;
      }
      .log-head {
        flex: none;
        display: flex;
        align-items: center;
        gap: var(--space-8);
        padding: 0 0 var(--space-8);
        margin-bottom: var(--space-8);
        border-bottom: 1px solid var(--border);
        font-family: var(--font-sans);
        font-size: var(--fs-12);
        color: var(--code-text);
        min-width: 0;
      }
      .dot {
        width: 7px;
        height: 7px;
        flex: none;
        border-radius: 50%;
        background: var(--text-faint);
      }
      .dot.tone-running {
        background: var(--success);
      }
      .dot.tone-failed {
        background: var(--danger);
      }
      .log-cmd {
        font-family: var(--font-mono);
        color: var(--code-text-strong);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        min-width: 0;
        max-width: 50%;
      }
      .log-meta {
        display: inline-flex;
        gap: var(--space-8);
        color: var(--text-faint);
        white-space: nowrap;
      }
      .log-meta .agent {
        color: var(--accent);
      }
      .spacer {
        flex: 1 1 auto;
      }
      .head-btn {
        flex: none;
        padding: 3px 10px;
        font-size: var(--fs-11-5);
        background: transparent;
        border: 1px solid var(--border);
        border-radius: var(--radius-control-sm);
        color: var(--text-muted);
      }
      .head-btn:hover:not(:disabled) {
        color: var(--accent);
        border-color: var(--accent);
      }
      .head-btn.stop:hover:not(:disabled) {
        color: var(--danger);
        border-color: var(--danger);
      }
      .head-btn:disabled {
        opacity: 0.5;
      }
      .log-error {
        flex: none;
        margin-bottom: var(--space-8);
        font-family: var(--font-sans);
        font-size: var(--fs-12);
        color: var(--danger);
      }
      .xterm-screen {
        flex: 1 1 auto;
        min-height: 0;
        width: 100%;
      }
    `,
  ],
})
export class ProcessLogTab implements AfterViewInit, OnDestroy {
  readonly processId = input.required<string>();
  /** Ticking clock for the uptime label (owned by the Terminal screen). */
  readonly now = input<number>(Date.now());

  private readonly screen = viewChild.required<ElementRef<HTMLElement>>('screen');

  private readonly engine = inject(EngineClient);
  private readonly store = inject(ProcessesStore);
  private readonly i18n = inject(I18nService);
  private readonly destroyRef = inject(DestroyRef);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly process = computed<ProcessInfo | null>(() => this.store.byId(this.processId()));
  readonly error = signal<string | null>(null);
  readonly stopping = signal(false);

  readonly tone = computed(() => {
    const p = this.process();
    return p ? processTone(p) : 'unknown';
  });

  readonly uptime = computed(() => {
    const p = this.process();
    return p ? formatUptime(p.started_at, p.status === 'running' ? this.now() : p.ended_at ?? this.now()) : '';
  });

  private term?: Terminal;
  private fit?: FitAddon;
  private resizeObserver?: ResizeObserver;
  private chunks?: Subscription;
  private terminated = false;
  /** Chunks that stream in before the snapshot arrived (replayed after it). */
  private pending: string[] = [];
  private seeded = false;
  private openedFor: string | null = null;

  constructor() {
    // A new process id on the same instance: reset the surface and re-seed.
    effect(() => {
      const id = this.processId();
      if (this.term && this.openedFor !== null && this.openedFor !== id) {
        this.term.reset();
        this.seeded = false;
        this.pending = [];
        this.openedFor = id;
        void this.seed(id);
      }
    });
  }

  ngAfterViewInit(): void {
    const term = createXterm({ disableStdin: true, cursorBlink: false, convertEol: true });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(this.screen().nativeElement);
    this.term = term;
    this.fit = fit;
    this.resizeObserver = new ResizeObserver(() => this.fit?.fit());
    this.resizeObserver.observe(this.screen().nativeElement);
    fit.fit();

    this.chunks = this.store.chunks$.subscribe((c) => {
      if (c.id !== this.processId()) {
        return;
      }
      if (this.seeded) {
        this.write(c.chunk);
      } else {
        this.pending.push(c.chunk);
      }
    });
    this.destroyRef.onDestroy(() => this.chunks?.unsubscribe());

    this.openedFor = this.processId();
    void this.seed(this.processId());
  }

  ngOnDestroy(): void {
    this.terminated = true;
    this.resizeObserver?.disconnect();
    this.chunks?.unsubscribe();
    this.term?.dispose();
  }

  async stop(): Promise<void> {
    const p = this.process();
    if (!p || p.status !== 'running' || this.stopping()) {
      return;
    }
    this.stopping.set(true);
    this.error.set(null);
    try {
      await this.store.kill(p.id);
    } catch (err) {
      this.error.set(err instanceof Error ? err.message : String(err));
    } finally {
      this.stopping.set(false);
    }
  }

  openUrl(url: string): void {
    window.open(url, '_blank', 'noopener');
  }

  private async seed(id: string): Promise<void> {
    this.error.set(null);
    try {
      const res = await this.engine.processLog(id, LOG_TAIL_BYTES);
      if (this.terminated || id !== this.processId()) {
        return;
      }
      this.store.seed(id, res.log);
      this.write(res.log);
    } catch (err) {
      if (this.terminated || id !== this.processId()) {
        return;
      }
      // No snapshot (older engine / log gone): fall back to what streamed in.
      this.error.set(err instanceof Error ? err.message : String(err));
      // The buffer already holds the chunks queued in `pending`.
      this.pending = [];
      this.write(this.store.output(id)());
    } finally {
      if (!this.terminated && id === this.processId()) {
        this.seeded = true;
        for (const chunk of this.pending) {
          this.write(chunk);
        }
        this.pending = [];
      }
    }
  }

  private write(text: string): void {
    if (!text || !this.term) {
      return;
    }
    this.term.write(text);
  }
}
