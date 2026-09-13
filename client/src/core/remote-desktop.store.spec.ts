/**
 * WP-M4 / F10-15: `RemoteDesktopStore` fetches status + devices exactly once
 * on `ensure()` and afterwards stays live purely from the three `remote.*`
 * bus events - no second poller behind the panel and the topbar pill.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { ENGINE_API } from './engine-api';
import { EngineEvent, RemoteDevice, RemoteStatus } from './engine.dtos';
import { EventsStore } from './events.store';
import { RemoteDesktopStore } from './remote-desktop.store';

function makeStatus(overrides: Partial<RemoteStatus> = {}): RemoteStatus {
  return {
    enabled: true,
    listening: true,
    endpoints: ['http://100.1.1.1:8790'],
    devicesOnline: 0,
    devices: 0,
    engineName: 'demo-desktop',
    fingerprint: 'abcd1234',
    port: 8790,
    allowLan: false,
    ...overrides,
  };
}

function device(id: string, extra: Partial<RemoteDevice> = {}): RemoteDevice {
  return {
    id,
    name: 'Phone',
    createdAt: 0,
    lastSeen: 0,
    lastIp: '100.1.1.2',
    revoked: false,
    model: '',
    platform: '',
    ...extra,
  };
}

describe('RemoteDesktopStore (F10-15)', () => {
  let listener: ((event: EngineEvent) => void) | null;
  let getRemoteStatus: jasmine.Spy;
  let listDevices: jasmine.Spy;
  let eventsStart: jasmine.Spy;
  let store: RemoteDesktopStore;

  function emit(type: string, properties: unknown): void {
    listener?.({ type, directory: '', sessionID: '', properties } as EngineEvent);
  }

  beforeEach(() => {
    listener = null;
    getRemoteStatus = jasmine.createSpy('getRemoteStatus').and.resolveTo(makeStatus());
    listDevices = jasmine.createSpy('listDevices').and.resolveTo([]);
    eventsStart = jasmine.createSpy('start');

    const engine = { getRemoteStatus, listDevices, connected: () => true, connect: jasmine.createSpy('connect') };
    const events = {
      start: eventsStart,
      onEvent: jasmine.createSpy('onEvent').and.callFake((fn: (event: EngineEvent) => void) => {
        listener = fn;
        return () => {
          listener = null;
        };
      }),
    };

    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        { provide: ENGINE_API, useValue: engine },
        { provide: EventsStore, useValue: events },
      ],
    });
    store = TestBed.inject(RemoteDesktopStore);
  });

  it('fetches status + devices exactly once, even across repeated ensure() calls', async () => {
    await store.ensure();
    await store.ensure();
    await store.ensure();
    expect(getRemoteStatus).toHaveBeenCalledTimes(1);
    expect(listDevices).toHaveBeenCalledTimes(1);
    expect(eventsStart).toHaveBeenCalledTimes(1);
    expect(store.enabled()).toBeTrue();
  });

  it('updates from a single remote.status event without an extra fetch', async () => {
    await store.ensure();
    emit('remote.status', makeStatus({ devicesOnline: 3, devices: 3 }));
    expect(store.devicesOnline()).toBe(3);
    expect(getRemoteStatus).toHaveBeenCalledTimes(1);
  });

  it('removes a device optimistically on remote.device.changed "removed"', async () => {
    listDevices.and.resolveTo([device('d1')]);
    await store.ensure();
    expect(store.devices().length).toBe(1);

    emit('remote.device.changed', { deviceId: 'd1', change: 'removed' });
    expect(store.devices().length).toBe(0);
    // no extra list fetch needed for a removal - it is applied locally
    expect(listDevices).toHaveBeenCalledTimes(1);
  });

  it('reconciles the device list on remote.device.changed "created"', async () => {
    await store.ensure();
    listDevices.and.resolveTo([device('d2', { name: 'New phone' })]);

    emit('remote.device.changed', { deviceId: 'd2', change: 'created' });
    await Promise.resolve();
    await Promise.resolve();

    expect(store.devices().length).toBe(1);
    expect(store.devices()[0].name).toBe('New phone');
    expect(listDevices).toHaveBeenCalledTimes(2);
  });

  it('surfaces a pairing request and clears it on demand', async () => {
    await store.ensure();
    emit('remote.pair.request', {
      pairId: 'p1',
      deviceName: 'Pixel',
      model: 'Pixel 9',
      platform: 'android',
      ip: '100.1.1.2',
      expiresAt: Date.now() + 60_000,
    });
    expect(store.pairRequest()?.pairId).toBe('p1');

    store.clearPairRequest();
    expect(store.pairRequest()).toBeNull();
  });

  it('surfaces a fetch failure on the error signal', async () => {
    getRemoteStatus.and.rejectWith(new Error('engine unreachable'));
    await store.ensure();
    expect(store.error()).toContain('engine unreachable');
  });
});

/**
 * Topbar (F10-14) calls `ensure()` as soon as it mounts, which can race the
 * app's own initial `connect()`. Since `ensure()` only fetches once, a bare
 * "not connected" failure at that moment would never self-heal.
 */
describe('RemoteDesktopStore connect-race (F10-15)', () => {
  it('connects the engine itself before the first fetch when not yet connected', async () => {
    let connected = false;
    const connect = jasmine.createSpy('connect').and.callFake(async () => {
      connected = true;
      return {} as never;
    });
    const engine = {
      connected: () => connected,
      connect,
      getRemoteStatus: jasmine.createSpy('getRemoteStatus').and.resolveTo(makeStatus()),
      listDevices: jasmine.createSpy('listDevices').and.resolveTo([]),
    };
    const events = { start: () => undefined, onEvent: () => () => undefined };

    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        { provide: ENGINE_API, useValue: engine },
        { provide: EventsStore, useValue: events },
      ],
    });
    const store = TestBed.inject(RemoteDesktopStore);

    await store.ensure();

    expect(connect).toHaveBeenCalledTimes(1);
    expect(engine.getRemoteStatus).toHaveBeenCalledTimes(1);
    expect(store.error()).toBeNull();
    expect(store.enabled()).toBeTrue();
  });
});
