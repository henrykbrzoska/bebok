/**
 * Find bar (Ctrl+F): the Angular rendering of the view-agnostic find state.
 *
 * `FindStore` (`core/find.store.ts`) owns open/query/toggles/matches;
 * `find-matches.ts` turns the query into ranges over plain text;
 * `find-highlight.ts` paints those ranges into the DOM. This component wires
 * the three together: it scans the owner view's `text` for matches, reports
 * the count back through `setMatches`, and paints the hits inside
 * `container` (unwrapping on every refresh/close so Angular's DOM is
 * restored).
 *
 * The owner view renders `<app-find-bar>` while `FindStore.open()` is true
 * and passes its searchable text plus the element showing it (e.g. the
 * explorer's highlighted `<code>` block). Match navigation (`next`/`prev`)
 * only moves `activeIndex`; this component scrolls the painted active hit
 * into view.
 */

import { Component, OnDestroy, OnInit, computed, effect, inject, input, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { FindStore } from '../../core/find.store';
import { I18nService } from '../../i18n/i18n.service';
import {
  FIND_HIT_ACTIVE_CLASS,
  FIND_HIT_CLASS,
  unwrapMarks,
  wrapRangeWithMark,
} from './find-highlight';
import { DEFAULT_MATCH_CAP, FindRange, buildFindRegex, findAll } from './find-matches';

/** How many hits are painted into the DOM (counting still uses the full cap). */
const PAINT_CAP = 200;

@Component({
  selector: 'app-find-bar',
  imports: [FormsModule],
  template: `
    <div
      class="find-bar"
      role="search"
      data-testid="find-bar"
      (keydown.escape)="close()"
    >
      <input
        class="find-input"
        type="text"
        data-testid="find-input"
        [ngModel]="store.query()"
        (ngModelChange)="store.setQuery($event)"
        [placeholder]="t('find.placeholder')"
        [attr.aria-label]="t('find.placeholder')"
      />
      <span class="find-count" data-testid="find-count">{{ counterText() }}</span>
      <button
        type="button"
        class="find-btn"
        data-testid="find-prev"
        [title]="t('find.prev')"
        [attr.aria-label]="t('find.prev')"
        [disabled]="!store.hasMatches()"
        (click)="store.prev()"
      >↑</button>
      <button
        type="button"
        class="find-btn"
        data-testid="find-next"
        [title]="t('find.next')"
        [attr.aria-label]="t('find.next')"
        [disabled]="!store.hasMatches()"
        (click)="store.next()"
      >↓</button>
      <button
        type="button"
        class="find-btn"
        data-testid="find-toggle-case"
        [title]="t('find.matchCase')"
        [attr.aria-label]="t('find.matchCase')"
        [attr.aria-pressed]="store.matchCase()"
        [class.on]="store.matchCase()"
        (click)="store.toggleMatchCase()"
      >Aa</button>
      <button
        type="button"
        class="find-btn"
        data-testid="find-toggle-word"
        [title]="t('find.wholeWord')"
        [attr.aria-label]="t('find.wholeWord')"
        [attr.aria-pressed]="store.wholeWord()"
        [class.on]="store.wholeWord()"
        (click)="store.toggleWholeWord()"
      >ab|</button>
      <button
        type="button"
        class="find-btn"
        data-testid="find-toggle-regex"
        [title]="t('find.useRegex')"
        [attr.aria-label]="t('find.useRegex')"
        [attr.aria-pressed]="store.useRegex()"
        [class.on]="store.useRegex()"
        (click)="store.toggleUseRegex()"
      >.*</button>
      <button
        type="button"
        class="find-btn"
        data-testid="find-close"
        [title]="t('find.close')"
        [attr.aria-label]="t('find.close')"
        (click)="close()"
      >✕</button>
    </div>
  `,
  styles: `
    .find-bar {
      display: flex;
      align-items: center;
      gap: 4px;
      padding: 4px 8px;
      border-bottom: 1px solid var(--border);
      background: var(--bg-elevated, var(--bg));
      font-size: var(--fs-12, 12px);
    }
    .find-input {
      flex: 1 1 auto;
      min-width: 0;
      padding: 3px 8px;
      border: 1px solid var(--border);
      border-radius: var(--radius-sm);
      background: var(--bg);
      color: var(--text);
      font: inherit;
    }
    .find-count {
      flex: none;
      min-width: 64px;
      text-align: center;
      color: var(--text-muted);
      font-variant-numeric: tabular-nums;
      white-space: nowrap;
    }
    .find-btn {
      flex: none;
      padding: 3px 7px;
      border: 1px solid transparent;
      border-radius: var(--radius-sm);
      background: transparent;
      color: var(--text-muted);
      font: inherit;
      cursor: pointer;
    }
    .find-btn:hover:not(:disabled) {
      background: var(--bg-hover);
      color: var(--text);
    }
    .find-btn:disabled {
      opacity: 0.4;
      cursor: default;
    }
    .find-btn.on {
      border-color: var(--accent);
      color: var(--accent);
    }
    mark.find-hit {
      background: var(--warning-soft, #f5d76e);
      color: inherit;
      border-radius: 2px;
    }
    mark.find-hit.find-hit-active {
      background: var(--warning, #e8a400);
      color: #000;
    }
  `,
})
export class FindBarComponent implements OnInit, OnDestroy {
  readonly store = inject(FindStore);
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);

  /** Scope the bar was opened for (see `FindStore.openFind`). */
  readonly scope = input('explorer');
  /** Plain text of the owner view that the query is matched against. */
  readonly text = input('');
  /** Element showing `text`; matches are painted inside it. */
  readonly container = input<HTMLElement | null>(null);

  /** True while the query is not a valid regular expression. */
  readonly regexError = signal(false);
  /** Bumped after every paint so the active-hit effect re-runs. */
  private readonly paintVersion = signal(0);
  private paintRun = 0;

  /** `3/17`, `3/500+`, an empty-state hint or an invalid-pattern warning. */
  readonly counterText = computed(() => {
    if (this.regexError()) {
      return this.t('find.invalidRegex');
    }
    const total = this.store.total();
    if (!this.store.query() || total === 0) {
      return this.t('find.noMatches');
    }
    const current = Math.min(this.store.activeIndex() + 1, total);
    return this.store.limited()
      ? this.t('find.counterLimited', { current, total })
      : this.t('find.counter', { current, total });
  });

  constructor() {
    // Re-scan whenever the query, the toggles or the owner text change.
    effect(() => {
      this.store.query();
      this.store.options();
      this.text();
      this.container();
      this.refresh();
    });
    // Re-apply the active hit after every paint and on navigation.
    effect(() => {
      this.store.activeIndex();
      this.paintVersion();
      this.applyActive();
    });
  }

  ngOnInit(): void {
    if (!this.store.open()) {
      this.store.openFind(this.scope());
    }
    this.refresh();
  }

  ngOnDestroy(): void {
    this.paintRun += 1;
    const el = this.container();
    if (el) {
      unwrapMarks(el);
    }
    if (this.store.activeScope() === this.scope()) {
      this.store.closeFind();
    }
  }

  close(): void {
    const el = this.container();
    if (el) {
      unwrapMarks(el);
    }
    this.store.closeFind();
  }

  private refresh(): void {
    const run = ++this.paintRun;
    const el = this.container();
    if (el) {
      unwrapMarks(el);
    }
    const built = buildFindRegex(this.store.query(), this.store.options());
    if ('error' in built) {
      this.regexError.set(true);
      this.store.setMatches(0, false);
      this.paintVersion.update((v) => v + 1);
      return;
    }
    this.regexError.set(false);
    const { ranges, limited } = findAll(this.text(), built.regex, DEFAULT_MATCH_CAP);
    this.store.setMatches(ranges.length, limited);
    // The owner view may not have rendered the new text yet; paint on the
    // next macrotask and drop the run if a newer refresh superseded it.
    window.setTimeout(() => {
      if (run !== this.paintRun) {
        return;
      }
      const target = this.container();
      if (target) {
        this.paintRanges(target, ranges.slice(0, PAINT_CAP));
      }
      this.paintVersion.update((v) => v + 1);
    }, 0);
  }

  /**
   * Paint ranges into the container's text nodes. Nodes are collected first
   * (splitting one node never shifts a later one) and each node's local
   * slices are wrapped back-to-front so earlier offsets stay valid.
   */
  private paintRanges(root: HTMLElement, ranges: readonly FindRange[]): void {
    const walker = root.ownerDocument.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    const nodes: Text[] = [];
    let node: Text | null = walker.nextNode() as Text | null;
    while (node) {
      nodes.push(node);
      node = walker.nextNode() as Text | null;
    }
    let offset = 0;
    for (const textNode of nodes) {
      const len = textNode.data.length;
      const local: Array<{ start: number; length: number }> = [];
      for (const r of ranges) {
        if (!r || r.length <= 0) {
          continue;
        }
        const from = Math.max(r.start, offset) - offset;
        const to = Math.min(r.start + r.length, offset + len) - offset;
        if (to > from) {
          local.push({ start: from, length: to - from });
        }
      }
      for (let i = local.length - 1; i >= 0; i--) {
        const slice = local[i];
        wrapRangeWithMark(textNode, slice.start, slice.length);
      }
      offset += len;
    }
  }

  private applyActive(): void {
    const el = this.container();
    if (!el) {
      return;
    }
    const marks = Array.from(el.querySelectorAll(`mark.${FIND_HIT_CLASS}`));
    marks.forEach((mark) => mark.classList.remove(FIND_HIT_ACTIVE_CLASS));
    if (marks.length === 0) {
      return;
    }
    const active = marks[Math.min(this.store.activeIndex(), marks.length - 1)];
    active.classList.add(FIND_HIT_ACTIVE_CLASS);
    if (typeof active.scrollIntoView === 'function') {
      try {
        active.scrollIntoView({ block: 'nearest' });
      } catch {
        // Detached node or headless DOM: the highlight itself is enough.
      }
    }
  }
}
