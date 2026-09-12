/**
 * Capacitor bridge to the embedded engine launcher (Android). The native
 * `EngineLauncherPlugin` spawns the bundled bebok-server sidecar and returns
 * its local URL. Only loaded dynamically when running inside Capacitor.
 *
 * Auth (F0-5): the plugin forwards the engine's `BEBOK_READY` line verbatim, so
 * `baseUrl` carries the per-launch capability token as `?token=…`. The caller
 * (`transport.strategy.ts#adopt`) splits it off — never build request URLs from
 * this value directly.
 */

import { registerPlugin } from '@capacitor/core';

export interface EngineLauncherPlugin {
  /**
   * Start the embedded engine.
   *
   * @returns the announced engine URL, including `?token=<capability token>`
   *          unless the engine was started with `BEBOK_NO_AUTH=1`.
   */
  start(): Promise<{ baseUrl: string }>;
  stop(): Promise<void>;
}

export const EngineLauncher = registerPlugin<EngineLauncherPlugin>('EngineLauncher');
