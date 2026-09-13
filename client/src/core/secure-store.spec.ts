/**
 * WP-M5 / F10-20: `SecureStoreTargetSecrets` round-trips through the native
 * plugin, prefixes keys, maps null tokens to removals, and degrades to an
 * in-memory map when the plugin is missing. The browser default stays
 * `localStorage`.
 */

import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';

import { LocalStorageTargetSecrets, TARGET_SECRETS } from './engine-target.store';
import {
  SECURE_TARGET_PREFIX,
  SecureStorePlugin,
  SecureStoreTargetSecrets,
  isCapacitorRuntime,
} from './secure-store';

function fakePlugin(): SecureStorePlugin & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return {
    data,
    get: async ({ key }) => ({ value: data.get(key) ?? null }),
    set: async ({ key, value }) => {
      data.set(key, value);
    },
    remove: async ({ key }) => {
      data.delete(key);
    },
  };
}

describe('SecureStoreTargetSecrets (F10-20)', () => {
  it('writes, reads and removes through the plugin with a prefixed key', async () => {
    const plugin = fakePlugin();
    const secrets = new SecureStoreTargetSecrets(async () => plugin);

    await secrets.write('desktop:abc', 'device-token');
    expect(plugin.data.get(SECURE_TARGET_PREFIX + 'desktop:abc')).toBe('device-token');
    expect(await secrets.read('desktop:abc')).toBe('device-token');

    await secrets.remove('desktop:abc');
    expect(plugin.data.size).toBe(0);
    expect(await secrets.read('desktop:abc')).toBeNull();
  });

  it('treats a null token as a removal', async () => {
    const plugin = fakePlugin();
    const secrets = new SecureStoreTargetSecrets(async () => plugin);
    await secrets.write('desktop:abc', 'device-token');
    await secrets.write('desktop:abc', null);
    expect(plugin.data.size).toBe(0);
  });

  it('never touches localStorage', async () => {
    localStorage.clear();
    const secrets = new SecureStoreTargetSecrets(async () => fakePlugin());
    await secrets.write('desktop:abc', 'device-token');
    expect(localStorage.length).toBe(0);
    localStorage.clear();
  });

  it('degrades to memory when the plugin is unavailable', async () => {
    const warn = spyOn(console, 'warn');
    const secrets = new SecureStoreTargetSecrets(async () => {
      throw new Error('"SecureStore" plugin is not implemented on android');
    });
    await secrets.write('desktop:abc', 'device-token');
    expect(await secrets.read('desktop:abc')).toBe('device-token');
    await secrets.remove('desktop:abc');
    expect(await secrets.read('desktop:abc')).toBeNull();
    expect(warn).toHaveBeenCalledTimes(1);
  });

  it('is not the default outside Capacitor (browser keeps localStorage)', () => {
    expect(isCapacitorRuntime()).toBeFalse();
    TestBed.configureTestingModule({ providers: [provideZonelessChangeDetection()] });
    expect(TestBed.inject(TARGET_SECRETS)).toBeInstanceOf(LocalStorageTargetSecrets);
  });
});
