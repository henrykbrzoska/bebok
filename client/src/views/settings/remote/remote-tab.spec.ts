/**
 * Settings -> Remote panel specs (WP-M4 / F10-13): pair-request ->
 * confirm/reject branches, the pairing countdown, and the `allow_lan`
 * warning gating.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { ENGINE_API } from '../../../core/engine-api';
import {
  EngineEvent,
  RemoteApiError,
  RemoteDevice,
  RemotePairStart,
  RemoteStatus,
} from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { RemoteDesktopStore } from '../../../core/remote-desktop.store';
import { RemoteTab } from './remote-tab';

function status(overrides: Partial<RemoteStatus> = {}): RemoteStatus {
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
    name: 'Pixel 9',
    createdAt: 0,
    lastSeen: 0,
    lastIp: '100.1.1.2',
    revoked: false,
    model: 'Pixel 9',
    platform: 'android',
    ...extra,
  };
}

function pairStart(overrides: Partial<RemotePairStart> = {}): RemotePairStart {
  return {
    pairId: 'p1',
    code: 'ABCDEFGH',
    expiresAt: Date.now() + 120_000,
    endpoints: ['http://100.1.1.1:8790'],
    engineName: 'demo-desktop',
    fingerprint: 'abcd1234',
    ...overrides,
  };
}

describe('RemoteTab (F10-13)', () => {
  let fixture: ComponentFixture<RemoteTab>;
  let component: RemoteTab;
  let engine: {
    connected: () => boolean;
    connect: jasmine.Spy;
    getRemoteStatus: jasmine.Spy;
    enableRemote: jasmine.Spy;
    disableRemote: jasmine.Spy;
    startPairing: jasmine.Spy;
    confirmPairing: jasmine.Spy;
    rejectPairing: jasmine.Spy;
    listDevices: jasmine.Spy;
    revokeDevice: jasmine.Spy;
    putConfig: jasmine.Spy;
    readLastDirectory: jasmine.Spy;
  };
  let listener: ((event: EngineEvent) => void) | null;

  async function setup(initialStatus: RemoteStatus = status()): Promise<void> {
    TestBed.resetTestingModule();
    listener = null;
    engine = {
      connected: () => true,
      connect: jasmine.createSpy('connect'),
      getRemoteStatus: jasmine.createSpy('getRemoteStatus').and.resolveTo(initialStatus),
      enableRemote: jasmine.createSpy('enableRemote').and.resolveTo(status({ enabled: true })),
      disableRemote: jasmine.createSpy('disableRemote').and.resolveTo(status({ enabled: false, listening: false })),
      startPairing: jasmine.createSpy('startPairing').and.resolveTo(pairStart()),
      confirmPairing: jasmine.createSpy('confirmPairing').and.resolveTo(device('d1')),
      rejectPairing: jasmine.createSpy('rejectPairing').and.resolveTo({ ok: true }),
      listDevices: jasmine.createSpy('listDevices').and.resolveTo([]),
      revokeDevice: jasmine.createSpy('revokeDevice').and.resolveTo({ ok: true }),
      putConfig: jasmine.createSpy('putConfig').and.resolveTo({}),
      readLastDirectory: jasmine.createSpy('readLastDirectory').and.returnValue('/tmp/project'),
    };
    const events = {
      start: jasmine.createSpy('start'),
      onEvent: jasmine.createSpy('onEvent').and.callFake((fn: (event: EngineEvent) => void) => {
        listener = fn;
        return () => {
          listener = null;
        };
      }),
    };
    TestBed.configureTestingModule({
      imports: [RemoteTab],
      providers: [
        provideZonelessChangeDetection(),
        { provide: ENGINE_API, useValue: engine },
        { provide: EventsStore, useValue: events },
      ],
    });
    fixture = TestBed.createComponent(RemoteTab);
    component = fixture.componentInstance;
    fixture.detectChanges();
    await Promise.resolve();
    await Promise.resolve();
    fixture.detectChanges();
  }

  function emit(type: string, properties: unknown): void {
    listener?.({ type, directory: '', sessionID: '', properties } as EngineEvent);
    fixture.detectChanges();
  }

  afterEach(() => {
    fixture?.destroy();
  });

  it('shows "no eligible interface" instead of a blank endpoint list', async () => {
    await setup(status({ endpoints: [], allowLan: false }));
    expect(component.hasEligibleInterface()).toBeFalse();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain(component.t('remote.noEligibleInterface'));
  });

  it('gates the allow_lan warning behind the switch', async () => {
    await setup(status({ allowLan: false }));
    let text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).not.toContain(component.t('remote.allowLanWarning'));

    await component.setAllowLan(true);
    fixture.detectChanges();
    text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(engine.putConfig).toHaveBeenCalledWith(
      '/tmp/project',
      { remote: { allow_lan: true } },
      { scope: 'global' },
    );
    expect(component.allowLanEffective()).toBeTrue();
    expect(text).toContain(component.t('remote.allowLanWarning'));
  });

  it('starts pairing and counts the code down to expiry', async () => {
    jasmine.clock().install();
    jasmine.clock().mockDate(new Date());
    try {
      await setup();
      engine.startPairing.and.resolveTo(pairStart({ expiresAt: Date.now() + 5_000 }));
      await component.startPairing();
      fixture.detectChanges();
      expect(component.secondsLeft()).toBe(5);
      expect(component.pairCodeExpired()).toBeFalse();

      jasmine.clock().tick(3_000);
      expect(component.secondsLeft()).toBe(2);

      jasmine.clock().tick(3_000);
      expect(component.secondsLeft()).toBe(0);
      expect(component.pairCodeExpired()).toBeTrue();
    } finally {
      jasmine.clock().uninstall();
    }
  });

  it('surfaces remote_disabled from startPairing as inline text', async () => {
    await setup();
    engine.startPairing.and.rejectWith(new RemoteApiError('remote_disabled', 'no listener', 409));
    await component.startPairing();
    fixture.detectChanges();
    expect(component.pairError()).toBe(component.t('remote.errorRemoteDisabled'));
  });

  it('shows the pair-request modal and confirms it into a device', async () => {
    await setup();
    emit('remote.pair.request', {
      pairId: 'p1',
      deviceName: 'Pixel 9',
      model: 'Pixel 9',
      platform: 'android',
      ip: '100.1.1.9',
      expiresAt: Date.now() + 60_000,
    });
    let text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('Pixel 9');

    engine.listDevices.and.resolveTo([device('d1')]);
    await component.confirmPairRequest();
    fixture.detectChanges();

    expect(engine.confirmPairing).toHaveBeenCalledWith('p1');
    const store = TestBed.inject(RemoteDesktopStore);
    expect(store.pairRequest()).toBeNull();
    expect(store.devices().length).toBe(1);
  });

  it('rejects a pair request without creating a device', async () => {
    await setup();
    emit('remote.pair.request', {
      pairId: 'p2',
      deviceName: 'Evil phone',
      model: '',
      platform: '',
      ip: '100.1.1.10',
      expiresAt: Date.now() + 60_000,
    });
    await component.rejectPairRequest();
    fixture.detectChanges();

    expect(engine.rejectPairing).toHaveBeenCalledWith('p2');
    expect(engine.confirmPairing).not.toHaveBeenCalled();
    const store = TestBed.inject(RemoteDesktopStore);
    expect(store.pairRequest()).toBeNull();
  });

  it('revokes a device on the second click, optimistically', async () => {
    await setup();
    engine.listDevices.and.resolveTo([device('d1')]);
    const store = TestBed.inject(RemoteDesktopStore);
    await store.refresh();
    fixture.detectChanges();
    expect(store.devices().length).toBe(1);

    component.armOrRevoke(store.devices()[0]);
    fixture.detectChanges();
    expect(component.revokeArmedId()).toBe('d1');

    component.armOrRevoke(store.devices()[0]);
    fixture.detectChanges();
    expect(store.devices().length).toBe(0);
    expect(engine.revokeDevice).toHaveBeenCalledWith('d1');
  });
});
