import { Component, computed, inject, input } from '@angular/core';

import { I18nService } from '../../i18n/i18n.service';
import { DiffViewStore } from './diff-view.store';

type LineKind = 'add' | 'del' | 'hunk' | 'meta' | 'ctx';

interface DiffLine {
  cls: LineKind;
  text: string;
}

/** One row of the split view: left = old side, right = new side. */
interface SplitRow {
  left: DiffLine | null;
  right: DiffLine | null;
  /** Full-width row (hunk headers, `diff --git`/`+++`/`---` metadata). */
  full: DiffLine | null;
}

/**
 * Diff block (F2-7).
 *
 * Renders tool output that looks like a unified diff with the handoff's
 * add/remove tokens, behind a unified/split toggle. Any other text is shown
 * verbatim in a monospace block. Diff detection stays conservative: only
 * output containing `@@` hunks or `diff --git` headers is interpreted.
 */
@Component({
  selector: 'app-diff-view',
  template: `
    @if (segments(); as lines) {
      <div class="diff-block">
        <div class="diff-head">
          @if (fileLabel()) {
            <span class="file">{{ fileLabel() }}</span>
          }
          <span class="stat add">+{{ added() }}</span>
          <span class="stat del">-{{ removed() }}</span>
          <div class="modes" role="group" [attr.aria-label]="t('diff.mode')">
            <button
              type="button"
              class="mode"
              [class.on]="mode() === 'unified'"
              [attr.aria-pressed]="mode() === 'unified'"
              (click)="store.setMode('unified')"
            >{{ t('diff.unified') }}</button>
            <button
              type="button"
              class="mode"
              [class.on]="mode() === 'split'"
              [attr.aria-pressed]="mode() === 'split'"
              (click)="store.setMode('split')"
            >{{ t('diff.split') }}</button>
          </div>
        </div>

        @if (mode() === 'split') {
          <div class="diff split">
            @for (row of splitRows(); track $index) {
              @if (row.full) {
                <div class="line full {{ row.full.cls }}"><code>{{ row.full.text }}</code></div>
              } @else {
                <div class="pair">
                  <div class="line side {{ row.left ? row.left.cls : 'empty' }}">
                    <code>{{ row.left ? row.left.text : '' }}</code>
                  </div>
                  <div class="line side {{ row.right ? row.right.cls : 'empty' }}">
                    <code>{{ row.right ? row.right.text : '' }}</code>
                  </div>
                </div>
              }
            }
          </div>
        } @else {
          <div class="diff">
            @for (line of lines; track $index) {
              <div class="line {{ line.cls }}"><code>{{ line.text }}</code></div>
            }
          </div>
        }
      </div>
    } @else {
      <pre class="plain"><code>{{ text() }}</code></pre>
    }
  `,
  styles: `
    .diff-block {
      border: 1px solid var(--border);
      border-radius: var(--radius-control-sm);
      overflow: hidden;
      background: var(--bg);
    }

    .diff-head {
      display: flex;
      align-items: center;
      gap: var(--space-8);
      padding: 5px var(--space-10);
      background: var(--surface-2);
      border-bottom: 1px solid var(--border);
    }

    .file {
      font-family: var(--font-mono);
      font-size: var(--fs-11-5);
      color: var(--text-muted);
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .stat {
      font-family: var(--font-mono);
      font-size: var(--fs-11);
    }
    .stat.add {
      color: var(--diff-add-text);
    }
    .stat.del {
      color: var(--diff-remove-text);
    }

    .modes {
      margin-left: auto;
      display: flex;
      gap: 2px;
      flex: none;
    }
    .mode {
      font-size: var(--fs-11);
      padding: 2px 8px;
      border: 1px solid var(--border);
      border-radius: var(--radius-control-sm);
      background: transparent;
      color: var(--text-faint);
    }
    .mode.on {
      background: var(--surface-3);
      color: var(--text);
      border-color: var(--border-strong);
    }

    .diff {
      padding: 6px 0;
      overflow-x: auto;
      font-size: var(--fs-12);
      line-height: 1.5;
    }

    .line {
      padding: 0 10px;
      white-space: pre;
      color: var(--code-text);
    }
    .line.add {
      background: var(--diff-add-bg);
      color: var(--diff-add-text);
    }
    .line.del {
      background: var(--diff-remove-bg);
      color: var(--diff-remove-text);
    }
    .line.hunk,
    .line.meta {
      color: var(--text-faint);
    }

    .split .pair {
      display: grid;
      grid-template-columns: 1fr 1fr;
      gap: 1px;
    }
    .split .side {
      min-width: 0;
      overflow: hidden;
      text-overflow: ellipsis;
    }
    .split .side.empty {
      background: var(--surface);
    }

    .plain {
      background: var(--bg);
      border: 1px solid var(--border);
      border-radius: var(--radius-control-sm);
      padding: 8px 10px;
      overflow-x: auto;
      white-space: pre-wrap;
      word-break: break-word;
      margin: 0;
      font-size: var(--fs-12);
      line-height: 1.5;
      color: var(--code-text-strong);
    }

    code {
      font-family: var(--font-mono);
    }
  `,
})
export class DiffViewComponent {
  private readonly i18n = inject(I18nService);
  readonly store = inject(DiffViewStore);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly text = input<string>('');
  /** Optional file path shown in the diff header. */
  readonly fileLabel = input<string>('');

