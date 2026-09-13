/**
 * F6-2: windowed transcript.
 *
 * `messages` stays the full history (index-based SSE patches rely on it);
 * only the rendered slice is windowed to the newest `MESSAGE_WINDOW` rows and
 * grown by "Load earlier messages".
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { Message } from '../../core/engine.dtos';
import { ChatView, MESSAGE_WINDOW, windowMessages } from './chat';

function history(count: number): Message[] {
  return Array.from({ length: count }, (_, i) => ({
    id: `m-${i}`,
    role: i % 2 === 0 ? 'user' : 'assistant',
    parts: [{ type: 'text', text: `message ${i}` }],
  }));
}

describe('windowMessages (F6-2)', () => {
  it('returns the whole list when it fits the window', () => {
    const list = history(10);
    expect(windowMessages(list, 60)).toEqual(list);
  });

  it('keeps only the newest entries otherwise', () => {
    const list = history(100);
    const windowed = windowMessages(list, 60);
    expect(windowed.length).toBe(60);
    expect(windowed[0].id).toBe('m-40');
    expect(windowed[59].id).toBe('m-99');
  });

  it('is empty for a non-positive window', () => {
    expect(windowMessages(history(5), 0)).toEqual([]);
  });
});

describe('ChatView transcript window (F6-2)', () => {
  let view: ChatView;

  beforeEach(() => {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [ChatView],
      providers: [provideZonelessChangeDetection(), provideRouter([])],
    });
    // No detectChanges(): ngOnInit (engine connect + SSE) is not exercised
    // here, only the signal graph that drives the rendered slice.
    view = TestBed.createComponent(ChatView).componentInstance;
  });

  it('renders at most MESSAGE_WINDOW newest messages initially', () => {
    expect(MESSAGE_WINDOW).toBe(60);
    view.messages.set(history(150));

    expect(view.windowedMessages().length).toBe(MESSAGE_WINDOW);
    expect(view.windowedMessages()[0].id).toBe('m-90');
    expect(view.windowedMessages().at(-1)!.id).toBe('m-149');
    expect(view.hiddenCount()).toBe(90);
  });

  it('renders a short session in full with nothing hidden', () => {
    view.messages.set(history(12));
    expect(view.windowedMessages().length).toBe(12);
    expect(view.hiddenCount()).toBe(0);
  });

  it('grows the window by MESSAGE_WINDOW per "Load earlier" until everything is shown', () => {
    view.messages.set(history(150));

    view.loadEarlier();
    expect(view.windowedMessages().length).toBe(120);
    expect(view.windowedMessages()[0].id).toBe('m-30');
    expect(view.hiddenCount()).toBe(30);

    view.loadEarlier();
    expect(view.windowedMessages().length).toBe(150);
    expect(view.hiddenCount()).toBe(0);

    // Nothing left to reveal: the count stays put.
    const before = view.visibleCount();
    view.loadEarlier();
    expect(view.visibleCount()).toBe(before);
  });

  it('keeps the full history as the source of truth for index-based patches', () => {
    view.messages.set(history(100));
    view.loadEarlier();

    // A snapshot of message 5 (outside the initial window) replaces index 5
    // in the full array and shows up once the window covers it.
    view.messages.update((list) => {
      const next = [...list];
      next[5] = { ...next[5], parts: [{ type: 'text', text: 'patched' }] };
      return next;
    });
    expect(view.messages().length).toBe(100);
    const rendered = view.windowedMessages().find((m) => m.id === 'm-5');
    expect(rendered?.parts[0]).toEqual({ type: 'text', text: 'patched' });
  });

  it('applies the model filter before windowing', () => {
    const list = history(100).map((m, i) => ({
      ...m,
      meta: { model: i < 70 ? 'a' : 'b' },
    }));
    view.messages.set(list);
    view.filterModel.set('b');

    expect(view.windowedMessages().length).toBe(30);
    expect(view.hiddenCount()).toBe(0);
    expect(view.windowedMessages().every((m) => m.meta?.model === 'b')).toBeTrue();
  });
});
