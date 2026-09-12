/**
 * WP-DELEGATION (F8-2): one-line "what is this sub-agent doing" strip.
 *
 * Pure presentational: `status` + `progress` + `tokens` in, one dense line
 * out - `● write_file · Adding the About route · 4 calls · 12.3k tok`. Used
 * by the chat's `task`/`fleet` tool rows (live sub-row while the call runs),
 * the Agents drawer panel and the transcript overlay header, so the three
 * places read the same way.
 */

import { ChangeDetectionStrategy, Component, computed, inject, input } from '@angular/core';

import { TaskProgress } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';

@Component({
  selector: 'app-task-progress-line',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <span class="line" [attr.data-status]="status()" data-testid="task-progress-line">
      <span class="dot" [class.pulse]="live()" aria-hidden="true"></span>
      @if (status() === 'queued') {
        <span class="tool">{{ t('agents.queuedHint') }}</span>
      } @else if (progress()?.lastTool; as tool) {
        <span class="tool" [class.err]="progress()?.lastToolState === 'error'">{{ tool }}</span>
      } @else if (live()) {
        <span class="tool muted">{{ t('agents.thinking') }}</span>
      }
      @if (summary()) {
        <span class="sep" aria-hidden="true">·</span>
        <span class="summary" [title]="summary()">{{ summary() }}</span>
      }
      @if (calls() > 0) {
        <span class="sep" aria-hidden="true">·</span>
        <span class="meta">{{ t('agents.calls', { n: calls() }) }}</span>
      }
      @if (tokens() > 0) {
        <span class="sep" aria-hidden="true">·</span>
        <span class="meta">{{ formatTokens(tokens()) }} {{ t('agents.tok') }}</span>
      }
    </span>
  `,
  styles: [
    `
      :host {
        display: block;
        min-width: 0;
      }
      .line {
        display: flex;
        align-items: center;
        gap: var(--space-6);
        min-width: 0;
        font-size: var(--fs-11);
        color: var(--text-muted);
        line-height: 1.4;
      }
      .dot {
        flex: none;
        width: 6px;
        height: 6px;
        border-radius: 50%;
        background: var(--text-faint);
      }
      .line[data-status='running'] .dot {
        background: var(--accent);
      }
      .line[data-status='queued'] .dot {
        background: var(--warning);
      }
      .line[data-status='done'] .dot {
        background: var(--success);
      }
      .line[data-status='failed'] .dot,
      .line[data-status='aborted'] .dot {
        background: var(--danger);
      }
      .dot.pulse {
        animation: task-progress-pulse 1.4s ease-in-out infinite;
      }
      @keyframes task-progress-pulse {
        0%,
        100% {
          opacity: 1;
        }
        50% {
          opacity: 0.3;
        }
      }
      .tool {
        flex: none;
        font-family: var(--font-mono);
        color: var(--text);
      }
      .tool.err {
        color: var(--danger);
      }
      .tool.muted {
        color: var(--text-faint);
        font-family: inherit;
        font-style: italic;
      }
      .sep {
        flex: none;
        color: var(--text-faint);
      }
      .summary {
        flex: 1 1 auto;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .meta {
        flex: none;
        color: var(--text-faint);
        font-variant-numeric: tabular-nums;
      }
    `,
  ],
})
export class TaskProgressLine {
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly status = input<string>('running');
  readonly progress = input<TaskProgress | null | undefined>(null);
  /** Input + output tokens of the child so far. */
  readonly tokens = input<number>(0);

  readonly live = computed(() => this.status() === 'running' || this.status() === 'queued');
  readonly summary = computed(() => this.progress()?.summary ?? '');
  readonly calls = computed(() => this.progress()?.toolCalls ?? 0);

  formatTokens(n: number): string {
    if (n >= 1_000_000) {
      return `${(n / 1_000_000).toFixed(1)}M`;
    }
    if (n >= 1_000) {
      return `${(n / 1_000).toFixed(1)}k`;
    }
    return String(n);
  }
}