  readonly mode = this.store.mode;

  readonly segments = computed<DiffLine[] | null>(() => {
    const text = this.text();
    const lines = text.split('\n');
    const looksLikeDiff = lines.some(
      (l) => l.startsWith('@@') || l.startsWith('diff --git') || l.startsWith('+++ b/'),
    );
    if (!looksLikeDiff) {
      return null;
    }
    return lines.map((line): DiffLine => {
      if (line.startsWith('+') && !line.startsWith('+++')) {
        return { cls: 'add', text: line };
      }
      if (line.startsWith('-') && !line.startsWith('---')) {
        return { cls: 'del', text: line };
      }
      if (line.startsWith('@@')) {
        return { cls: 'hunk', text: line };
      }
      if (line.startsWith('diff ') || line.startsWith('+++') || line.startsWith('---')) {
        return { cls: 'meta', text: line };
      }
      return { cls: 'ctx', text: line };
    });
  });

  readonly added = computed(
    () => this.segments()?.filter((l) => l.cls === 'add').length ?? 0,
  );
  readonly removed = computed(
    () => this.segments()?.filter((l) => l.cls === 'del').length ?? 0,
  );

  /**
   * Side-by-side rows: each run of deletions is zipped with the run of
   * additions that follows it, context lines appear on both sides, and hunk
   * headers / file metadata span the full width.
   */
  readonly splitRows = computed<SplitRow[]>(() => {
    const lines = this.segments();
    if (!lines) {
      return [];
    }
    const rows: SplitRow[] = [];
    let i = 0;
    while (i < lines.length) {
      const line = lines[i];
      if (line.cls === 'hunk' || line.cls === 'meta') {
        rows.push({ left: null, right: null, full: line });
        i += 1;
        continue;
      }
      if (line.cls === 'ctx') {
        rows.push({ left: line, right: line, full: null });
        i += 1;
        continue;
      }
      const dels: DiffLine[] = [];
      const adds: DiffLine[] = [];
      while (i < lines.length && lines[i].cls === 'del') {
        dels.push(lines[i]);
        i += 1;
      }
      while (i < lines.length && lines[i].cls === 'add') {
        adds.push(lines[i]);
        i += 1;
      }
      const height = Math.max(dels.length, adds.length);
      for (let k = 0; k < height; k++) {
        rows.push({ left: dels[k] ?? null, right: adds[k] ?? null, full: null });
      }
    }
    return rows;
  });
}
