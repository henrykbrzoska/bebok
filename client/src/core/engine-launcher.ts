/**
 * Capacitor bridge to the embedded engine launcher (Android). The native
 * `EngineLauncherPlugin` spawns the bundled bebok-server sidecar and returns
 * its local URL. Only loaded dynamically when running inside Capacitor.
 */

import { registerPlugin } from '@capacitor/core';

export interface EngineLauncherPlugin {
  start(): Promise<{ baseUrl: string }>;
  stop(): Promise<void>;
}

export const EngineLauncher = registerPlugin<EngineLauncherPlugin>('EngineLauncher');
