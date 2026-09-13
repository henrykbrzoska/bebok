/**
 * SecureStore (WP-M5 / F10-20): Android Keystore-backed `TargetSecrets`.
 *
 * The native `SecureStorePlugin` (Capacitor plugin `SecureStore`, package
 * `dev.bebok.mobile`) keeps one AES-256/GCM key inside `AndroidKeyStore` and
 * stores each value as `base64(iv || ciphertext)` in a private
 * `SharedPreferences` file - so `adb shell run-as dev.bebok.mobile cat
 * shared_prefs/*.xml` only ever shows ciphertext, and the key itself never
 * leaves the hardware-backed store.
 *
 * `SecureStoreTargetSecrets` implements `TargetSecrets` on top of it and is
 * what the `TARGET_SECRETS` token resolves to **on Capacitor only**
 * (`engine-target.store.ts` keeps `localStorage` for the browser and the
 * desktop shell). `@capacitor/core` is imported lazily, so nothing here
 * reaches the desktop bundle.
 *
 * If the native plugin is missing (an APK built without it) every call
 * degrades to an in-memory map with a console warning rather than throwing
 * into the target store; tokens are then lost on restart, never leaked.
 */

import type { TargetSecrets } from './engine-target.store';

export interface SecureStorePlugin {
  get(options: { key: string }): Promise<{ value: string | null }>;
  set(options: { key: string; value: string }): Promise<void>;
  remove(options: { key: string }): Promise<void>;
}

/** Prefix for the target-token entries inside the store. */
export const SECURE_TARGET_PREFIX = 'target.';

let bridgePromise: Promise<SecureStorePlugin> | null = null;

/** The native plugin proxy, registered on first use (lazy `@capacitor/core`). */
export function secureStore(): Promise<SecureStorePlugin> {
  bridgePromise ??= import('@capacitor/core').then(({ registerPlugin }) => {
    const plugin = registerPlugin<SecureStorePlugin>('SecureStore');
    // Same pitfall as `engine-launcher.ts`: the Capacitor proxy is a
    // thenable (it answers `then` with a native method wrapper), so a promise
    // resolved with it never settles. Return plain bindings instead.
    return {
      get: (options) => plugin.get(options),
      set: (options) => plugin.set(options),
      remove: (options) => plugin.remove(options),
    };
  });
  return bridgePromise;
}

/** Test seam: replace the native proxy. */
export function setSecureStoreBridge(plugin: SecureStorePlugin | null): void {
  bridgePromise = plugin ? Promise.resolve(plugin) : null;
}

export class SecureStoreTargetSecrets implements TargetSecrets {
  /** Fallback when the plugin is unavailable: memory only, never persisted. */
  private readonly memory = new Map<string, string>();
  private unavailable = false;

  constructor(private readonly plugin: () => Promise<SecureStorePlugin> = secureStore) {}

  async read(id: string): Promise<string | null> {
    const key = SECURE_TARGET_PREFIX + id;
    if (this.unavailable) {
      return this.memory.get(key) ?? null;
    }
    try {
      const { value } = await (await this.plugin()).get({ key });
      return value ?? null;
    } catch (err) {
      this.degrade(err);
      return this.memory.get(key) ?? null;
    }
  }

  async write(id: string, token: string | null): Promise<void> {
    if (!token) {
      return this.remove(id);
    }
    const key = SECURE_TARGET_PREFIX + id;
    if (this.unavailable) {
      this.memory.set(key, token);
      return;
    }
    try {
      await (await this.plugin()).set({ key, value: token });
    } catch (err) {
      this.degrade(err);
      this.memory.set(key, token);
    }
  }

  async remove(id: string): Promise<void> {
    const key = SECURE_TARGET_PREFIX + id;
    this.memory.delete(key);
    if (this.unavailable) {
      return;
    }
    try {
      await (await this.plugin()).remove({ key });
    } catch (err) {
      this.degrade(err);
    }
  }

  private degrade(err: unknown): void {
    if (!this.unavailable) {
      this.unavailable = true;
      console.warn('SecureStore plugin unavailable - device tokens are kept in memory only', err);
    }
  }
}

/** Capacitor injects `window.Capacitor` and a `Capacitor` UA token. */
export function isCapacitorRuntime(): boolean {
  if (typeof window === 'undefined') {
    return false;
  }
  const ua = navigator.userAgent || '';
  return ua.includes('Capacitor') || 'Capacitor' in window;
}
