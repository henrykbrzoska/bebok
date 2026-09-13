/**
 * WP-M6 (F10-27): offline cache read/write/eviction and the queued-command
 * expiry (fake timers).
 */

import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection, signal } from '@angular/core';

import { ENGINE_API, EngineApi } from '../engine-api';
import { EngineTargetStore } from '../engine-target.store';
import type { Message, SessionMeta } from '../engine.dtos';
import { EventsStore } from '../events.store';
import {
  MAX_CACHED_MESSAGES,
  MAX_CACHED_TRANSCRIPTS,
  MemoryCacheStore,
  OFFLINE_CACHE_CIPHER,
  OFFLINE_CACHE_STORE,
  OfflineCache,
  OfflineQueue,
  QUEUE_TICK_MS,
  QUEUE_TTL_MS,
  SubtleCacheCipher,
  isNetworkError,
} from './offline-cache';

function msg(i: number): Message {
  return { id: `m${i}`, role: i % 2 ? 'assistant' : 'user', parts: [{ type: 'text', text: `t${i}` }] };
}

function session(id: string): SessionMeta {
  return {
    id,
    directory: 'C:/p',
    agent: 'code',
    created_at: 1,
    updated_at: 2,
    usage: { input_tokens: 0, output_tokens: 0 },
  };
}

describe('OfflineCache (WP-M6 / F10-27)', () => {
  let cache: OfflineCache;
  let store: MemoryCacheStore;
  let targets: EngineTargetStore;

  beforeEach(() => {
    localStorage.clear();
    store = new MemoryCacheStore();
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        { provide: OFFLINE_CACHE_STORE, useValue: store },
        { provide: OFFLINE_CACHE_CIPHER, useValue: new SubtleCacheCipher() },
      ],
    });
    cache = TestBed.inject(OfflineCache);
    targets = TestBed.inject(EngineTargetStore);
    targets.upsert({ id: 'desktop:1', kind: 'desktop', label: 'd', baseUrl: 'http://1', token: 'secret-1' });
  });

  afterEach(() => localStorage.clear());

  it('round-trips the session list, sealed with the target token', async () => {
    await cache.putSessions('desktop:1', [session('s1'), session('s2')]);
    const raw = store.map.get('sessions:desktop:1')!;
    expect(raw.startsWith('v1:')).toBeTrue();
    expect(raw).not.toContain('"id":"s1"');
    const back = await cache.getSessions('desktop:1');
    expect(back?.sessions.map((s) => s.id)).toEqual(['s1', 's2']);
  });

  it('stores the transcript tail (last 200) and flags truncation', async () => {
    const messages = Array.from({ length: MAX_CACHED_MESSAGES + 25 }, (_, i) => msg(i));
    await cache.putTranscript('desktop:1', 's1', messages);
    const back = await cache.getTranscript('desktop:1', 's1');
    expect(back?.messages.length).toBe(MAX_CACHED_MESSAGES);
    expect(back?.messages[0].id).toBe('m25');
    expect(back?.truncated).toBeTrue();
  });

  it('cannot be opened with another token (wrong key -> null, never garbage)', async () => {
    await cache.putTranscript('desktop:1', 's1', [msg(1)]);
    targets.upsert({ id: 'desktop:1', kind: 'desktop', label: 'd', baseUrl: 'http://1', token: 'other' });
    expect(await cache.getTranscript('desktop:1', 's1')).toBeNull();
  });

  it('falls back to marked plaintext without a token', async () => {
    targets.upsert({ id: 'desktop:2', kind: 'desktop', label: 'd', baseUrl: 'http://2', token: null });
    await cache.putSessions('desktop:2', [session('s9')]);
    expect(store.map.get('sessions:desktop:2')!.startsWith('p:')).toBeTrue();
    expect((await cache.getSessions('desktop:2'))?.sessions[0].id).toBe('s9');
  });

  it('evicts the least recently saved transcripts beyond the per-target cap', async () => {
    let now = 1000;
    cache.now = () => now;
    for (let i = 0; i < MAX_CACHED_TRANSCRIPTS + 3; i++) {
      now += 1;
      await cache.putTranscript('desktop:1', `s${i}`, [msg(i)]);
    }
    expect(await cache.getTranscript('desktop:1', 's0')).toBeNull();
    expect(await cache.getTranscript('desktop:1', 's2')).toBeNull();
    expect(await cache.getTranscript('desktop:1', 's3')).not.toBeNull();
    expect(await cache.getTranscript('desktop:1', `s${MAX_CACHED_TRANSCRIPTS + 2}`)).not.toBeNull();
    const transcripts = await store.keys('transcript:desktop:1:');
    expect(transcripts.length).toBe(MAX_CACHED_TRANSCRIPTS);
  });

  it('evictTarget drops everything of one target and nothing of another', async () => {
    targets.upsert({ id: 'desktop:2', kind: 'desktop', label: 'd', baseUrl: 'http://2', token: 't2' });
    await cache.putSessions('desktop:1', [session('a')]);
    await cache.putTranscript('desktop:1', 'a', [msg(1)]);
    await cache.putSessions('desktop:2', [session('b')]);
    await cache.evictTarget('desktop:1');
    expect(await cache.getSessions('desktop:1')).toBeNull();
    expect(await cache.getTranscript('desktop:1', 'a')).toBeNull();
    expect((await cache.getSessions('desktop:2'))?.sessions[0].id).toBe('b');
  });
});

