/**
 * Find bar state (Ctrl+F): opening/closing, the query and its toggles, and
 * the match bookkeeping (count, active index, cap flag, wrap-around).
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { FindStore } from './find.store';

describe('FindStore', () => {
  let store: FindStore;

  beforeEach(() => {
    TestBed.configureTestingModule({ providers: [provideZonelessChangeDetection()] });
    store = TestBed.inject(FindStore);
  });

  it('starts closed, empty and without matches', () => {
    expect(store.open()).toBeFalse();
    expect(store.query()).toBe('');
    expect(store.activeScope()).toBeNull();
    expect(store.total()).toBe(0);
    expect(store.activeIndex()).toBe(0);
    expect(store.limited()).toBeFalse();
    expect(store.hasMatches()).toBeFalse();
    expect(store.options()).toEqual({ matchCase: false, wholeWord: false, useRegex: false });
  });

  it('openFind() opens the bar for a scope and resets the match state', () => {
    store.setMatches(7, true);
    store.setActive(3);
    store.openFind('chat:session-1');
    expect(store.open()).toBeTrue();
    expect(store.activeScope()).toBe('chat:session-1');
    expect(store.total()).toBe(0);
    expect(store.activeIndex()).toBe(0);
    expect(store.limited()).toBeFalse();
  });

  it('openFind() on an already open bar keeps the query', () => {
    store.openFind('chat:session-1');
    store.setQuery('needle');
    store.setMatches(2, false);
    store.openFind('chat:session-1');
    expect(store.open()).toBeTrue();
    expect(store.query()).toBe('needle');
    expect(store.total()).toBe(2);
    expect(store.activeScope()).toBe('chat:session-1');
  });

  it('closeFind() hides the bar, clears matches and keeps the query', () => {
    store.openFind('chat:session-1');
    store.setQuery('needle');
    store.setMatches(3, false);
    store.closeFind();
    expect(store.open()).toBeFalse();
    expect(store.activeScope()).toBeNull();
    expect(store.total()).toBe(0);
    expect(store.activeIndex()).toBe(0);
    expect(store.hasMatches()).toBeFalse();
    expect(store.query()).toBe('needle');
  });

  it('setQuery() resets the active index', () => {
    store.setMatches(5, false);
    store.setActive(4);
    store.setQuery('cat');
    expect(store.query()).toBe('cat');
    expect(store.activeIndex()).toBe(0);
  });

  it('toggles the three options independently', () => {
    store.toggleMatchCase();
    store.toggleWholeWord();
    expect(store.matchCase()).toBeTrue();
    expect(store.wholeWord()).toBeTrue();
    expect(store.useRegex()).toBeFalse();
    store.toggleUseRegex();
    store.toggleWholeWord();
    expect(store.options()).toEqual({ matchCase: true, wholeWord: false, useRegex: true });
  });

  it('setMatches() clamps the active index and reports the cap flag', () => {
    store.setMatches(3, false);
    store.setActive(2);
    store.setMatches(2, true);
    expect(store.total()).toBe(2);
    expect(store.activeIndex()).toBe(1);
    expect(store.limited()).toBeTrue();
    expect(store.hasMatches()).toBeTrue();
    store.setMatches(0, true);
    expect(store.total()).toBe(0);
    expect(store.activeIndex()).toBe(0);
    expect(store.limited()).toBeFalse();
    expect(store.hasMatches()).toBeFalse();
  });

  it('next() and prev() wrap around the ends', () => {
    store.setMatches(3, false);
    expect(store.activeIndex()).toBe(0);
    store.next();
    expect(store.activeIndex()).toBe(1);
    store.next();
    expect(store.activeIndex()).toBe(2);
    store.next();
    expect(store.activeIndex()).toBe(0);
    store.prev();
    expect(store.activeIndex()).toBe(2);
    store.prev();
    expect(store.activeIndex()).toBe(1);
  });

  it('next()/prev()/setActive() are no-ops without matches', () => {
    store.next();
    store.prev();
    store.setActive(4);
    expect(store.activeIndex()).toBe(0);
  });

  it('setActive() clamps out-of-range indexes', () => {
    store.setMatches(4, false);
    store.setActive(99);
    expect(store.activeIndex()).toBe(3);
    store.setActive(-5);
    expect(store.activeIndex()).toBe(0);
  });
});
