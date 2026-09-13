/**
 * F9-5: the toast store keeps a signal list, auto-dismisses after the TTL
 * and drops a toast on demand.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { DEFAULT_TOAST_TTL_MS, ToastStore } from './toast.store';

describe('ToastStore (F9-5)', () => {
  let store: ToastStore;

  beforeEach(() => {
    jasmine.clock().install();
    TestBed.configureTestingModule({ providers: [provideZonelessChangeDetection()] });
    store = TestBed.inject(ToastStore);
  });

  afterEach(() => {
    store.clear();
    jasmine.clock().uninstall();
  });

  it('starts empty and appends toasts with the default kind', () => {
    expect(store.toasts()).toEqual([]);
    const id = store.show('write_file allowed');
    expect(store.toasts().length).toBe(1);
    expect(store.toasts()[0]).toEqual(
      jasmine.objectContaining({ id, text: 'write_file allowed', kind: 'info' }),
    );
    store.show('boom', { kind: 'danger' });
    expect(store.toasts().map((t) => t.kind)).toEqual(['info', 'danger']);
  });

  it('auto-dismisses after the default TTL (5 s)', () => {
    store.show('gone soon');
    jasmine.clock().tick(DEFAULT_TOAST_TTL_MS - 1);
    expect(store.toasts().length).toBe(1);
    jasmine.clock().tick(1);
    expect(store.toasts().length).toBe(0);
  });

  it('honours a custom TTL and keeps a toast with ttlMs 0 until dismissed', () => {
    store.show('short', { ttlMs: 100 });
    const sticky = store.show('sticky', { ttlMs: 0 });
    jasmine.clock().tick(100);
    expect(store.toasts().map((t) => t.text)).toEqual(['sticky']);
    jasmine.clock().tick(60_000);
    expect(store.toasts().map((t) => t.text)).toEqual(['sticky']);
    store.dismiss(sticky);
    expect(store.toasts()).toEqual([]);
  });

  it('dismisses one toast by id without touching the others', () => {
    const a = store.show('a');
    store.show('b');
    store.dismiss(a);
    expect(store.toasts().map((t) => t.text)).toEqual(['b']);
    // Dismissing an unknown id is a no-op (same array instance).
    const before = store.toasts();
    store.dismiss(999);
    expect(store.toasts()).toBe(before);
  });

  it('caps the stack at five, dropping the oldest', () => {
    for (let i = 0; i < 7; i++) {
      store.show(`t${i}`);
    }
    expect(store.toasts().map((t) => t.text)).toEqual(['t2', 't3', 't4', 't5', 't6']);
  });
});