describe('OfflineQueue (WP-M6 / F10-27)', () => {
  let queue: OfflineQueue;
  let events: EventsStore;
  let promptSpy: jasmine.Spy;
  let resolveSpy: jasmine.Spy;
  let now: number;

  beforeEach(() => {
    localStorage.clear();
    jasmine.clock().install();
    now = 1_000_000;
    promptSpy = jasmine.createSpy('prompt').and.resolveTo({});
    resolveSpy = jasmine.createSpy('resolvePermission').and.resolveTo({});
    const engine = {
      connection: signal(null),
      unauthorized: signal(false),
      prompt: promptSpy,
      resolvePermission: resolveSpy,
      abort: jasmine.createSpy('abort').and.resolveTo({}),
    } as unknown as EngineApi;
    TestBed.configureTestingModule({
      providers: [provideZonelessChangeDetection(), { provide: ENGINE_API, useValue: engine }],
    });
    queue = TestBed.inject(OfflineQueue);
    queue.now = () => now;
    events = TestBed.inject(EventsStore);
    const targets = TestBed.inject(EngineTargetStore);
    targets.upsert({ id: 'desktop:1', kind: 'desktop', label: 'd', baseUrl: 'http://1', token: 't' });
    targets.setActive('desktop:1');
  });

  afterEach(() => {
    jasmine.clock().uninstall();
    localStorage.clear();
  });

  it('a queued command expires after 10 minutes and is marked, not dropped', () => {
    const cmd = queue.enqueue('prompt', 's1', { message: 'hello' });
    expect(cmd.expiresAt - cmd.queuedAt).toBe(QUEUE_TTL_MS);
    expect(queue.pending().length).toBe(1);
    now += QUEUE_TTL_MS - 1;
    jasmine.clock().tick(QUEUE_TTL_MS - 1);
    expect(queue.pending().length).toBe(1);
    now += QUEUE_TICK_MS;
    jasmine.clock().tick(QUEUE_TICK_MS);
    expect(queue.pending().length).toBe(0);
    expect(queue.expired().length).toBe(1);
    expect(queue.commands()[0].state).toBe('expired');
    expect(promptSpy).not.toHaveBeenCalled();
  });

  it('flushes pending commands in order once the stream is live', async () => {
    queue.enqueue('prompt', 's1', { message: 'first' });
    queue.enqueue('permission', 's1', { requestID: 'r1', decision: 'allow', always: false });
    queue.enqueue('prompt', 's1', { message: 'second' });
    events.state.set('live');
    TestBed.tick();
    for (let i = 0; i < 10; i++) {
      await Promise.resolve();
    }
    expect(promptSpy.calls.allArgs()).toEqual([
      ['s1', 'first'],
      ['s1', 'second'],
    ]);
    expect(resolveSpy).toHaveBeenCalledWith('s1', 'r1', { decision: 'allow', always: false });
    expect(queue.commands().map((c) => c.state)).toEqual(['sent', 'sent', 'sent']);
  });

  it('an expired command is never sent on flush', async () => {
    queue.enqueue('prompt', 's1', { message: 'stale' });
    now += QUEUE_TTL_MS + 1;
    await queue.flush();
    expect(promptSpy).not.toHaveBeenCalled();
    expect(queue.commands()[0].state).toBe('expired');
  });

  it('keeps a command pending when the send fails at the network level', async () => {
    promptSpy.and.rejectWith(new TypeError('Failed to fetch'));
    queue.enqueue('prompt', 's1', { message: 'x' });
    await queue.flush();
    expect(queue.commands()[0].state).toBe('pending');
  });

  it('marks a command failed when the engine refuses it (no endless retry)', async () => {
    promptSpy.and.rejectWith(new Error('engine POST /session/s1/prompt -> 409: busy'));
    queue.enqueue('prompt', 's1', { message: 'x' });
    await queue.flush();
    expect(queue.commands()[0].state).toBe('failed');
    expect(queue.commands()[0].error).toContain('409');
  });

  it('isNetworkError tells fetch failures from engine errors', () => {
    expect(isNetworkError(new TypeError('Failed to fetch'))).toBeTrue();
    expect(isNetworkError(new Error('engine GET /x -> 500: boom'))).toBeFalse();
  });
});
