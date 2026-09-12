/**
 * F2-12 / F2-15: the drawer's Session panel derives every number from
 * `ChatSessionStore`, and the drawer's default demo state (Session + Explorer
 * open, Terminal closed) must survive a fresh install.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { Message, SessionMeta } from '../../core/engine.dtos';
import { UiPrefsStore } from '../../core/ui-prefs.store';
import {
  ChatSessionStore,
  DEFAULT_AUTO_COMPACT_PERCENT,
  DEFAULT_CONTEXT_WINDOW,
  UNKNOWN_COST_LABEL,
  compactBudgetFor,
  contextLevelFor,
  formatCost,
  formatTokens,
  shouldAutoCompact,
} from './chat-session.store';

function assistant(id: string, parts: Message['parts']): Message {
  return { id, role: 'assistant', parts } as Message;
}

function meta(extra: Partial<SessionMeta>): SessionMeta {
  return {
    id: 's1',
    directory: '/p',
    agent: 'code',
    created_at: 0,
    updated_at: 0,
    usage: { input_tokens: 0, output_tokens: 0 },
    ...extra,
  } as SessionMeta;
}

describe('ChatSessionStore', () => {
  let store: ChatSessionStore;

  beforeEach(() => {
    localStorage.clear();
    TestBed.configureTestingModule({ providers: [provideZonelessChangeDetection()] });
    store = TestBed.inject(ChatSessionStore);
  });

  it('sums usage parts into token totals, cost and a cache hit rate', () => {
    store.messages.set([
      assistant('m1', [
        {
          type: 'usage',
          input_tokens: 100,
          output_tokens: 40,
          cache_read_input_tokens: 300,
          cache_creation_input_tokens: 0,
          cost: 0.0125,
        },
      ]),
      assistant('m2', [
        {
          type: 'usage',
          input_tokens: 100,
          output_tokens: 60,
          cache_read_input_tokens: 100,
          cache_creation_input_tokens: 0,
          cost: 0.0075,
        },
      ]),
    ]);

    const totals = store.totals();
    expect(totals.input).toBe(200);
    expect(totals.output).toBe(100);
    expect(totals.cacheRead).toBe(400);
    expect(store.cacheRate()).toBe('66.7%');
    expect(store.costLabel()).toBe('$0.0200');
  });

  it('derives per-file +/- line counts from write_file and edit_file calls', () => {
    store.messages.set([
      assistant('m1', [
        {
          type: 'tool',
          name: 'write_file',
          state: {
            state: 'completed',
            input: { path: 'src/a.ts', content: 'one\ntwo\nthree' },
            output: 'ok',
          },
        },
        {
          type: 'tool',
          name: 'edit_file',
          state: {
            state: 'completed',
            input: { path: 'src/a.ts', old_string: 'two', new_string: 'two\ntwo-b' },
            output: 'ok',
          },
        },
        {
          type: 'tool',
          name: 'edit_file',
          state: { state: 'error', input: { path: 'src/b.ts', old_string: 'x', new_string: 'y' }, error: 'nope' },
        },
      ] as Message['parts']),
    ]);

    const files = store.filesChanged();
    expect(files.length).toBe(1);
    expect(files[0].path).toBe('src/a.ts');
    expect(files[0].added).toBe(5);
    expect(files[0].removed).toBe(1);
  });

  it('reports an unknown cost and an empty hit rate for a fresh session', () => {
    // No usage part has priced anything yet: "—", never a fake "$0.0000".
    expect(store.totals().cost).toBeNull();
    expect(store.costLabel()).toBe(UNKNOWN_COST_LABEL);
    expect(store.cacheRate()).toBe('–');
    expect(store.filesChanged()).toEqual([]);
  });

  it('shows "—" when every turn used an unknown-pricing model (F6-5)', () => {
    store.messages.set([
      assistant('m1', [{ type: 'usage', input_tokens: 100, output_tokens: 40 }]),
      assistant('m2', [{ type: 'usage', input_tokens: 100, output_tokens: 60, cost: null }]),
    ] as Message[]);
    expect(store.totals().input).toBe(200);
    expect(store.totals().cost).toBeNull();
    expect(store.costLabel()).toBe('—');
  });

  it('keeps a genuine zero cost as "$0.0000" (F6-5)', () => {
    store.messages.set([
      assistant('m1', [{ type: 'usage', input_tokens: 100, output_tokens: 40, cost: 0 }]),
    ]);
    expect(store.totals().cost).toBe(0);
    expect(store.costLabel()).toBe('$0.0000');
  });

  it('sums only the known costs when pricing is mixed (F6-5)', () => {
    store.messages.set([
      assistant('m1', [{ type: 'usage', input_tokens: 100, output_tokens: 40, cost: 0.01 }]),
      assistant('m2', [{ type: 'usage', input_tokens: 100, output_tokens: 60 }]),
      assistant('m3', [{ type: 'usage', input_tokens: 100, output_tokens: 60, cost: 0.02 }]),
    ] as Message[]);
    expect(store.totals().cost).toBeCloseTo(0.03, 10);
    expect(store.costLabel()).toBe('$0.0300');
    expect(formatCost(null)).toBe('—');
    expect(formatCost(1.23456)).toBe('$1.2346');
  });
});

describe('ChatSessionStore context meter (F6-3)', () => {
  let store: ChatSessionStore;

  beforeEach(() => {
    localStorage.clear();
    TestBed.configureTestingModule({ providers: [provideZonelessChangeDetection()] });
    store = TestBed.inject(ChatSessionStore);
  });

  it('has no meter before the first turn', () => {
    store.meta.set(meta({}));
    expect(store.contextUsed()).toBeNull();
    expect(store.contextPercent()).toBeNull();
    expect(store.contextLabel()).toBe('');
    expect(store.contextLevel()).toBe('ok');
    // Window still resolves (catalog fallback) so nothing renders NaN.
    expect(store.contextWindow()).toBe(DEFAULT_CONTEXT_WINDOW);
  });

  it('derives used / window / percent from the session meta', () => {
    store.meta.set(meta({ context_used: 42_000, context_window: 200_000 }));
    expect(store.contextUsed()).toBe(42_000);
    expect(store.contextWindow()).toBe(200_000);
    expect(store.contextPercent()).toBe(21);
    expect(store.contextLabel()).toBe('42k / 200k · 21%');
    expect(store.contextLevel()).toBe('ok');
  });

  it('falls back to the 64k window for an unknown model', () => {
    store.meta.set(meta({ context_used: 32_000, context_window: null }));
    expect(store.contextWindow()).toBe(64_000);
    expect(store.contextPercent()).toBe(50);
    expect(store.contextLabel()).toBe('32k / 64k · 50%');
  });

  it('switches colour bands at 80% (warning) and 95% (danger)', () => {
    store.meta.set(meta({ context_used: 158_000, context_window: 200_000 }));
    expect(store.contextLevel()).toBe('ok');
    store.meta.set(meta({ context_used: 160_000, context_window: 200_000 }));
    expect(store.contextPercent()).toBe(80);
    expect(store.contextLevel()).toBe('warning');
    store.meta.set(meta({ context_used: 190_000, context_window: 200_000 }));
    expect(store.contextLevel()).toBe('danger');
    expect(contextLevelFor(null)).toBe('ok');
    expect(contextLevelFor(79)).toBe('ok');
    expect(contextLevelFor(80)).toBe('warning');
    expect(contextLevelFor(95)).toBe('danger');
  });

  it('formats token counts compactly', () => {
    expect(formatTokens(950)).toBe('950');
    expect(formatTokens(42_000)).toBe('42k');
    expect(formatTokens(200_000)).toBe('200k');
    expect(formatTokens(1_048_576)).toBe('1M');
    expect(formatTokens(1_250_000)).toBe('1.3M');
  });
});

describe('ChatSessionStore auto-compaction threshold (F6-4)', () => {
  let store: ChatSessionStore;

  beforeEach(() => {
    localStorage.clear();
    TestBed.configureTestingModule({ providers: [provideZonelessChangeDetection()] });
    store = TestBed.inject(ChatSessionStore);
  });

  it('defaults to 85% and persists a change', () => {
    expect(store.autoCompactPercent()).toBe(DEFAULT_AUTO_COMPACT_PERCENT);
    store.setAutoCompactPercent(70);
    expect(localStorage.getItem('bebok.chat.autoCompactPercent')).toBe('70');
    const fresh = new ChatSessionStore();
    expect(fresh.autoCompactPercent()).toBe(70);
  });

  it('fires exactly at the threshold, never below it or before a turn', () => {
    expect(shouldAutoCompact(null, 85)).toBeFalse();
    expect(shouldAutoCompact(84, 85)).toBeFalse();
    expect(shouldAutoCompact(85, 85)).toBeTrue();
    expect(shouldAutoCompact(100, 85)).toBeTrue();
    // 0 disables the feature entirely.
    expect(shouldAutoCompact(100, 0)).toBeFalse();

    store.meta.set(meta({ context_used: 84_000, context_window: 100_000 }));
    expect(store.needsAutoCompact()).toBeFalse();
    store.meta.set(meta({ context_used: 85_000, context_window: 100_000 }));
    expect(store.needsAutoCompact()).toBeTrue();
    store.meta.set(meta({}));
    expect(store.needsAutoCompact()).toBeFalse();
  });

  it('derives the compaction budget from the live window and gates on length', () => {
    expect(compactBudgetFor(200_000)).toBe(100_000);
    expect(compactBudgetFor(1)).toBe(1);
    store.meta.set(meta({ context_used: 10, context_window: 64_000 }));
    expect(store.compactBudget()).toBe(32_000);
    store.messages.set([assistant('a', []), assistant('b', []), assistant('c', [])]);
    expect(store.canCompact()).toBeFalse();
    store.messages.set([
      assistant('a', []),
      assistant('b', []),
      assistant('c', []),
      assistant('d', []),
    ]);
    expect(store.canCompact()).toBeTrue();
  });
});

describe('right drawer default state (F2-15)', () => {
  beforeEach(() => {
    localStorage.clear();
    TestBed.configureTestingModule({ providers: [provideZonelessChangeDetection()] });
  });

  it('opens Session + Explorer and leaves Terminal closed', () => {
    const prefs = TestBed.inject(UiPrefsStore);
    expect(prefs.rightDrawerOpen()).toBeTrue();
    expect(prefs.rightDrawerPanels()).toEqual({
      session: true,
      explorer: true,
      terminal: false,
      agents: false,
      changes: false,
      preview: false,
      browser: false,
    });
  });
});
