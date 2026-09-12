/**
 * Stats store (F7-5).
 *
 * Owns the two filters of the Stats screen (project: all / current, range:
 * 7d / 30d / all), the `GET /stats` request they map to, and the derived
 * view models (summary tiles, daily bars). Table sorting is a pure helper
 * (`sortRows`) shared by every breakdown table so it can be unit-tested
 * without a component.
 *
 * The engine does the heavy lifting (one disk scan cached across requests);
 * this store only re-fetches when a filter changes, when the user asks, or
 * when a turn finishes (`session.updated` with `running: false`).
 */

import { Injectable, computed, inject, signal } from '@angular/core';

import { EngineClient } from '../../core/engine-client.service';
import { StatsDay, StatsResponse } from '../../core/engine.dtos';
import { ProjectSessionsStore } from '../../ui/shell/project-sessions.store';

export type ProjectScope = 'all' | 'current';
export type RangeScope = '7d' | '30d' | 'all';
export type SortDir = 'asc' | 'desc';

export interface SortState<K extends string = string> {
  key: K;
  dir: SortDir;
}

const DAY_MS = 86_400_000;

/** Days covered by each range option (`null` = unbounded). */
export const RANGE_DAYS: Record<RangeScope, number | null> = { '7d': 7, '30d': 30, all: null };

/** Lower bound (epoch ms) for a range option, anchored at `now`. */
export function rangeFrom(range: RangeScope, now: number): number | null {
  const days = RANGE_DAYS[range];
  return days === null ? null : now - days * DAY_MS;
}

/**
 * Sort rows by one column. Numbers sort numerically, strings case-insensitively;
 * unknown values (null cost, empty label) sink to the bottom regardless of
 * direction.
 * Stable: equal keys keep their incoming order. Never mutates `rows`.
 */
export function sortRows<T extends object>(
  rows: readonly T[],
  sort: SortState<keyof T & string> | null,
): T[] {
  if (!sort) {
    return [...rows];
  }
  const sign = sort.dir === 'asc' ? 1 : -1;
  return rows
    .map((row, index) => ({ row, index }))
    .sort((a, b) => {
      const av = a.row[sort.key] as unknown;
      const bv = b.row[sort.key] as unknown;
      const an = av === null || av === undefined || av === '';
      const bn = bv === null || bv === undefined || bv === '';
      // Unknown values (null cost, empty label) sink to the bottom in both directions.
      if (an !== bn) {
        return an ? 1 : -1;
      }
      const cmp = an ? 0 : compareValues(av, bv) * sign;
      return cmp !== 0 ? cmp : a.index - b.index;
    })
    .map(({ row }) => row);
}

/** Next sort state after clicking a header: same column flips, new column starts desc for numbers, asc for text. */
export function toggleSort<K extends string>(
  current: SortState<K> | null,
  key: K,
  numeric: boolean,
): SortState<K> {
  if (current && current.key === key) {
    return { key, dir: current.dir === 'asc' ? 'desc' : 'asc' };
  }
  return { key, dir: numeric ? 'desc' : 'asc' };
}

function compareValues(a: unknown, b: unknown): number {
  if (typeof a === 'number' && typeof b === 'number') {
    return a - b;
  }
  return String(a).localeCompare(String(b), undefined, { sensitivity: 'base' });
}

/** One bar of the daily chart. */
export interface DayBar {
  day: string;
  /** `9/10` style short label. */
  label: string;
  total: number;
  /** Segment heights as a percentage of the tallest bar. */
  inPct: number;
  outPct: number;
  cachePct: number;
  cost: number | null;
  calls: number;
}

/** `2026-09-10` -> `10 Sep`-ish short label without locale machinery: `M/D`. */
export function shortDayLabel(day: string): string {
  const [, m, d] = day.split('-');
  return `${Number(m)}/${Number(d)}`;
}

/** Build chart bars scaled to the tallest day (empty days keep a 0 height). */
export function dayBars(days: readonly StatsDay[]): DayBar[] {
  const totals = days.map(
    (d) => d.input_tokens + d.output_tokens + d.cache_read_tokens + d.cache_write_tokens,
  );
  const max = Math.max(0, ...totals);
  const pct = (v: number): number => (max > 0 ? (v / max) * 100 : 0);
  return days.map((d, i) => ({
    day: d.day,
    label: shortDayLabel(d.day),
    total: totals[i],
    inPct: pct(d.input_tokens),
    outPct: pct(d.output_tokens),
    cachePct: pct(d.cache_read_tokens + d.cache_write_tokens),
    cost: d.cost,
    calls: d.llm_calls,
  }));
}

@Injectable({ providedIn: 'root' })
export class StatsStore {
  private readonly engine = inject(EngineClient);
  private readonly project = inject(ProjectSessionsStore);

  readonly scope = signal<ProjectScope>('all');
  readonly range = signal<RangeScope>('30d');

  readonly data = signal<StatsResponse | null>(null);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  /** Directory the "current" scope resolves to (null = no project open). */
  readonly currentDirectory = computed(() => this.project.directory());
  /** True when "current" is selected but nothing is open: the request falls back to all projects. */
  readonly scopeFallback = computed(() => this.scope() === 'current' && !this.currentDirectory());

  readonly totals = computed(() => this.data()?.totals ?? null);
  readonly bars = computed<DayBar[]>(() => dayBars(this.data()?.by_day ?? []));
  readonly hasActivity = computed(() => {
    const t = this.totals();
    return !!t && (t.sessions > 0 || t.llm_calls > 0 || t.tool_calls > 0);
  });

  private requestSeq = 0;

  setScope(scope: ProjectScope): void {
    if (scope !== this.scope()) {
      this.scope.set(scope);
      void this.load();
    }
  }

  setRange(range: RangeScope): void {
    if (range !== this.range()) {
      this.range.set(range);
      void this.load();
    }
  }

  /** The query the current filters map to (exposed for specs). */
  query(now = Date.now()): { directory: string | null; from: number | null } {
    const directory = this.scope() === 'current' ? this.currentDirectory() : null;
    return { directory, from: rangeFrom(this.range(), now) };
  }

  async load(): Promise<void> {
    const seq = ++this.requestSeq;
    this.loading.set(true);
    this.error.set(null);
    try {
      if (!this.engine.connected()) {
        await this.engine.connect();
      }
      const res = await this.engine.stats(this.query());
      if (seq === this.requestSeq) {
        this.data.set(res);
      }
    } catch (err) {
      if (seq === this.requestSeq) {
        this.error.set(err instanceof Error ? err.message : String(err));
      }
    } finally {
      if (seq === this.requestSeq) {
        this.loading.set(false);
      }
    }
  }
}
