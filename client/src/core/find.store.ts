/**
 * Find-in-page state (Ctrl+F).
 *
 * A single root store owns everything the find bar needs to render: whether
 * it is open and for which *scope* (`activeScope`, e.g. a chat transcript or
 * a rendered file preview), the query and its three option toggles, and the
 * match bookkeeping (how many hits the view found, which one is active and
 * whether the search stopped at the view's cap).
 *
 * The store is deliberately view-agnostic: it never touches the DOM and never
 * knows *how* a match was produced. The owner view runs `findAll` over its own
 * text, reports the count back through `setMatches`, and asks for the next /
 * previous hit with `next` / `prev` - wrapping around the ends of the list the
 * way a browser's find bar does. Two views can therefore share one bar by
 * passing their own scope to `openFind`.
 *
 * Nothing here talks to the engine; the state is ephemeral (not persisted).
 */

import { Injectable, computed, signal } from '@angular/core';

/** Options that turn a raw query into a matcher (see `find-matches.ts`). */
export interface FindOptions {
  matchCase: boolean;
  wholeWord: boolean;
  useRegex: boolean;
}

@Injectable({ providedIn: 'root' })
export class FindStore {
  /** Whether the find bar is visible. */
  readonly open = signal(false);
  /** Current query, verbatim (never trimmed - spaces are meaningful). */
  readonly query = signal('');
  /** `Aa` toggle: match case exactly. */
  readonly matchCase = signal(false);
  /** `ab|` toggle: only whole words. */
  readonly wholeWord = signal(false);
  /** `.*` toggle: treat the query as a regular expression. */
  readonly useRegex = signal(false);
  /** Number of matches the owning view reported, or 0 while unknown. */
  readonly total = signal(0);
  /** Index of the active match within `[0, total)`, or 0 when there is none. */
  readonly activeIndex = signal(0);
  /** True when the view stopped counting at its cap (there may be more). */
  readonly limited = signal(false);
  /** Which view opened the bar (null while closed). */
  readonly activeScope = signal<string | null>(null);

  /** Convenience for the counter (`3/17`) and the empty-state hint. */
  readonly hasMatches = computed(() => this.total() > 0);

  /** Options bundle for `buildFindRegex` / `findAll` consumers. */
  readonly options = computed<FindOptions>(() => ({
    matchCase: this.matchCase(),
    wholeWord: this.wholeWord(),
    useRegex: this.useRegex(),
  }));

  /**
   * Open the bar for a scope. Opening an already-open bar is idempotent: the
   * query (and the toggles) survive, so a second Ctrl+F from a nested view
   * does not throw the user's search away. Only the transition from closed to
   * open resets the match bookkeeping.
   */
  openFind(scope: string): void {
    if (this.open()) {
      this.activeScope.set(scope);
      return;
    }
    this.activeScope.set(scope);
    this.total.set(0);
    this.activeIndex.set(0);
    this.limited.set(false);
    this.open.set(true);
  }

  /** Hide the bar. The query is kept so reopening it restores the search. */
  closeFind(): void {
    this.open.set(false);
    this.activeScope.set(null);
    this.total.set(0);
    this.activeIndex.set(0);
    this.limited.set(false);
  }

  /** Replace the query and restart from the first match. */
  setQuery(query: string): void {
    if (this.query() === query) {
      return;
    }
    this.query.set(query);
    this.activeIndex.set(0);
    this.limited.set(false);
    this.total.set(0);
  }

  toggleMatchCase(): void {
    this.matchCase.update((value) => !value);
  }

  toggleWholeWord(): void {
    this.wholeWord.update((value) => !value);
  }

  toggleUseRegex(): void {
    this.useRegex.update((value) => !value);
  }

  /** Report the result of a fresh scan of the active scope. */
  setMatches(total: number, limited: boolean): void {
    const safeTotal = Number.isFinite(total) ? Math.max(0, Math.floor(total)) : 0;
    this.total.set(safeTotal);
    this.limited.set(safeTotal > 0 ? limited : false);
    if (safeTotal === 0) {
      this.activeIndex.set(0);
      return;
    }
    this.activeIndex.set(Math.min(this.activeIndex(), safeTotal - 1));
  }

  /** Move to the next match, wrapping past the last one back to the first. */
  next(): void {
    const total = this.total();
    if (total <= 0) {
      return;
    }
    this.activeIndex.set((this.activeIndex() + 1) % total);
  }

  /** Move to the previous match, wrapping before the first one to the last. */
  prev(): void {
    const total = this.total();
    if (total <= 0) {
      return;
    }
    this.activeIndex.set((this.activeIndex() - 1 + total) % total);
  }

  /** Select one match by index; out-of-range values are clamped. */
  setActive(index: number): void {
    const total = this.total();
    if (total <= 0) {
      this.activeIndex.set(0);
      return;
    }
    if (!Number.isFinite(index)) {
      return;
    }
    this.activeIndex.set(Math.min(total - 1, Math.max(0, Math.floor(index))));
  }
}
