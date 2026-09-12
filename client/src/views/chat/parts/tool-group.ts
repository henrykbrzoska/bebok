import { Component, inject, input, output } from '@angular/core';

import { Part, ToolStateKind } from '../../../core/engine.dtos';
import { I18nService } from '../../../i18n/i18n.service';
import { PartRendererComponent } from './part-renderer';

/** One part plus the info the renderer needs to pick its default state. */
export interface RenderedPart {
  kind: 'part';
  part: Part;
  /** 0-based index of this part among the tool parts of the same message
   *  (or, inside a merged run, among the tool parts of the whole run). */
  toolIndex: number;
}

/**
 * A thin divider marking where one merged turn ends and the next begins
 * inside a cross-message run (F6-1c), carrying that turn's own token usage
 * so it stays visible once the run is expanded even though the group header
 * only shows the sum.
 */
export interface TurnStrip {
  kind: 'turn';
  tokensIn: number;
  tokensOut: number;
}

/** Anything `ToolGroupComponent` can render inside its expanded body. */
export type GroupRow = RenderedPart | TurnStrip;

/** Names beyond this many are folded into a trailing ellipsis. */
const MAX_SUMMARY_NAMES = 4;

/**
 * Summarize a run of tool-call rows for a group header: the worst state
 * across the run (error > running/pending > completed) and a "read ×2, edit"
 * name list (first-appearance order, counts collapsed, capped at
 * `MAX_SUMMARY_NAMES` names before an ellipsis). Shared by the per-message
 * grouping pass (F6-1, `message-row.ts`) and the cross-message merge
 * (F6-1c, `tool-run-row.ts`) so both read the same rules.
 */
export function summarizeToolRun(
  rows: readonly RenderedPart[],
): { state: ToolStateKind; names: string } {
  const counts = new Map<string, number>();
  let state: ToolStateKind = 'completed';
  for (const row of rows) {
    if (row.part.type !== 'tool') {
      continue;
    }
    counts.set(row.part.name, (counts.get(row.part.name) ?? 0) + 1);
    const kind = row.part.state.state;
    if (kind === 'error') {
      state = 'error';
    } else if ((kind === 'running' || kind === 'pending') && state !== 'error') {
      state = 'running';
    }
  }
  const labels = [...counts.entries()].map(([name, n]) => (n > 1 ? `${name} ×${n}` : name));
  const names =
    labels.length > MAX_SUMMARY_NAMES
      ? `${labels.slice(0, MAX_SUMMARY_NAMES).join(', ')}, …`
      : labels.join(', ');
  return { state, names };
}

/**
 * Collapsible "N tool calls · read ×2, edit" summary row (F6-1), reused for
 * two cases: a run of >= 2 consecutive tool calls inside one message
 * (`message-row.ts`) and a run of >= 2 consecutive tool-only *messages*
 * merged into one group (F6-1c, `tool-run-row.ts`). The caller owns the
 * open/closed state (a per-message record in the first case, a single flag
 * in the second) and passes the already-summarized `state`/`names`/`count`.
 *
 * `rows` interleaves `RenderedPart`s with `TurnStrip` dividers: a lone
 * message never has any strips, a merged run has one before each turn's own
 * rows so the per-turn token counts stay visible once expanded even though
 * the header only shows the combined total.
 */
@Component({
  selector: 'app-tool-group',
  imports: [PartRendererComponent],
  template: `
    <div class="tool-group" [class.open]="open()">
      <button
        type="button"
        class="group-head"
        (click)="toggle.emit()"
        [attr.aria-expanded]="open()"
        [title]="open() ? t('toolGroup.collapse') : t('toolGroup.expand')"
      >
        <span class="dot state-{{ state() }}" aria-hidden="true"></span>
        <span class="group-count">{{ t('toolGroup.summary', { n: count() }) }}</span>
        <span class="group-sep" aria-hidden="true">·</span>
        <span class="group-names">{{ names() }}</span>
        <span class="chevron" aria-hidden="true">{{ open() ? '▾' : '▸' }}</span>
      </button>
      @if (open()) {
        <div class="group-body">
          @for (row of rows(); track $index) {
            @if (row.kind === 'turn') {
              <div class="turn-strip" [class.first]="$first">
                <span class="turn-usage">
                  {{ t('drawer.tokensIn') }} {{ row.tokensIn }} · {{ t('drawer.tokensOut') }} {{ row.tokensOut }}
                </span>
              </div>
            } @else {
              <app-part-renderer [part]="row.part" [toolIndex]="row.toolIndex" [taskLinks]="taskLinks()" />
            }
          }
        </div>
      }
    </div>
  `,
  styles: `
    .tool-group {
      border: 1px solid var(--border);
      border-radius: var(--radius-panel);
      background: var(--surface);
      overflow: hidden;
    }
    .group-head {
      display: flex;
      align-items: center;
      gap: var(--space-8);
      width: 100%;
      min-height: 28px;
      background: none;
      border: none;
      border-radius: 0;
      padding: 5px var(--space-12);
      cursor: pointer;
      text-align: left;
      min-width: 0;
    }
    .group-head:hover {
      background: var(--surface-2);
    }
    .group-head .dot {
      width: 7px;
      height: 7px;
      border-radius: 50%;
      flex: none;
      background: var(--text-faint);
    }
    .group-head .dot.state-completed {
      background: var(--success);
    }
    .group-head .dot.state-running {
      background: var(--accent);
      animation: tool-dot-pulse 1.4s ease-in-out infinite;
    }
    .group-head .dot.state-error {
      background: var(--danger);
    }
    @keyframes tool-dot-pulse {
      0%, 100% { opacity: 1; }
      50% { opacity: 0.35; }
    }
    .group-count {
      flex: none;
      font-family: var(--font-mono);
      font-size: var(--fs-12-5);
      font-weight: 600;
      color: var(--text);
    }
    .group-sep {
      flex: none;
      color: var(--text-faint);
      font-size: var(--fs-11-5);
    }
    .group-names {
      flex: 1 1 auto;
      min-width: 0;
      font-family: var(--font-mono);
      font-size: var(--fs-11-5);
      color: var(--text-muted);
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .group-head .chevron {
      flex: none;
      font-size: 10px;
      color: var(--text-faint);
    }
    .group-body {
      display: flex;
      flex-direction: column;
      gap: var(--space-4);
      padding: var(--space-8);
      border-top: 1px solid var(--border);
    }

    /* F6-1c: subtle divider between merged turns, carrying that turn's own
       token counts as a small label (the header only shows the sum). */
    .turn-strip {
      display: flex;
      align-items: center;
      padding-top: var(--space-4);
      margin-top: 2px;
      border-top: 1px dashed var(--border);
    }
    .turn-strip.first {
      padding-top: 0;
      margin-top: 0;
      border-top: none;
    }
    .turn-usage {
      font-family: var(--font-mono);
      font-size: var(--fs-11);
      color: var(--text-faint);
    }
  `,
})
export class ToolGroupComponent {
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly rows = input.required<GroupRow[]>();
  readonly state = input.required<ToolStateKind>();
  readonly names = input.required<string>();
  readonly count = input.required<number>();
  readonly open = input.required<boolean>();
  readonly taskLinks = input<Map<string, string>>(new Map());
  readonly toggle = output<void>();
}
