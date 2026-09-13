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
import { EventsStore, parseSseBlock } from './events.store';
import { EngineConnection } from './transport.strategy';

/** A `fetch` stub that keeps every `/event` stream open until aborted or closed. */
interface OpenStream {
  url: string;
  signal: AbortSignal;
  /** Request headers as sent (F10-30: `Last-Event-ID`). */
  headers: Record<string, string>;
  push: (block: string) => void;
  /** End the stream the way a restarting engine would (clean EOF). */
  close: () => void;
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
    const headers: Record<string, string> = {};
    new Headers(init.headers).forEach((value, key) => (headers[key.toLowerCase()] = value));
    streams.push({
      url,
      signal,
      headers,
      push: (block: string) => controller?.enqueue(new TextEncoder().encode(block)),
      close: () => {
        try {
          controller?.close();
        } catch {
          /* already closed */
        }
      },
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

  it('relaunches a dead embedded engine instead of retrying its old port (idle auto-stop)', async () => {
    store.reconnectDelayMs = 5;
    store.start();
    await flush();
    expect(engine.connect).toHaveBeenCalledTimes(1);
    // The platform target is the embedded engine (Capacitor).
    targets.upsert({ ...targets.byId('a')!, kind: 'embedded' });
    // The native side killed it: the stream drops and the port is dead.
    fetchSpy.and.callFake(() => Promise.reject(new TypeError('Failed to fetch')));
    streams[0].close();
    await new Promise((resolve) => setTimeout(resolve, 30));
    await flush();
    // The connection was forgotten, so the loop went through connect()
    // again (= EngineLauncher.start() on the phone) instead of hammering
    // the old URL forever.
    expect(engine.connect).toHaveBeenCalledTimes(2);
    expect(store.state()).not.toBe('live');
  });

  it('keeps retrying the same address for a non-embedded target', async () => {
    store.reconnectDelayMs = 5;
    store.start();
    await flush();
    fetchSpy.and.callFake(() => Promise.reject(new TypeError('Failed to fetch')));
    streams[0].close();
    await new Promise((resolve) => setTimeout(resolve, 30));
    await flush();
    expect(engine.connect).toHaveBeenCalledTimes(1);
    expect(engine.connection()).not.toBeNull();
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

/** F10-30: `id:` / `event:` parsing, `Last-Event-ID` resume, `resync`, `revoked`. */
describe('EventsStore SSE ids and named events (F10-30)', () => {
  let streams: OpenStream[];
  let targets: EngineTargetStore;
  let engine: EngineApi;
  let store: EventsStore;

  /** Wait for the retry back-off (shortened below) plus the fetch to settle. */
  async function reconnect(): Promise<void> {
    await new Promise((resolve) => setTimeout(resolve, 5));
    await flush(10);
  }

  beforeEach(() => {
    localStorage.clear();
    setEngineToken(null);
    streams = [];
    spyOn(window, 'fetch').and.callFake(streamingFetch(streams));
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        { provide: ENGINE_API, useFactory: () => fakeEngine(TestBed.inject(EngineTargetStore)) },
      ],
    });
    targets = TestBed.inject(EngineTargetStore);
    engine = TestBed.inject(ENGINE_API);
    store = TestBed.inject(EventsStore);
    store.reconnectDelayMs = 1;
  });

  afterEach(() => {
    store.stop();
    localStorage.clear();
    setEngineToken(null);
  });

  describe('parseSseBlock', () => {
    it('reads id, event and data fields and skips comments', () => {
      expect(parseSseBlock(': keep-alive\nid: 42\nevent: resync\ndata:')).toEqual({
        id: '42',
        event: 'resync',
        data: '',
      });
    });

    it('joins multi-line data and tolerates a missing space after the colon', () => {
      expect(parseSseBlock('data:{"a":\ndata: 1}\nid:7')).toEqual({
        id: '7',
        event: null,
        data: '{"a":\n1}',
      });
    });

    it('ignores unknown fields, a field without a colon and NUL ids', () => {
      expect(parseSseBlock(`retry: 100\nfoo\nid: 1${String.fromCharCode(0)}\ndata: x`)).toEqual({
        id: null,
        event: null,
        data: 'x',
      });
    });
  });

  it('remembers the last id and fans out the data regardless', async () => {
    const seen: string[] = [];
    store.onEvent((ev) => seen.push(ev.type));
    store.start();
    await flush();
    expect(streams[0].headers['last-event-id']).toBeUndefined();

    streams[0].push('id: 10\ndata: {"type":"session.created","properties":{}}\n\n');
    streams[0].push('id: 11\ndata: {"type":"session.updated","properties":{}}\n\n');
    await flush();

    expect(seen).toEqual(['session.created', 'session.updated']);
    expect(store.resumeId()).toBe('11');
  });

  it('resumes with Last-Event-ID after the stream drops and does not force a full refresh', async () => {
    store.start();
    await flush();
    streams[0].push('id: 5\ndata: {"type":"session.updated","properties":{}}\n\n');
    await flush();
    expect(store.reconnectVersion()).toBe(1);

    // The engine closed the stream (restart / network blip): reconnect.
    streams[0].close();
    await reconnect();

    expect(streams.length).toBe(2);
    expect(streams[1].url).toBe('http://engine-a:1/event');
    expect(streams[1].headers['last-event-id']).toBe('5');
    expect(store.state()).toBe('live');
    // The replay fills the gap: the views keep their state.
    expect(store.reconnectVersion()).toBe(1);
  });

  it('bumps reconnectVersion on `event: resync` (gap older than the engine buffer)', async () => {
    const seen: string[] = [];
    store.onEvent((ev) => seen.push(ev.type));
    store.start();
    await flush();
    streams[0].push('id: 5\ndata: {"type":"session.updated","properties":{}}\n\n');
    await flush();
    streams[0].close();
    await reconnect();
    expect(streams[1].headers['last-event-id']).toBe('5');

    streams[1].push('event: resync\ndata: \n\n');
    streams[1].push('id: 900\ndata: {"type":"session.created","properties":{}}\n\n');
    await flush();

    expect(store.reconnectVersion()).toBe(2);
    expect(seen).toEqual(['session.updated', 'session.created']);
    expect(store.resumeId()).toBe('900');
  });

  it('does not resume against a different engine (target switch)', async () => {
    store.start();
    await flush();
    streams[0].push('id: 5\ndata: {"type":"session.updated","properties":{}}\n\n');
    await flush();

    targets.upsert({ id: 'b', kind: 'desktop', label: 'B', baseUrl: 'http://engine-b:2', token: 'tok' });
    await engine.switchTarget('b');
    TestBed.tick();
    await flush(10);

    expect(streams.length).toBe(2);
    expect(streams[1].url).toBe('http://engine-b:2/event');
    expect(streams[1].headers['last-event-id']).toBeUndefined();
    expect(store.reconnectVersion()).toBe(2);
    expect(store.resumeId()).toBeNull();
  });

  it('parks as unauthorized on `event: revoked` without retrying', async () => {
    const seen: string[] = [];
    store.onEvent((ev) => seen.push(ev.type));
    store.start();
    await flush();

    streams[0].push('id: 3\ndata: {"type":"session.updated","properties":{}}\n\n');
    streams[0].push('event: revoked\ndata: \n\n');
    await reconnect();

    expect(seen).toEqual(['session.updated']);
    expect(store.state()).toBe('unauthorized');
    expect(store.revokedVersion()).toBe(1);
    expect(store.resumeId()).toBeNull();
    // Parked: no reconnect attempt until `restart()`.
    expect(streams.length).toBe(1);
  });

  it('ignores unknown named events and treats `event: message` as data', async () => {
    const seen: string[] = [];
    store.onEvent((ev) => seen.push(ev.type));
    store.start();
    await flush();

    streams[0].push('event: fancy-new-thing\ndata: {"type":"session.deleted","properties":{}}\n\n');
    streams[0].push('event: message\ndata: {"type":"session.updated","properties":{}}\n\n');
    await flush();

    expect(seen).toEqual(['session.updated']);
    expect(store.state()).toBe('live');
  });
});
