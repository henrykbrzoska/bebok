import { Component, computed, inject, input } from '@angular/core';

import { I18nService } from '../../i18n/i18n.service';
import { CodeHighlightService } from '../code-highlight/code-highlight.service';
import { DiffViewStore } from './diff-view.store';

type LineKind = 'add' | 'del' | 'hunk' | 'meta' | 'ctx';

interface DiffLine {
  cls: LineKind;
  text: string;
  /**
   * F7-4: safe-to-bind HTML for this line. For `add`/`del`/`ctx` this is the
   * leading `+`/`-`/` ` marker plus syntax-highlighted code (tokens colored,
   * the add/remove *background* stays on `.line.add`/`.line.del` from `cls`
   * so highlighting never fights the diff coloring). `hunk`/`meta` lines are
   * just escaped, unhighlighted text.
   */
  html: string;
}

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
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
                <div class="line full {{ row.full.cls }}"><code [innerHTML]="row.full.html"></code></div>
              } @else {
                <div class="pair">
                  <div class="line side {{ row.left ? row.left.cls : 'empty' }}">
                    <code [innerHTML]="row.left ? row.left.html : ''"></code>
                  </div>
                  <div class="line side {{ row.right ? row.right.cls : 'empty' }}">
                    <code [innerHTML]="row.right ? row.right.html : ''"></code>
                  </div>
                </div>
              }
            }
          </div>
        } @else {
          <div class="diff">
            @for (line of lines; track $index) {
              <div class="line {{ line.cls }}"><code [innerHTML]="line.html"></code></div>
            }
          </div>
        }
      </div>
    } @else {
      <pre class="plain"><code class="hljs" [innerHTML]="plainHtml()"></code></pre>
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
  private readonly codeHighlight = inject(CodeHighlightService);
  readonly store = inject(DiffViewStore);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly text = input<string>('');
  /** Optional file path shown in the diff header. */
  readonly fileLabel = input<string>('');

  readonly mode = this.store.mode;

  /** Line classification only (no highlighting yet - see `segments`). */
  private readonly rawLines = computed<Pick<DiffLine, 'cls' | 'text'>[] | null>(() => {
    const text = this.text();
    const lines = text.split('\n');
    const looksLikeDiff = lines.some(
      (l) => l.startsWith('@@') || l.startsWith('diff --git') || l.startsWith('+++ b/'),
    );
    if (!looksLikeDiff) {
      return null;
    }
    return lines.map((line): Pick<DiffLine, 'cls' | 'text'> => {
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

  /**
   * F7-4: one language for the whole block, so every line tokenizes
   * consistently. Preference: the `[fileLabel]` input (set by the caller when
   * it knows the real path - `tool-part.ts`, `diff-overlay.ts`), falling back
   * to the path named on the diff's own `+++ b/…`/`diff --git a/… b/…` line.
   */
  private readonly language = computed<string | null>(() => {
    const label = this.fileLabel();
    if (label) {
      const lang = this.codeHighlight.resolveLanguage(null, label);
      if (lang) {
        return lang;
      }
    }
    const lines = this.rawLines();
    if (!lines) {
      return null;
    }
    for (const line of lines) {
      if (line.cls !== 'meta') {
        continue;
      }
      const match = /^\+\+\+ b\/(.+)$/.exec(line.text) ?? /^diff --git a\/\S+ b\/(.+)$/.exec(line.text);
      if (match) {
        const lang = this.codeHighlight.resolveLanguage(null, match[1]);
        if (lang) {
          return lang;
        }
      }
    }
    return null;
  });

  /**
   * Diff lines to render. `add`/`del`/`ctx` keep their leading `+`/`-`/` `
   * marker and get the rest of the line syntax-highlighted; `hunk`/`meta`
   * lines are just escaped (they are diff punctuation, not code).
   */
  readonly segments = computed<DiffLine[] | null>(() => {
    const lines = this.rawLines();
    if (!lines) {
      return null;
    }
    const language = this.language();
    return lines.map((line) => ({ ...line, html: this.renderLineHtml(line, language) }));
  });

  private renderLineHtml(line: Pick<DiffLine, 'cls' | 'text'>, language: string | null): string {
    if (line.cls === 'hunk' || line.cls === 'meta') {
      return escapeHtml(line.text);
    }
    let marker = '';
    let code = line.text;
    if (line.cls === 'add' || line.cls === 'del') {
      marker = line.text.slice(0, 1);
      code = line.text.slice(1);
    } else if (line.text.startsWith(' ')) {
      marker = ' ';
      code = line.text.slice(1);
    }
    const { html } = this.codeHighlight.highlight(code, { language });
    return `${escapeHtml(marker)}${html}`;
  }

  readonly added = computed(
    () => this.segments()?.filter((l) => l.cls === 'add').length ?? 0,
  );
  readonly removed = computed(
    () => this.segments()?.filter((l) => l.cls === 'del').length ?? 0,
  );

  /** Same highlighting for the non-diff plain-text fallback below. */
  readonly plainHtml = computed(
    () => this.codeHighlight.highlight(this.text(), { filename: this.fileLabel() || null }).html,
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
