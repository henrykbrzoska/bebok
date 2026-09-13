/**
 * Capacitor bridge to the embedded engine launcher (Android). The native
 * `EngineLauncherPlugin` spawns the bundled bebok-server sidecar and returns
 * its local URL. Only loaded dynamically when running inside Capacitor.
 *
 * Auth (F0-5): the plugin forwards the engine's `BEBOK_READY` line verbatim, so
 * `baseUrl` carries the per-launch capability token as `?token=…`. The caller
 * (`transport.strategy.ts#adopt`) splits it off — never build request URLs from
 * this value directly.
 *
 * WP-M5 (F10-21): `beginWork()`/`endWork()` mirror the plugin's reference-
 * counted foreground service (WP-M3 / F10-10). `EngineWorkTracker` below is
 * what the composer calls: it keeps the counts balanced per session (one
 * `beginWork` per running turn, one `endWork` when the engine reports the
 * session idle again - `turn.end`, abort, or a failed prompt) and is a no-op
 * outside Capacitor.
 *
 * Debug mock provider: `start()` forwards `env.BEBOK_PROVIDER_MOCK=1` (WP-M1's
 * deterministic provider) when the on-device developer toggle
 * (`bebok.mobile.mockProvider` in localStorage, Settings-lite → This device)
 * is on. The native side only honours it in debuggable builds.
 */

import { Injectable, inject, signal } from '@angular/core';

import { EventsStore } from './events.store';
import { isCapacitorRuntime } from './secure-store';

/** Options accepted by the native `start()` (all optional, all debug-only). */
export interface EngineLaunchOptions {
  /**
   * Extra environment for the engine process. The native plugin applies an
   * allow-list (`BEBOK_PROVIDER_MOCK` only) and ignores the whole map in
   * non-debuggable builds, so a release APK can never run the mock provider.
   */
  env?: Record<string, string>;
}

export interface EngineLauncherPlugin {
  /**
   * Start the embedded engine.
   *
   * @returns the announced engine URL, including `?token=<capability token>`
   *          unless the engine was started with `BEBOK_NO_AUTH=1`.
   */
  start(options?: EngineLaunchOptions): Promise<{ baseUrl: string }>;
  stop(): Promise<void>;
  /** Raise the foreground service (reference counted, F10-10). */
  beginWork(options?: { reason?: string }): Promise<void>;
  /** Matching end for `beginWork`; the service stops at count zero. */
  endWork(): Promise<void>;
}

/** localStorage key of the developer toggle "use the mock provider". */
export const MOCK_PROVIDER_KEY = 'bebok.mobile.mockProvider';

/** True when the on-device developer toggle for the mock provider is on. */
export function readMockProviderFlag(): boolean {
  try {
    return localStorage.getItem(MOCK_PROVIDER_KEY) === '1';
  } catch {
    return false;
  }
}

export function writeMockProviderFlag(on: boolean): void {
  try {
    if (on) {
      localStorage.setItem(MOCK_PROVIDER_KEY, '1');
    } else {
      localStorage.removeItem(MOCK_PROVIDER_KEY);
    }
  } catch {
    /* ignore */
  }
}

/** The launch options for the current developer settings. */
export function embeddedLaunchOptions(): EngineLaunchOptions {
  return readMockProviderFlag() ? { env: { BEBOK_PROVIDER_MOCK: '1' } } : {};
}

let bridgePromise: Promise<EngineLauncherPlugin> | null = null;

/**
 * The native plugin proxy, registered on first use. `@capacitor/core` is
 * imported dynamically so this module (now also reached statically through
 * `EngineWorkTracker`) adds nothing to the browser/desktop bundle.
 */
function bridge(): Promise<EngineLauncherPlugin> {
  bridgePromise ??= import('@capacitor/core').then(({ registerPlugin }) =>
    registerPlugin<EngineLauncherPlugin>('EngineLauncher'),
  );
  return bridgePromise;
}

/**
 * The launcher as the rest of the client sees it. `start()` threads the
 * developer options through so `TransportStrategy` (which calls it without
 * arguments) needs no change.
 */
export const EngineLauncher: EngineLauncherPlugin = {
  start: async (options) => (await bridge()).start(options ?? embeddedLaunchOptions()),
  stop: async () => (await bridge()).stop(),
  beginWork: async (options) => (await bridge()).beginWork(options),
  endWork: async () => (await bridge()).endWork(),
};

/** The subset of the bridge the tracker needs (swapped in tests). */
export type EngineWorkBridge = Pick<EngineLauncherPlugin, 'beginWork' | 'endWork'>;

/**
 * F10-21: per-session "a turn is running" bookkeeping in front of the native
 * reference counter.
 *
 * - `begin(sessionID)` is called right before `POST /session/{id}/prompt`;
 *   a second call for a session already held is ignored, so a queue drain
 *   or a retry never double-counts.
 * - `end(sessionID)` releases it: the composer calls it on abort and on a
 *   failed prompt, and the tracker itself listens to the SSE stream for
 *   `session.updated { running: false }` (turn end, abort and stream errors
 *   all surface that way), so a turn finishing while the chat view is closed
 *   still drops the service.
 *
 * Calls are serialised per tracker so `beginWork`/`endWork` reach the plugin
 * in order even when the bridge resolves asynchronously.
 */
@Injectable({ providedIn: 'root' })
export class EngineWorkTracker {
  private readonly events = inject(EventsStore);
  private native = isCapacitorRuntime();
  private bridge: EngineWorkBridge = EngineLauncher;
  private readonly held = new Set<string>();
  private queue: Promise<void> = Promise.resolve();
  private listening = false;

  /** Sessions currently counted as working (for the UI / tests). */
  readonly active = signal<readonly string[]>([]);

  /** Test seam: force the native branch and/or replace the bridge. */
  configure(overrides: { native?: boolean; bridge?: EngineWorkBridge }): void {
    if (overrides.native !== undefined) {
      this.native = overrides.native;
    }
    if (overrides.bridge) {
      this.bridge = overrides.bridge;
    }
  }

  begin(sessionID: string, reason = 'turn'): void {
    if (!this.native || !sessionID || this.held.has(sessionID)) {
      return;
    }
    this.ensureListening();
    this.held.add(sessionID);
    this.active.set([...this.held]);
    this.enqueue(() => this.bridge.beginWork({ reason }));
  }

  end(sessionID: string): void {
    if (!this.held.delete(sessionID)) {
      return;
    }
    this.active.set([...this.held]);
    this.enqueue(() => this.bridge.endWork());
  }

  /** Release everything (engine stopped / target switched). */
  endAll(): void {
    for (const id of [...this.held]) {
      this.end(id);
    }
  }

  /** Resolves once every queued bridge call settled (tests). */
  settled(): Promise<void> {
    return this.queue;
  }

  private ensureListening(): void {
    if (this.listening) {
      return;
    }
    this.listening = true;
    this.events.onEvent((event) => {
      if (event.type === 'session.updated' && event.properties?.['running'] === false) {
        this.end(event.sessionID);
      }
    });
  }

  private enqueue(call: () => Promise<void>): void {
    this.queue = this.queue.then(call).catch((err: unknown) => {
      console.warn('engine work bridge call failed', err);
    });
  }
}
