/**
 * WP-M2: `EventsStore` depends on the `ENGINE_API` contract (F10-6) and
 * follows the active engine target (F10-7): a `switchTarget()` aborts the
 * stream against the old engine and opens one against the new base URL.
 */

import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection, signal } from '@angular/core';

import { setEngineToken } from './auth.interceptor';
import { ENGINE_API, EngineApi } from './engine-api';
import { EngineTargetStore } from './engine-target.store';
import { EventsStore } from './events.store';
import { EngineConnection } from './transport.strategy';

/** A `fetch` stub that keeps every `/event` stream open until aborted. */
interface OpenStream {
  url: string;
  signal: AbortSignal;
  push: (block: string) => void;
}

function streamingFetch(streams: OpenStream[]): jasmine.Spy {
  return jasmine.createSpy('fetch').and.callFake((url: string, init: RequestInit = {}) => {
    const signal = init.signal as AbortSignal;
    let controller: ReadableStreamDefaultController<Uint8Array> | null = null;
    const body = new ReadableStream<Uint8Array>({
      start(c) {
        controller = c;
        signal.addEventListener('abort', () => {
          try {
            c.error(new DOMException('aborted', 'AbortError'));
          } catch {
            /* already closed */
          }
        });
      },
    });
    streams.push({
      url,
      signal,
      push: (block: string) => controller?.enqueue(new TextEncoder().encode(block)),
    });
    return Promise.resolve(new Response(body, { status: 200 }));
  });
}

function fakeEngine(targets: EngineTargetStore): EngineApi {
  const connection = signal<EngineConnection | null>(null);
  const api = {
    connection,
    connected: () => connection() !== null,
    isTauri: signal(false),
    unauthorized: signal(false),
    platform: 'http',
    isCapacitor: false,
    connect: jasmine.createSpy('connect').and.callFake(async () => {
      const conn = { kind: 'http', baseUrl: 'http://engine-a:1' } as EngineConnection;
      targets.upsert({
        id: 'a',
        kind: 'remote-url',
        label: 'A',
        baseUrl: conn.baseUrl,
        token: null,
        ephemeral: true,
      });
      targets.setActive('a');
      connection.set(conn);
      return conn;
    }),
    switchTarget: jasmine.createSpy('switchTarget').and.callFake(async (id: string) => {
      const target = targets.byId(id)!;
      connection.set({ kind: 'http', baseUrl: target.baseUrl });
      targets.setActive(id);
      return target;
    }),
  };
  return api as unknown as EngineApi;
}

async function flush(times = 5): Promise<void> {
  for (let i = 0; i < times; i++) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

describe('EventsStore (WP-M2)', () => {
  let streams: OpenStream[];
  let fetchSpy: jasmine.Spy;
  let targets: EngineTargetStore;
  let engine: EngineApi;
  let store: EventsStore;

  beforeEach(() => {
    localStorage.clear();
    setEngineToken(null);
    streams = [];
    fetchSpy = streamingFetch(streams);
    spyOn(window, 'fetch').and.callFake(fetchSpy);
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        // The store must see the engine through the token, not the class.
        { provide: ENGINE_API, useFactory: () => fakeEngine(TestBed.inject(EngineTargetStore)) },
      ],
    });
    targets = TestBed.inject(EngineTargetStore);
    engine = TestBed.inject(ENGINE_API);
    store = TestBed.inject(EventsStore);
  });

  afterEach(() => {
    store.stop();
    localStorage.clear();
    setEngineToken(null);
  });

  it('uses the EngineApi provided through ENGINE_API (F10-6)', async () => {
    store.start();
    await flush();

    expect(engine.connect).toHaveBeenCalled();
    expect(streams.length).toBe(1);
    expect(streams[0].url).toBe('http://engine-a:1/event');
    expect(store.state()).toBe('live');
    expect(store.reconnectVersion()).toBe(1);
  });

  it('fans out parsed events to listeners', async () => {
    const seen: string[] = [];
    store.onEvent((ev) => seen.push(ev.type));
    store.start();
    await flush();

    streams[0].push('data: {"type":"session.updated","properties":{}}\n\n');
    await flush();
    expect(seen).toEqual(['session.updated']);
  });

  it('aborts the old stream and reconnects on switchTarget (F10-7)', async () => {
    store.start();
    await flush();
    expect(streams.length).toBe(1);
    const first = streams[0];

    targets.upsert({
      id: 'b',
      kind: 'desktop',
      label: 'B',
      baseUrl: 'http://engine-b:2',
      token: 'tok-b',
    });
    await engine.switchTarget('b');
    // The store watches `activeId` through an effect: flush it, then let the
    // reconnect loop spin.
    TestBed.tick();
    await flush(10);

    expect(first.signal.aborted).toBeTrue();
    expect(streams.length).toBe(2);
    expect(streams[1].url).toBe('http://engine-b:2/event');
    expect(streams[1].signal.aborted).toBeFalse();
    expect(store.state()).toBe('live');
    expect(store.reconnectVersion()).toBe(2);
    expect(targets.byId('b')?.lastOk).toBeDefined();
  });

  it('does not restart when the target changes before the stream was started', async () => {
    targets.upsert({ id: 'x', kind: 'desktop', label: 'X', baseUrl: 'http://x:1', token: null });
    targets.setActive('x');
    TestBed.tick();
    await flush();
    expect(fetchSpy).not.toHaveBeenCalled();
    expect(store.state()).toBe('idle');
  });
});
