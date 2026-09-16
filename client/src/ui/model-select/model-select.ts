/**
 * Searchable model picker (replaces every model `<select>` in the GUI).
 *
 * A native `<select>` cannot be filtered, and providers like OpenRouter expose
 * hundreds of models - scrolling an unfiltered dropdown is unusable. This
 * combobox renders the current value as a button (styled by the caller through
 * `buttonClass` so it blends into the toolbar / dialog / settings card) and
 * opens a panel with a filter input plus the options grouped by provider.
 *
 * API: `[models]` (full `provider/model` ids), `[value]` + `(valueChange)`,
 * `[defaultLabel]` (the empty "use default" row), `[buttonClass]`,
 * `[testId]`, `[disabled]`. A value missing from `models` (a stale override)
 * is kept as an extra row so an existing choice is never silently dropped.
 */

import { ChangeDetectionStrategy, Component, computed, effect, ElementRef, inject, input, model, signal, viewChild } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { I18nService } from '../../i18n/i18n.service';

export interface ModelGroup {
  provider: string;
  items: string[];
}

/** Split `provider/model` for grouped display (`short` drops the prefix). */
export function splitModelOption(id: string): { provider: string; short: string } {
  const slash = id.indexOf('/');
  if (slash < 0) {
    return { provider: '', short: id };
  }
  return { provider: id.slice(0, slash), short: id.slice(slash + 1) };
}

/** Case-insensitive substring filter over the full `provider/model` ids. */
export function filterModelOptions(models: readonly string[], filter: string): string[] {
  const needle = filter.trim().toLowerCase();
  if (!needle) {
    return [...models];
  }
  return models.filter((m) => m.toLowerCase().includes(needle));
}

/** Group ids by provider prefix, preserving first-seen order. */
export function groupModelOptions(models: readonly string[]): ModelGroup[] {
  const groups: ModelGroup[] = [];
  const byProvider = new Map<string, string[]>();
  for (const id of models) {
    const { provider } = splitModelOption(id);
    const list = byProvider.get(provider);
    if (list) {
      list.push(id);
    } else {
      byProvider.set(provider, [id]);
    }
  }
  for (const [provider, items] of byProvider) {
    groups.push({ provider, items });
  }
  return groups;
}

@Component({
  selector: 'app-model-select',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  templateUrl: './model-select.html',
  styleUrl: './model-select.css',
  host: { '(document:click)': 'onDocumentClick($event)', '(document:keydown)': 'onDocumentKeydown($event)' },
})
export class ModelSelect {
  private readonly el = inject(ElementRef<HTMLElement>);
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);

  /** Full `provider/model` ids to offer. */
  readonly models = input.required<string[]>();
  /** Current value; `''` means "default" (two-way via `[(value)]`). */
  readonly value = model<string>('');
  /** Label of the empty "use default" row. */
  readonly defaultLabel = input.required<string>();
  /** Extra class(es) for the trigger button (`tb-select`, `control`, `mono`). */
  readonly buttonClass = input('');
  readonly testId = input<string | null>(null);
  readonly disabled = input(false);

  readonly open = signal(false);
  readonly filter = signal('');
  /** Flat index of the keyboard-highlighted row (0 = the default row). */
  readonly highlight = signal(0);
  readonly searchInput = viewChild<ElementRef<HTMLInputElement>>('search');

  /** Models plus the stale current value (never drop an existing choice). */
  readonly options = computed<string[]>(() => {
    const list = this.models();
    const current = this.value();
    if (current && !list.includes(current)) {
      return [current, ...list];
    }
    return list;
  });

  readonly filtered = computed<string[]>(() => filterModelOptions(this.options(), this.filter()));
  readonly groups = computed<ModelGroup[]>(() => groupModelOptions(this.filtered()));
  /** Flat rows for keyboard navigation: `''` (default) first, then filtered. */
  readonly flatRows = computed<string[]>(() => ['', ...this.filtered()]);

  readonly displayText = computed(() => this.value() || this.defaultLabel());
  readonly displayTitle = computed(() => this.value() || this.defaultLabel());

  constructor() {
    // A new filter (or reopen) invalidates the keyboard highlight.
    effect(() => {
      this.filter();
      this.open();
      this.highlight.set(0);
    });
  }

  toggle(event: Event): void {
    event.stopPropagation();
    if (this.disabled()) {
      return;
    }
    this.open.update((o) => !o);
    if (this.open()) {
      this.filter.set('');
      // The panel renders on the next change-detection pass; focus afterwards.
      setTimeout(() => this.searchInput()?.nativeElement.focus(), 0);
    }
  }

  pick(id: string): void {
    this.value.set(id);
    this.open.set(false);
  }

  shortName(id: string): string {
    return splitModelOption(id).short;
  }

  onSearchKeydown(event: KeyboardEvent): void {
    const rows = this.flatRows();
    if (event.key === 'Escape') {
      event.preventDefault();
      this.open.set(false);
    } else if (event.key === 'Enter') {
      event.preventDefault();
      this.pick(rows[Math.min(this.highlight(), rows.length - 1)] ?? '');
    } else if (event.key === 'ArrowDown') {
      event.preventDefault();
      this.highlight.update((h) => Math.min(h + 1, rows.length - 1));
    } else if (event.key === 'ArrowUp') {
      event.preventDefault();
      this.highlight.update((h) => Math.max(h - 1, 0));
    }
  }

  onDocumentClick(event: Event): void {
    if (this.open() && !this.el.nativeElement.contains(event.target as Node)) {
      this.open.set(false);
    }
  }

  onDocumentKeydown(event: KeyboardEvent): void {
    if (event.key === 'Escape' && this.open()) {
      this.open.set(false);
    }
  }
}
