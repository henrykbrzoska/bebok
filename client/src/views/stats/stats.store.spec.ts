/**
 * F7-5: Stats store.
 *
 * - the two filters must map onto the `GET /stats` query (directory only for
 *   "current", `from` only for bounded ranges);
 * - a filter change reloads, a no-op change does not;
 * - a failing request surfaces as `error` and leaves the last data alone;
 * - `dayBars` scales every day against the tallest one and `sortRows`
 *   orders numbers/strings with unknown costs sinking to the bottom.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { EngineClient } from '../../core/engine-client.service';
import { StatsDay, StatsResponse } from '../../core/engine.dtos';
import { ProjectSessionsStore } from '../../ui/shell/project-sessions.store';
import { StatsStore, dayBars, rangeFrom, shortDayLabel, sortRows, toggleSort } from './stats.store';

const DAY = 86_400_000;

function day(d: string, input = 0, output = 0, cache = 0): StatsDay {
  return {
    day: d,
    input_tokens: input,
    output_tokens: output,
    cache_read_tokens: cache,
    cache_write_tokens: 0,
    cost: null,
    llm_calls: input + output > 0 ? 1 : 0,
    tool_calls: 0,
  };
}

function response(overrides: Partial<StatsResponse> = {}): StatsResponse {
  return {
    range: { directory: null, from: null, to: null, scanned_sessions: 2 },
    totals: {
      sessions: 2,
      turns: 3,
      llm_calls: 4,
      tool_calls: 5,
      input_tokens: 1000,
      output_tokens: 200,
      cache_read_tokens: 50,
      cache_write_tokens: 0,
      cost: 0.12,
      cost_unknown_calls: 1,
    },
    by_model: [],
    by_provider: [],
    by_agent: [],
    by_project: [],
    by_day: [day('2026-09-09', 10, 5), day('2026-09-10', 100, 50, 50)],
    top_sessions: [],
    tools: [],
    compaction: { count: 0, avg_context_before: null },
    ...overrides,
  };
}

describe('StatsStore (F7-5)', () => {
  let store: StatsStore;
  let engine: EngineClient;
  let project: ProjectSessionsStore;

  beforeEach(() => {
    localStorage.clear();
    TestBed.configureTestingModule({
      providers: [provideZonelessChangeDetection()],
    });
    engine = TestBed.inject(EngineClient);
    project = TestBed.inject(ProjectSessionsStore);
    spyOn(engine, 'connected').and.returnValue(true);
    store = TestBed.inject(StatsStore);
  });

  it('maps the default filters to an unbounded, all-projects query', () => {
    const now = 1_788_998_400_000;
    expect(store.scope()).toBe('all');
    expect(store.range()).toBe('30d');
    expect(store.query(now)).toEqual({ directory: null, from: now - 30 * DAY });
  });

  it('sends the open project only for the "current" scope', () => {
    const now = 1_788_998_400_000;
    project.directory.set('C:/projects/demo');
    store.scope.set('current');
    store.range.set('all');
    expect(store.query(now)).toEqual({ directory: 'C:/projects/demo', from: null });
    expect(store.scopeFallback()).toBeFalse();

    project.directory.set(null);
    expect(store.query(now).directory).withContext('no project: fall back to all').toBeNull();
    expect(store.scopeFallback()).toBeTrue();
  });

  it('reloads on a filter change and stores the payload', async () => {
    const spy = spyOn(engine, 'stats').and.returnValue(Promise.resolve(response()));
    store.setRange('7d');
    await Promise.resolve();
    await Promise.resolve();
    expect(spy).toHaveBeenCalledTimes(1);
    const query = spy.calls.mostRecent().args[0]!;
    expect(query.directory).toBeNull();
    expect(typeof query.from).toBe('number');

    store.setRange('7d');
    expect(spy).withContext('same range: no request').toHaveBeenCalledTimes(1);

    await store.load();
    expect(store.data()?.totals.sessions).toBe(2);
    expect(store.hasActivity()).toBeTrue();
    expect(store.loading()).toBeFalse();
    expect(store.error()).toBeNull();
  });

  it('keeps the previous payload and reports the error when a request fails', async () => {
    spyOn(engine, 'stats').and.returnValues(
      Promise.resolve(response()),
      Promise.reject(new Error('engine GET /stats -> 500: boom')),
    );
    await store.load();
    expect(store.data()).not.toBeNull();

    await store.load();
    expect(store.error()).toContain('500');
    expect(store.data()?.totals.sessions).withContext('stale data stays visible').toBe(2);
    expect(store.loading()).toBeFalse();
  });

  it('treats a payload without any activity as empty', async () => {
    spyOn(engine, 'stats').and.returnValue(
      Promise.resolve(
        response({
          totals: {
            sessions: 0,
            turns: 0,
            llm_calls: 0,
            tool_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost: null,
            cost_unknown_calls: 0,
          },
          by_day: [],
        }),
      ),
    );
    await store.load();
    expect(store.hasActivity()).toBeFalse();
    expect(store.bars()).toEqual([]);
  });

  it('scales the daily bars against the tallest day', async () => {
    spyOn(engine, 'stats').and.returnValue(Promise.resolve(response()));
    await store.load();
    const bars = store.bars();
    expect(bars.length).toBe(2);
    expect(bars[1].total).toBe(200);
    expect(bars[1].inPct).toBe(50);
    expect(bars[1].outPct).toBe(25);
    expect(bars[1].cachePct).toBe(25);
    expect(bars[0].total).toBe(15);
    expect(bars[0].inPct).toBe(5);
    expect(bars[0].label).toBe('9/9');
  });
});

describe('stats helpers (F7-5)', () => {
  it('rangeFrom anchors bounded ranges at now', () => {
    expect(rangeFrom('7d', 1000 + 7 * DAY)).toBe(1000);
    expect(rangeFrom('30d', 30 * DAY)).toBe(0);
    expect(rangeFrom('all', 123)).toBeNull();
  });

  it('shortDayLabel strips the year and leading zeros', () => {
    expect(shortDayLabel('2026-09-01')).toBe('9/1');
    expect(shortDayLabel('2026-12-25')).toBe('12/25');
  });

  it('dayBars yields zero heights when every day is empty', () => {
    const bars = dayBars([day('2026-01-01'), day('2026-01-02')]);
    expect(bars.map((b) => b.inPct + b.outPct + b.cachePct)).toEqual([0, 0]);
  });

  describe('sortRows', () => {
    const rows = [
      { key: 'b-model', tokens: 10, cost: null as number | null },
      { key: 'A-model', tokens: 30, cost: 0.5 },
      { key: 'c-model', tokens: 20, cost: 0.1 },
      { key: 'd-model', tokens: 20, cost: null as number | null },
    ];

    it('returns a copy in the incoming order without a sort', () => {
      const out = sortRows(rows, null);
      expect(out).toEqual(rows);
      expect(out).not.toBe(rows);
    });

    it('sorts numbers descending and keeps ties stable', () => {
      const out = sortRows(rows, { key: 'tokens', dir: 'desc' });
      expect(out.map((r) => r.key)).toEqual(['A-model', 'c-model', 'd-model', 'b-model']);
    });

    it('sorts numbers ascending', () => {
      const out = sortRows(rows, { key: 'tokens', dir: 'asc' });
      expect(out.map((r) => r.key)).toEqual(['b-model', 'c-model', 'd-model', 'A-model']);
    });

    it('sorts strings case-insensitively', () => {
      const asc = sortRows(rows, { key: 'key', dir: 'asc' });
      expect(asc.map((r) => r.key)).toEqual(['A-model', 'b-model', 'c-model', 'd-model']);
      const desc = sortRows(rows, { key: 'key', dir: 'desc' });
      expect(desc.map((r) => r.key)).toEqual(['d-model', 'c-model', 'b-model', 'A-model']);
    });

    it('sinks unknown costs to the bottom in both directions', () => {
      const desc = sortRows(rows, { key: 'cost', dir: 'desc' });
      expect(desc.map((r) => r.key)).toEqual(['A-model', 'c-model', 'b-model', 'd-model']);
      const asc = sortRows(rows, { key: 'cost', dir: 'asc' });
      expect(asc.map((r) => r.key)).toEqual(['c-model', 'A-model', 'b-model', 'd-model']);
    });

    it('never mutates the input', () => {
      const copy = rows.map((r) => ({ ...r }));
      sortRows(rows, { key: 'tokens', dir: 'asc' });
      expect(rows).toEqual(copy);
    });
  });

  describe('toggleSort', () => {
    it('starts numeric columns descending and text columns ascending', () => {
      expect(toggleSort(null, 'tokens', true)).toEqual({ key: 'tokens', dir: 'desc' });
      expect(toggleSort(null, 'key', false)).toEqual({ key: 'key', dir: 'asc' });
    });

    it('flips the direction on a repeated click and resets on a new column', () => {
      const first = toggleSort(null, 'tokens', true);
      const second = toggleSort(first, 'tokens', true);
      expect(second).toEqual({ key: 'tokens', dir: 'asc' });
      expect(toggleSort(second, 'cost', true)).toEqual({ key: 'cost', dir: 'desc' });
    });
  });
});
