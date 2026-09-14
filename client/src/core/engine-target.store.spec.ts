/**
 * WP-M2 / F10-7: engine targets persist (minus the platform-resolved ones),
 * restore with their tokens through `TargetSecrets`, and expose the active
 * id as a signal.
 */

import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';

import {
  ACTIVE_TARGET_KEY,
  EngineTarget,
  EngineTargetStore,
  LocalStorageTargetSecrets,
  TARGETS_KEY,
  TARGET_SECRETS,
  TargetSecrets,
  isPersistedTarget,
} from './engine-target.store';

const desktop: EngineTarget = {
  id: 'desktop:abc',
  kind: 'desktop',
  label: 'Desktop · rafal-pc',
  baseUrl: 'http://100.64.0.7:8790',
  token: 'device-token',
};

function makeStore(secrets?: TargetSecrets): EngineTargetStore {
  TestBed.resetTestingModule();
  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      ...(secrets ? [{ provide: TARGET_SECRETS, useValue: secrets }] : []),
    ],
  });
  return TestBed.inject(EngineTargetStore);
}

describe('EngineTargetStore (F10-7)', () => {
  beforeEach(() => localStorage.clear());
  afterEach(() => localStorage.clear());

  it('classifies which targets persist', () => {
    expect(isPersistedTarget(desktop)).toBeTrue();
    expect(isPersistedTarget({ ...desktop, kind: 'remote-url' })).toBeTrue();
    expect(isPersistedTarget({ ...desktop, kind: 'remote-url', ephemeral: true })).toBeFalse();
    expect(isPersistedTarget({ ...desktop, kind: 'sidecar' })).toBeFalse();
    expect(isPersistedTarget({ ...desktop, kind: 'embedded' })).toBeFalse();
  });

  it('persists desktop targets without their token and restores them with it', async () => {
    const store = makeStore();
    await store.ready;
    store.upsert(desktop);
    store.upsert({
      id: 'embedded',
      kind: 'embedded',
      label: 'This device',
      baseUrl: 'http://127.0.0.1:41234',
      token: 'launch-token',
    });
    store.setActive(desktop.id);

    const stored = JSON.parse(localStorage.getItem(TARGETS_KEY)!) as Array<Record<string, unknown>>;
    expect(stored.length).toBe(1);
    expect(stored[0]['id']).toBe(desktop.id);
    expect('token' in stored[0]).toBeFalse();
    expect(localStorage.getItem(TARGETS_KEY)).not.toContain('device-token');
    expect(localStorage.getItem(TARGETS_KEY)).not.toContain('launch-token');
    expect(localStorage.getItem(ACTIVE_TARGET_KEY)).toBe(desktop.id);

    // Fresh injector = app relaunch: the desktop target is back, token included
    // (through the secrets store), the embedded one is not (re-resolved).
    const again = makeStore();
    await again.ready;
    expect(again.targets().map((t) => t.id)).toEqual([desktop.id]);
    expect(again.byId(desktop.id)?.token).toBe('device-token');
    expect(again.activeId()).toBe(desktop.id);
    expect(again.active()?.baseUrl).toBe(desktop.baseUrl);
  });

  it('switches the active target and ignores unknown ids', () => {
    const store = makeStore();
    store.upsert(desktop);
    store.upsert({ ...desktop, id: 'desktop:xyz', label: 'Other' });
    expect(store.setActive('nope')).toBeFalse();
    expect(store.activeId()).toBeNull();
    expect(store.setActive('desktop:xyz')).toBeTrue();
    expect(store.active()?.label).toBe('Other');
    expect(localStorage.getItem(ACTIVE_TARGET_KEY)).toBe('desktop:xyz');
  });

  it('upsert merges by id and markOk stamps lastOk', () => {
    const store = makeStore();
    store.upsert(desktop);
    store.upsert({ ...desktop, label: 'Renamed' });
    expect(store.targets().length).toBe(1);
    expect(store.byId(desktop.id)?.label).toBe('Renamed');
    store.markOk(desktop.id, 1234);
    expect(store.byId(desktop.id)?.lastOk).toBe(1234);
    const stored = JSON.parse(localStorage.getItem(TARGETS_KEY)!) as Array<Record<string, unknown>>;
    expect(stored[0]['lastOk']).toBe(1234);
  });

  it('unions known endpoints on upsert and persists them (1.8 roaming)', () => {
    const store = makeStore();
    store.upsert({
      ...desktop,
      endpoints: ['http://100.64.0.7:8790', 'https://r.workers.dev/t/aa'],
    });
    store.upsert({
      ...desktop,
      baseUrl: 'https://r.workers.dev/t/aa',
      endpoints: ['https://r.workers.dev/t/aa'],
    });
    expect(store.byId(desktop.id)?.baseUrl).toBe('https://r.workers.dev/t/aa');
    expect(store.byId(desktop.id)?.endpoints).toEqual([
      'https://r.workers.dev/t/aa',
      'http://100.64.0.7:8790',
    ]);
    const stored = JSON.parse(localStorage.getItem(TARGETS_KEY)!) as Array<Record<string, unknown>>;
    expect(stored[0]['endpoints']).toEqual([
      'https://r.workers.dev/t/aa',
      'http://100.64.0.7:8790',
    ]);
  });

  it('remove drops the target, its secret and the active pointer', async () => {
    const store = makeStore();
    store.upsert(desktop);
    store.setActive(desktop.id);
    store.remove(desktop.id);
    await Promise.resolve();
    expect(store.targets()).toEqual([]);
    expect(store.activeId()).toBeNull();
    expect(localStorage.getItem(ACTIVE_TARGET_KEY)).toBeNull();
    expect(await new LocalStorageTargetSecrets().read(desktop.id)).toBeNull();
  });

  it('reads and writes tokens through the TargetSecrets interface', async () => {
    const vault = new Map<string, string>();
    const secrets: TargetSecrets = {
      read: async (id) => vault.get(id) ?? null,
      write: async (id, token) => {
        if (token) {
          vault.set(id, token);
        } else {
          vault.delete(id);
        }
      },
      remove: async (id) => {
        vault.delete(id);
      },
    };
    const store = makeStore(secrets);
    await store.ready;
    store.upsert(desktop);
    await Promise.resolve();
    expect(vault.get(desktop.id)).toBe('device-token');
    expect(localStorage.getItem('bebok.targets.secret.' + desktop.id)).toBeNull();

    const again = makeStore(secrets);
    await again.ready;
    expect(again.byId(desktop.id)?.token).toBe('device-token');
    expect(await again.tokenFor(desktop.id)).toBe('device-token');
  });

  it('survives corrupt persisted data', async () => {
    localStorage.setItem(TARGETS_KEY, '{not json');
    const store = makeStore();
    await store.ready;
    expect(store.targets()).toEqual([]);
    localStorage.setItem(TARGETS_KEY, JSON.stringify([{ id: 1 }, { ...desktop, kind: 'sidecar' }]));
    const again = makeStore();
    await again.ready;
    expect(again.targets()).toEqual([]);
  });
});
