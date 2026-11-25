import {
  Component,
  ElementRef,
  computed,
  effect,
  inject,
  signal,
  viewChild,
} from '@angular/core';

import type { CompactPreviewResponse } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';

/**
 * Window event the command palette dispatches to request "compact the open
 * session" (`detail: { sessionID }`). The chat view handles it; it is the only
 * handler, so the event is a no-op when no chat view is mounted.
 */
export const COMPACT_REQUESTED_EVENT = 'bebok:compact-requested';

export type CompactConfirmMode = 'manual' | 'suggest';
export type CompactConfirmOutcome = 'compact' | 'cancel' | 'send-without';

/** Token count as `42k` / `1.2M` / `950` (matches the toolbar meter). */
export function formatCompactTokens(n: number): string {
  if (n >= 1_000_000) {
    return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, '')}M`;
  }
  if (n >= 1_000) {
    return `${Math.round(n / 1_000)}k`;
  }
  return String(Math.round(n));
}

/**
 * Compaction confirmation (modal). `open(preview, mode)` shows what a
 * compaction would do and resolves with the user's choice:
 *
 * - `compact`: fork the session and summarize the older messages;
 * - `cancel`: do nothing (Escape, backdrop, Anuluj or a dismissed dialog);
 * - `send-without`: skip compaction and send the queued prompt (suggest mode only).
 *
 * Only one confirmation is open at a time: opening a new one resolves the
 * previous one as `cancel`.
 */
@Component({
  selector: 'app-compact-confirm-dialog',
  templateUrl: './compact-confirm-dialog.html',
  styleUrl: './compact-confirm-dialog.css',
})
export class CompactConfirmDialog {
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);
  readonly formatTokens = formatCompactTokens;

  /** Preview on screen; `null` = dialog closed. */
  readonly preview = signal<CompactPreviewResponse | null>(null);
  readonly mode = signal<CompactConfirmMode>('manual');

  /** Messages at index < cutoff are summarized, the rest are kept. */
  readonly summarizedCount = computed(() => this.preview()?.cutoff ?? 0);
  readonly keptCount = computed(() => this.preview()?.kept ?? 0);
  readonly summaryText = computed(() =>
    this.t('compact.summary', { summarized: this.summarizedCount(), kept: this.keptCount() }),
  );
  readonly titleText = computed(() => {
    const p = this.preview();
    if (!p) {
      return '';
    }
    return this.t('compact.title', {
      before: formatCompactTokens(p.before),
      after: formatCompactTokens(p.after),
    });
  });

  private resolver: ((outcome: CompactConfirmOutcome) => void) | null = null;

  private readonly dialog = viewChild<ElementRef<HTMLElement>>('dialog');

  constructor() {
    // Focus the primary action when a confirmation opens, for keyboard users.
    effect(() => {
      if (!this.preview()) {
        return;
      }
      queueMicrotask(() => {
        this.dialog()?.nativeElement.querySelector<HTMLButtonElement>('.primary')?.focus();
      });
    });
  }

  /** Show the preview; resolves with the user's choice. */
  open(preview: CompactPreviewResponse, mode: CompactConfirmMode = 'manual'): Promise<CompactConfirmOutcome> {
    this.resolve('cancel');
    return new Promise<CompactConfirmOutcome>((resolve) => {
      this.resolver = resolve;
      this.mode.set(mode);
      this.preview.set(preview);
    });
  }

  /** Close without a choice (e.g. the view navigated away). Resolves `cancel`. */
  close(): void {
    this.resolve('cancel');
  }

  choose(outcome: CompactConfirmOutcome): void {
    this.resolve(outcome);
  }

  onKeydown(event: KeyboardEvent): void {
    if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      this.choose('cancel');
    }
  }

  isSummarized(index: number): boolean {
    return index < this.summarizedCount();
  }

  private resolve(outcome: CompactConfirmOutcome): void {
    const pending = this.resolver;
    this.resolver = null;
    this.preview.set(null);
    pending?.(outcome);
  }
}
