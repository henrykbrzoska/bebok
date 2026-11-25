/**
 * EventsStore lifecycle: the idle state before and after a run.
 */

import { TestBed } from '@angular/core/testing';

import { EventsStore } from './events.store';
import { EngineClient } from './engine-client.service';

describe('EventsStore', () => {
  let store: EventsStore;

  beforeEach(() => {
    TestBed.configureTestingModule({
      providers: [EventsStore, EngineClient],
    });
    store = TestBed.inject(EventsStore);
  });

  afterEach(() => {
    store.stop();
  });

  it('starts in idle state', () => {
    expect(store.state()).toBe('idle');
  });

  it('stop() sets state to idle', () => {
    store.start();
    store.stop();
    expect(store.state()).toBe('idle');
  });
});
