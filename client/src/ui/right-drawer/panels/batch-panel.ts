/**
 * Right-drawer "Batch" panel (OpenRouter Batch API, option B).
 *
 * Lists the async bulk jobs of the open session (`GET /session/{id}/batches`):
 * live progress from `batch.progress` events, per-item results once a batch
 * completes, and cancel for running jobs. Refreshes on every `batch.*` event
 * via the shared {@link BatchStore}.
 */

import { ChangeDetectionStrategy, Component, computed, inject } from '@angular/core';

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

  readonly batches = computed<BatchRecord[]>(() => this.store.batches());
  readonly loading = this.store.loading;
  readonly error = this.store.error;

  readonly sessionId = computed(() => this.session?.meta()?.id ?? this.store.sessionId());

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
