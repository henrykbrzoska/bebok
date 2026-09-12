/**
 * F2-12 / F2-15: the drawer's Session panel derives every number from
 * `ChatSessionStore`, and the drawer's default demo state (Session + Explorer
 * open, Terminal closed) must survive a fresh install.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { Message } from '../../core/engine.dtos';
import { UiPrefsStore } from '../../core/ui-prefs.store';
import { ChatSessionStore } from './chat-session.store';

function assistant(id: string, parts: Message['parts']): Message {
  return { id, role: 'assistant', parts } as Message;
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

  it('reports a zero cost and an empty hit rate for a fresh session', () => {
    expect(store.costLabel()).toBe('$0.0000');
    expect(store.cacheRate()).toBe('–');
    expect(store.filesChanged()).toEqual([]);
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
    });
  });
});
