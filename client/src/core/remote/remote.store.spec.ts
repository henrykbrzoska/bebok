/**
 * WP-M6 (F10-24/F10-27): remote link state and the "device revoked" path.
 */

import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection, signal } from '@angular/core';

import { ENGINE_API, EngineApi } from '../engine-api';
import { EngineTargetStore } from '../engine-target.store';
import { EventsStore } from '../events.store';
import { EngineConnection } from '../transport.strategy';
import { MemoryCacheStore, OFFLINE_CACHE_STORE, OfflineCache } from './offline-cache';
import { HEARTBEAT_MS, RemoteStore, TAILSCALE_PACKAGE } from './remote.store';

async function settle(): Promise<void> {
  for (let i = 0; i < 8; i++) {
    await Promise.resolve();
  }
}

describe('RemoteStore (WP-M6)', () => {
  let store: RemoteStore;
  let targets: EngineTargetStore;
  let events: EventsStore;
  let cache: OfflineCache;
  let switchSpy: jasmine.Spy;
  let connection: ReturnType<typeof signal<EngineConnection | null>>;

  beforeEach(() => {
    localStorage.clear();
    // The store restarts the SSE stream after a revoke; keep it off the network.
    spyOn(window, 'fetch').and.rejectWith(new TypeError('Failed to fetch'));
    connection = signal<EngineConnection | null>(null);
    switchSpy = jasmine.createSpy('switchTarget').and.callFake(async (id: string) => {
      const t = TestBed.inject(EngineTargetStore);
      const target = t.byId(id)!;
      t.setActive(id);
      connection.set({ kind: 'http', baseUrl: target.baseUrl });
      return target;
    });
    const engine = {
      connection,
      unauthorized: signal(false),
      switchTarget: switchSpy,
    } as unknown as EngineApi;
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        { provide: ENGINE_API, useValue: engine },
        { provide: OFFLINE_CACHE_STORE, useValue: new MemoryCacheStore() },
      ],
    });
    targets = TestBed.inject(EngineTargetStore);
    events = TestBed.inject(EventsStore);
    cache = TestBed.inject(OfflineCache);
    targets.upsert({ id: 'embedded', kind: 'embedded', label: 'This device', baseUrl: 'http://127.0.0.1:1', token: null, ephemeral: true });
    targets.upsert({ id: 'desktop:1', kind: 'desktop', label: 'rafal-pc', baseUrl: 'http://100.64.0.7:8790', token: 'tok' });
    targets.setActive('embedded');
    store = TestBed.inject(RemoteStore);
  });

  afterEach(() => {
    events.stop();
    localStorage.clear();
  });

  it('exposes the paired desktop and a link of none while the phone engine is active', () => {
    expect(store.desktops().map((t) => t.id)).toEqual(['desktop:1']);
    expect(store.desktop()?.id).toBe('desktop:1');
    expect(store.active()).toBeNull();
    expect(store.link()).toBe('none');
  });

  it('connect() switches to the desktop; the link follows the SSE state', async () => {
    await store.connect();
    expect(switchSpy).toHaveBeenCalledWith('desktop:1');
    expect(store.active()?.id).toBe('desktop:1');
    events.state.set('connecting');
    expect(store.link()).toBe('connecting');
    events.state.set('live');
    expect(store.link()).toBe('live');
    events.state.set('error');
    expect(store.link()).toBe('offline');
    expect(store.offline()).toBeTrue();
  });

  it('401 on a desktop target = revoked: target gone, cache dropped, back to the phone engine', async () => {
    await store.connect();
    await cache.putSessions('desktop:1', []);
    const evict = spyOn(cache, 'evictTarget').and.callThrough();
    switchSpy.calls.reset();
    events.state.set('unauthorized');
    TestBed.tick();
    await settle();
    expect(store.revoked()).toBe('rafal-pc');
    expect(store.link()).toBe('revoked');
    expect(targets.byId('desktop:1')).toBeNull();
    expect(localStorage.getItem('bebok.targets.secret.desktop:1')).toBeNull();
    expect(evict).toHaveBeenCalledWith('desktop:1');
    expect(switchSpy).toHaveBeenCalledWith('embedded');
    // The parked stream was restarted against the phone engine.
    expect(events.state()).not.toBe('unauthorized');
    store.acknowledgeRevoked();
    expect(store.revoked()).toBeNull();
    expect(store.link()).toBe('none');
  });

  it('401 on the phone engine is not a revocation', async () => {
    events.state.set('unauthorized');
    TestBed.tick();
    await settle();
    expect(store.revoked()).toBeNull();
    expect(targets.byId('desktop:1')).not.toBeNull();
  });

  it('unpair() forgets the desktop and its cache', async () => {
    await store.connect();
    await store.unpair('desktop:1');
    expect(targets.byId('desktop:1')).toBeNull();
    expect(targets.activeId()).toBe('embedded');
  });

  it('heartbeats while live against a desktop, never against the phone engine', async () => {
    jasmine.clock().install();
    try {
      const beat = jasmine.createSpy('heartbeat').and.resolveTo(new Response('{}', { status: 200 }));
      store.heartbeatFetch = beat as unknown as typeof store.heartbeatFetch;
      events.state.set('live');
      TestBed.tick();
      jasmine.clock().tick(HEARTBEAT_MS + 1);
      expect(beat).not.toHaveBeenCalled();
      await store.connect();
      TestBed.tick();
      jasmine.clock().tick(HEARTBEAT_MS + 1);
      expect(beat).toHaveBeenCalledWith('http://100.64.0.7:8790/remote/heartbeat', jasmine.objectContaining({ method: 'POST' }));
    } finally {
      jasmine.clock().uninstall();
    }
  });

  it('openTailscale() launches the Tailscale package through AppLauncher', async () => {
    const openUrl = jasmine.createSpy('openUrl').and.resolveTo({ completed: true });
    store.appLauncher = async () => ({ openUrl });
    expect(await store.openTailscale()).toBeTrue();
    expect(openUrl).toHaveBeenCalledWith({ url: TAILSCALE_PACKAGE });
  });
});
