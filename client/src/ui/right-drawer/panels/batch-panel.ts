/**
 * Right-drawer "Batch" panel (OpenRouter Batch API, option B).
 *
 * Lists the async bulk jobs of the open session (`GET /session/{id}/batches`):
 * live progress from `batch.progress` events, per-item results once a batch
 * completes, and cancel for running jobs. Refreshes on every `batch.*` event
 * via the shared {@link BatchStore}.
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  untracked,
} from '@angular/core';

import { BatchRecord } from '../../../core/engine.dtos';
import { BatchStore } from '../../../core/batch.store';
import { I18nService } from '../../../i18n/i18n.service';
import { MessageKey } from '../../../i18n/index';
import { ChatSessionStore } from '../../../views/chat/chat-session.store';

/** Batch state -> i18n key (t() needs a literal MessageKey, no concatenation). */
const STATE_KEYS: Record<string, MessageKey> = {
  Created: 'batch.status.created',
  Uploading: 'batch.status.uploading',
  Submitted: 'batch.status.submitted',
  Polling: 'batch.status.polling',
  Completed: 'batch.status.completed',
  Failed: 'batch.status.failed',
  Cancelled: 'batch.status.cancelled',
};

/**
 * Refresh cadence for the open panel while a job is still running. A batch API
 * job sits in "processing" for minutes — sometimes tens of minutes — and the
 * engine only learns about it on its own 30 s poll, so an SSE event can be
 * missed (drawer opened late, stream reconnect). Polling the record keeps the
 * panel honest without a manual refresh.
 */
const REFRESH_INTERVAL_MS = 15_000;

