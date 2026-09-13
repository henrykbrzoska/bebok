/**
 * WP-M2 / F10-7: `EngineClient.switchTarget()` re-points every REST call at
 * the new base URL + token within one tick, aborts the requests still in
 * flight against the old target, and registers the platform default as a
 * target on `connect()`.
 */

import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';

import { getEngineToken, setEngineToken } from './auth.interceptor';
import { ENGINE_API } from './engine-api';
import { EngineClient, PLATFORM_TARGET_ID } from './engine-client.service';
import { EngineTargetStore } from './engine-target.store';

interface Captured {
  url: string;
  auth: string | null;
  signal: AbortSignal;
  resolve: (body: unknown) => void;
}

describe('EngineClient.switchTarget (F10-7)', () => {
  let calls: Captured[];
  let client: EngineClient;
  let targets: EngineTargetStore;

  beforeEach(() => {
    localStorage.clear();
    localStorage.setItem('bebok.remote.baseUrl', 'http://engine-a:1');
    localStorage.setItem('bebok.remote.token', 'tok-a');
    setEngineToken(null);
    calls = [];
    spyOn(window, 'fetch').and.callFake((input: RequestInfo | URL, init: RequestInit = {}) => {
      const headers = new Headers(init.headers);
      return new Promise<Response>((resolve, reject) => {
        const signal = init.signal as AbortSignal;
        signal?.addEventListener('abort', () =>
          reject(new DOMException('aborted', 'AbortError')),
        );
        calls.push({
          url: String(input),
          auth: headers.get('Authorization'),
          signal,
          resolve: (body) =>
            resolve(
              new Response(JSON.stringify(body), {
                status: 200,
                headers: { 'Content-Type': 'application/json' },
              }),
            ),
        });
      });
    });
    TestBed.configureTestingModule({ providers: [provideZonelessChangeDetection()] });
    client = TestBed.inject(EngineClient);
    targets = TestBed.inject(EngineTargetStore);
  });

  afterEach(() => {
    localStorage.clear();
    setEngineToken(null);
  });

  it('is what ENGINE_API resolves to by default', () => {
    expect(TestBed.inject(ENGINE_API)).toBe(client);
  });

  it('connect() registers the platform default as the active target', async () => {
    const conn = await client.connect();
    expect(conn).toEqual({ kind: 'http', baseUrl: 'http://engine-a:1' });
    expect(targets.activeId()).toBe(PLATFORM_TARGET_ID['remote-url']);
    const target = targets.active()!;
    expect(target.kind).toBe('remote-url');
    expect(target.token).toBe('tok-a');
    expect(target.ephemeral).toBeTrue();
    // Ephemeral: never written to `bebok.targets`.
    expect(localStorage.getItem('bebok.targets')).toBeNull();
  });

  it('re-points REST calls and the token, aborting in-flight requests', async () => {
    await client.connect();
    targets.upsert({
      id: 'desktop:1',
      kind: 'desktop',
      label: 'Desktop',
      baseUrl: 'http://engine-b:2',
      token: 'tok-b',
    });

    const slow = client.listProjects();
    expect(calls.length).toBe(1);
    expect(calls[0].url).toBe('http://engine-a:1/projects');
    expect(calls[0].auth).toBe('Bearer tok-a');

    client.unauthorized.set(true);
    await client.switchTarget('desktop:1');

    expect(calls[0].signal.aborted).toBeTrue();
    await expectAsync(slow).toBeRejected();
    expect(client.connection()).toEqual({ kind: 'http', baseUrl: 'http://engine-b:2' });
    expect(client.unauthorized()).toBeFalse();
    expect(getEngineToken()).toBe('tok-b');
    expect(targets.activeId()).toBe('desktop:1');
    expect(client.eventUrl()).toBe('http://engine-b:2/event');

    const next = client.listProjects();
    expect(calls.length).toBe(2);
    expect(calls[1].url).toBe('http://engine-b:2/projects');
    expect(calls[1].auth).toBe('Bearer tok-b');
    expect(calls[1].signal.aborted).toBeFalse();
    calls[1].resolve({ projects: [] });
    expect(await next).toEqual([]);
  });

  it('switching back to the platform target restores its token', async () => {
    await client.connect();
    targets.upsert({ id: 'd', kind: 'desktop', label: 'D', baseUrl: 'http://b:2', token: 'tok-b' });
    await client.switchTarget('d');
    await client.switchTarget(PLATFORM_TARGET_ID['remote-url']);
    expect(getEngineToken()).toBe('tok-a');
    expect(client.connection()?.baseUrl).toBe('http://engine-a:1');
  });

  it('rejects an unknown target id', async () => {
    await client.connect();
    await expectAsync(client.switchTarget('ghost')).toBeRejectedWithError(/unknown engine target/);
  });
});
