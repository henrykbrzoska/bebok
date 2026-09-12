/**
 * Sortable breakdown table (F7-5), shared by the models / providers / agents /
 * projects / tools / top-sessions sections of the Stats screen.
 *
 * Columns are declared by the parent (`StatsColumn[]`): the key to read, the
 * i18n label and how to render the cell. Clicking a header sorts by that
 * column (numbers start descending, text ascending; a second click flips).
 * Sorting itself is the pure `sortRows` helper from the store so it is unit-
 * tested without a DOM.
 */

import { ChangeDetectionStrategy, Component, computed, inject, input, signal } from '@angular/core';

import { I18nService } from '../../i18n/i18n.service';
import { MessageKey } from '../../i18n';
import { formatCost } from '../chat/chat-session.store';
import { SortState, sortRows, toggleSort } from './stats.store';

export type CellKind = 'text' | 'mono' | 'number' | 'cost';

export interface StatsColumn<T extends object = Record<string, unknown>> {
  key: keyof T & string;
  labelKey: MessageKey;
  kind: CellKind;
  /** Optional fallback i18n key when the value is null/empty (text cells). */
  emptyKey?: MessageKey;
}

/** `12 345` - full number, thousands separated by a thin space. */
export function formatNumber(value: number): string {
  return Math.round(value)
    .toString()
    .replace(/\B(?=(\d{3})+(?!\d))/g, '\u202f');
}

@Component({
  selector: 'app-stats-table',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="table-wrap">
      <table class="stats-table">
        <thead>
          <tr>
            @for (col of columns(); track col.key) {
              <th
                scope="col"
                [class.num]="isNumeric(col)"
                [class.sorted]="sort()?.key === col.key"
                [attr.aria-sort]="ariaSort(col)"
              >
                <button type="button" class="sort-btn" (click)="sortBy(col)">
                  <span>{{ t(col.labelKey) }}</span>
                  <span class="arrow" aria-hidden="true">{{ arrow(col) }}</span>
                </button>
              </th>
            }
          </tr>
        </thead>
        <tbody>
          @for (row of sorted(); track trackRow($index, row)) {
            <tr>
              @for (col of columns(); track col.key) {
                <td
                  [class.num]="isNumeric(col)"
                  [class.mono]="col.kind === 'mono'"
                  [class.muted]="isEmpty(row, col)"
                >
                  {{ cell(row, col) }}
                </td>
              }
            </tr>
          }
          @if (sorted().length === 0) {
            <tr>
              <td class="empty" [attr.colspan]="columns().length">{{ t('stats.tableEmpty') }}</td>
            </tr>
          }
        </tbody>
      </table>
    </div>
  `,
  styles: `
    :host {
      display: block;
    }
    .table-wrap {
      overflow-x: auto;
    }
    .stats-table {
      width: 100%;
      border-collapse: collapse;
      font-size: var(--fs-12-5);
    }
    th,
    td {
      padding: 6px var(--space-10);
      border-bottom: 1px solid var(--border);
      text-align: left;
      white-space: nowrap;
    }
    th {
      font-size: var(--fs-label);
      font-weight: 600;
      letter-spacing: var(--label-tracking);
      text-transform: uppercase;
      color: var(--text-faint);
      background: var(--surface-2);
      position: sticky;
      top: 0;
    }
    th.sorted {
      color: var(--text);
    }
    .sort-btn {
      all: unset;
      cursor: pointer;
      display: inline-flex;
      align-items: center;
      gap: 4px;
    }
    .sort-btn:hover {
      color: var(--text);
    }
    .sort-btn:focus-visible {
      outline: 1px solid var(--accent);
      outline-offset: 2px;
      border-radius: 3px;
    }
    .arrow {
      display: inline-block;
      width: 1em;
      color: var(--accent);
    }
    th.num,
    td.num {
      text-align: right;
      font-variant-numeric: tabular-nums;
    }
    td.mono {
      font-family: var(--font-mono);
      font-size: var(--fs-12);
      color: var(--code-text-strong);
      max-width: 420px;
      overflow: hidden;
      text-overflow: ellipsis;
    }
    td.muted {
      color: var(--text-faint);
    }
    tbody tr:hover td {
      background: var(--surface-2);
    }
    td.empty {
      text-align: center;
      color: var(--text-faint);
      padding: var(--space-14);
    }
  `,
})
export class StatsTable<T extends object = Record<string, unknown>> {
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly rows = input.required<readonly T[]>();
  readonly columns = input.required<StatsColumn<T>[]>();
  /** Initial sort; defaults to the first numeric column, descending. */
  readonly initialSort = input<SortState<keyof T & string> | null>(null);

  private readonly userSort = signal<SortState<keyof T & string> | null>(null);

  readonly sort = computed<SortState<keyof T & string> | null>(() => {
    const chosen = this.userSort();
    if (chosen) {
      return chosen;
    }
    const initial = this.initialSort();
    if (initial) {
      return initial;
    }
    const numeric = this.columns().find((c) => this.isNumeric(c));
    return numeric ? { key: numeric.key, dir: 'desc' } : null;
  });

  readonly sorted = computed<T[]>(() => sortRows(this.rows(), this.sort()));

  isNumeric(col: StatsColumn<T>): boolean {
    return col.kind === 'number' || col.kind === 'cost';
  }

  sortBy(col: StatsColumn<T>): void {
    this.userSort.set(toggleSort(this.sort(), col.key, this.isNumeric(col)));
  }

  arrow(col: StatsColumn<T>): string {
    const s = this.sort();
    if (!s || s.key !== col.key) {
      return '';
    }
    return s.dir === 'asc' ? '↑' : '↓';
  }

  ariaSort(col: StatsColumn<T>): 'ascending' | 'descending' | null {
    const s = this.sort();
    if (!s || s.key !== col.key) {
      return null;
    }
    return s.dir === 'asc' ? 'ascending' : 'descending';
  }

  isEmpty(row: T, col: StatsColumn<T>): boolean {
    const v = row[col.key] as unknown;
    return v === null || v === undefined || v === '';
  }

  cell(row: T, col: StatsColumn<T>): string {
    const v = row[col.key] as unknown;
    switch (col.kind) {
      case 'number':
        return typeof v === 'number' ? formatNumber(v) : '—';
      case 'cost':
        return formatCost(typeof v === 'number' ? v : null);
      default:
        if (v === null || v === undefined || v === '') {
          return col.emptyKey ? this.t(col.emptyKey) : '—';
        }
        return String(v);
    }
  }

  trackRow(index: number, row: T): string {
    const r = row as Record<string, unknown>;
    const id = r['id'] ?? r['key'] ?? r['name'];
    return typeof id === 'string' ? id : String(index);
  }
}