@Component({
  selector: 'app-batch-panel',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (!sessionId()) {
      <div class="empty">{{ t('drawer.noSession') }}</div>
    } @else {
      <div class="panel-body">
        <div class="head">
          <span class="count">{{ t('batch.count', { n: batches().length }) }}</span>
          <button
            type="button"
            class="refresh"
            (click)="refresh()"
            [disabled]="loading()"
            [title]="t('batch.refresh')"
            [attr.aria-label]="t('batch.refresh')"
            data-testid="batch-refresh"
          >
            ↻
          </button>
        </div>

        @if (error()) {
          <div class="error">{{ error() }}</div>
        } @else if (batches().length === 0) {
          <div class="none">{{ loading() ? t('batch.loading') : t('batch.empty') }}</div>
        } @else {
          <ul class="batches">
            @for (batch of batches(); track batch.id) {
              <li class="batch" data-testid="batch-row" [attr.data-batch]="batch.id">
                <div class="batch-head">
                  <span class="batch-model">{{ batch.model }}</span>
                  <span class="batch-state" [attr.data-state]="batch.state">{{
                    stateLabel(batch)
                  }}</span>
                </div>
                <div class="batch-meta">
                  <span>{{ batch.items.length }} items</span>
                  @if (progressOf(batch.id); as p) {
                    <span>{{ p.done }}/{{ p.total }}</span>
                  }
                  <span class="batch-age">{{ elapsed(batch) }}</span>
                  @if (!isTerminal(batch)) {
                    <button
                      type="button"
                      class="cancel"
                      (click)="cancel(batch)"
                      data-testid="batch-cancel"
                      [attr.data-batch]="batch.id"
                    >
                      {{ t('batch.action.cancel') }}
                    </button>
                  }
                </div>
                @if (batch.error) {
                  <div class="error">{{ batch.error }}</div>
                }
                @if (isDone(batch)) {
                  <ul class="items">
                    @for (item of batch.items; track item.index) {
                      <li class="item" [class.failed]="item.status === 'Failed'">
                        <span class="item-index">#{{ item.index }}</span>
                        @if (item.status === 'Done') {
                          <span class="item-result">{{ item.result }}</span>
                        } @else {
                          <span class="item-error">{{ item.error ?? item.status }}</span>
                        }
                      </li>
                    }
                  </ul>
                }
              </li>
            }
          </ul>
        }
      </div>
    }
  `,
  styles: [
    '.panel-body{display:flex;flex-direction:column;gap:8px;padding:8px}',
    '.head{display:flex;align-items:center;justify-content:space-between}',
    '.batches{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:8px}',
    '.batch{border:1px solid var(--border);border-radius:6px;padding:8px}',
    '.batch-head{display:flex;justify-content:space-between;gap:8px}',
    '.batch-model{font-family:monospace;overflow:hidden;text-overflow:ellipsis}',
    '.batch-meta{display:flex;gap:12px;align-items:center;color:var(--muted)}',
    '.items{list-style:none;margin:8px 0 0;padding:0;display:flex;flex-direction:column;gap:4px}',
    '.item-result{white-space:pre-wrap}',
    '.item.failed{color:var(--danger)}',
    '.error{color:var(--danger)}',
    '.none,.empty{color:var(--muted)}',
  ],
})
export class BatchPanel {
  private readonly i18n = inject(I18nService);
  private readonly store = inject(BatchStore);
  private readonly session = inject(ChatSessionStore, { optional: true });
  readonly t = this.i18n.t.bind(this.i18n);

  /** Session whose list this instance already started loading. */
  private loadedFor: string | null = null;
  private timer: number | undefined;

  readonly batches = computed<BatchRecord[]>(() => this.store.batches());
  readonly loading = this.store.loading;
  readonly error = this.store.error;

  readonly sessionId = computed(() => this.session?.meta()?.id ?? this.store.sessionId());

  constructor() {
    // The panel is the only consumer of the batch list, and the drawer
    // destroys it while the pill is off — so it loads the list itself from
    // the session the chat view publishes instead of relying on a caller.
    effect(() => {
      const meta = this.session?.meta();
      if (!meta?.id || !meta.directory || meta.id === this.loadedFor) {
        return;
      }
      untracked(() => {
        this.loadedFor = meta.id;
        if (this.store.sessionId() === meta.id && this.store.directory() === meta.directory) {
          // Re-opened panel on a session the store already knows: refresh so
          // anything that changed while the panel was closed shows up.
          void this.store.reload();
        } else {
          this.store.load(meta.id, meta.directory);
        }
      });
    });

    // A batch API job runs for minutes and its progress event can be missed,
    // so a running job is re-read on a timer until it reaches a terminal
    // state — otherwise a finished job keeps looking "running 0/1".
    this.timer = window.setInterval(() => {
      if (this.loadedFor && this.batches().some((b) => !this.isTerminal(b))) {
        void this.store.reload();
      }
    }, REFRESH_INTERVAL_MS);
    inject(DestroyRef).onDestroy(() => window.clearInterval(this.timer));
  }

  stateLabel(batch: BatchRecord): string {
    const key = STATE_KEYS[batch.state] ?? 'batch.status.submitted';
    return this.t(key);
  }

  progressOf(id: string): { done: number; total: number } | null {
    return this.store.batchProgress(id)();
  }

  isTerminal(batch: BatchRecord): boolean {
    return ['Completed', 'Failed', 'Cancelled'].includes(batch.state);
  }

  /**
   * How long the job has been with the provider (`1m 04s`), frozen once it is
   * terminal. Batch jobs routinely run for minutes with no intermediate
   * counts, so the age is what tells the user it is progressing at all.
   */
  elapsed(batch: BatchRecord): string {
    const start = Date.parse(batch.created_at);
    if (!Number.isFinite(start)) {
      return '';
    }
    const finished = this.isTerminal(batch) ? Date.parse(batch.updated_at) : Number.NaN;
    const end = Number.isFinite(finished) ? finished : Date.now();
    const seconds = Math.max(0, Math.round((end - start) / 1000));
    const minutes = Math.floor(seconds / 60);
    return minutes > 0 ? `${minutes}m ${String(seconds % 60).padStart(2, '0')}s` : `${seconds}s`;
  }

  isDone(batch: BatchRecord): boolean {
    return batch.state === 'Completed';
  }

  refresh(): void {
    void this.store.reload();
  }

  cancel(batch: BatchRecord): void {
    void this.store.cancel(batch.id);
  }
}
