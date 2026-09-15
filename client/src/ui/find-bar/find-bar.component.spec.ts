/**
 * Find bar wiring: query/toggle rendering and store integration.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { FindStore } from '../../core/find.store';
import { FindBarComponent } from './find-bar.component';

describe('FindBarComponent', () => {
  let store: FindStore;

  beforeEach(() => {
    TestBed.configureTestingModule({ providers: [provideZonelessChangeDetection()] });
    store = TestBed.inject(FindStore);
  });

  function create(text: string, container: HTMLElement | null = null): FindBarComponent {
    const fixture = TestBed.createComponent(FindBarComponent);
    fixture.componentRef.setInput('text', text);
    fixture.componentRef.setInput('container', container);
    fixture.detectChanges();
    return fixture.componentInstance;
  }

  it('opens the store for its scope on init', () => {
    const bar = create('hello');
    expect(store.open()).toBeTrue();
    expect(store.activeScope()).toBe('explorer');
    expect(bar).toBeTruthy();
  });

  it('scanning "lorem lorem" reports two matches', () => {
    create('lorem lorem');
    store.setQuery('lorem');
    TestBed.tick(0);
    expect(store.total()).toBe(2);
  });

  it('an invalid regex sets the error flag instead of crashing', () => {
    const bar = create('lorem');
    store.toggleUseRegex();
    store.setQuery('([');
    TestBed.tick(0);
    expect(bar.regexError()).toBeTrue();
    expect(store.total()).toBe(0);
    expect(bar.counterText()).toBe('Invalid pattern');
  });

  it('counter shows the active position and wraps with next()/prev()', () => {
    create('a a a');
    store.setQuery('a');
    TestBed.tick(0);
    expect(bar_counter(store)).toBe('1/3');
    store.next();
    expect(bar_counter(store)).toBe('2/3');
    store.next();
    store.next();
    expect(bar_counter(store)).toBe('1/3');
    store.prev();
    expect(bar_counter(store)).toBe('3/3');
  });

  it('close() hides the bar and keeps the query', () => {
    const bar = create('lorem lorem');
    store.setQuery('lorem');
    bar.close();
    expect(store.open()).toBeFalse();
    expect(store.query()).toBe('lorem');
  });

  function bar_counter(s: FindStore): string {
    return `${s.activeIndex() + 1}/${s.total()}`;
  }
});
