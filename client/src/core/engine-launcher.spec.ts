/**
 * WP-M5 / F10-21: `EngineWorkTracker` keeps `beginWork`/`endWork` balanced
 * across a successful turn, an aborted turn and a failed prompt, releases
 * on the SSE `session.updated { running: false }` signal, and is a no-op
 * outside Capacitor. Also: the debug launch options for the mock provider.
 */

import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';

import {
  EngineWorkBridge,
  EngineWorkTracker,
  MOCK_PROVIDER_KEY,
  embeddedLaunchOptions,
  readMockProviderFlag,
  writeMockProviderFlag,
} from './engine-launcher';
import { EventsStore } from './events.store';
import { EngineEvent } from './engine.dtos';

type Listener = (event: EngineEvent) => void;

describe('EngineWorkTracker (F10-21)', () => {
  let listeners: Listener[];
  let calls: string[];
  let bridge: EngineWorkBridge;
  let tracker: EngineWorkTracker;

  function emit(sessionID: string, running: boolean): void {
    for (const l of listeners) {
      l({ type: 'session.updated', directory: '/p', sessionID, properties: { running } });
    }
  }

  beforeEach(() => {
    listeners = [];
    calls = [];
    bridge = {
      beginWork: async (o) => {
        calls.push(`begin:${o?.reason ?? ''}`);
      },
      endWork: async () => {
        calls.push('end');
      },
    };
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        {
          provide: EventsStore,
          useValue: {
            onEvent: (l: Listener) => {
              listeners.push(l);
              return () => listeners.splice(listeners.indexOf(l), 1);
            },
          },
        },
      ],
    });
    tracker = TestBed.inject(EngineWorkTracker);
    tracker.configure({ native: true, bridge });
  });

  it('balances a successful turn: begin on send, end on session idle', async () => {
    tracker.begin('s1');
    expect(tracker.active()).toEqual(['s1']);
    emit('s1', true); // still running - no release
    emit('s1', false);
    await tracker.settled();
    expect(calls).toEqual(['begin:turn', 'end']);
    expect(tracker.active()).toEqual([]);
  });

  it('balances an aborted turn (explicit end + the SSE echo)', async () => {
    tracker.begin('s1');
    tracker.end('s1'); // abortTurn()
    emit('s1', false); // engine confirms
    await tracker.settled();
    expect(calls).toEqual(['begin:turn', 'end']);
  });

  it('balances a failed prompt (end without any SSE)', async () => {
    tracker.begin('s1');
    tracker.end('s1');
    await tracker.settled();
    expect(calls).toEqual(['begin:turn', 'end']);
    expect(tracker.active()).toEqual([]);
  });

  it('never double-counts a session and tracks sessions independently', async () => {
    tracker.begin('s1');
    tracker.begin('s1');
    tracker.begin('s2');
    emit('s3', false); // unrelated session
    emit('s1', false);
    await tracker.settled();
    expect(calls).toEqual(['begin:turn', 'begin:turn', 'end']);
    expect(tracker.active()).toEqual(['s2']);
    tracker.endAll();
    await tracker.settled();
    expect(calls).toEqual(['begin:turn', 'begin:turn', 'end', 'end']);
  });

  it('keeps going after a bridge failure', async () => {
    const warn = spyOn(console, 'warn');
    tracker.configure({
      bridge: {
        beginWork: async () => {
          throw new Error('not implemented');
        },
        endWork: bridge.endWork,
      },
    });
    tracker.begin('s1');
    tracker.end('s1');
    await tracker.settled();
    expect(warn).toHaveBeenCalled();
    expect(calls).toEqual(['end']);
  });

  it('is a no-op outside Capacitor', async () => {
    tracker.configure({ native: false });
    tracker.begin('s1');
    tracker.end('s1');
    await tracker.settled();
    expect(calls).toEqual([]);
    expect(listeners.length).toBe(0);
  });
});

describe('embedded launch options (mock provider toggle)', () => {
  afterEach(() => localStorage.removeItem(MOCK_PROVIDER_KEY));

  it('passes BEBOK_PROVIDER_MOCK only when the developer toggle is on', () => {
    writeMockProviderFlag(false);
    expect(readMockProviderFlag()).toBeFalse();
    expect(embeddedLaunchOptions()).toEqual({});
    writeMockProviderFlag(true);
    expect(readMockProviderFlag()).toBeTrue();
    expect(embeddedLaunchOptions()).toEqual({ env: { BEBOK_PROVIDER_MOCK: '1' } });
  });
});
