import type { CapacitorConfig } from '@capacitor/cli';

/**
 * Capacitor (mobile) shell config (M6): the same Angular bundle as the desktop
 * Tauri shell, served from `dist/bebok/browser`. The mobile client is thin -
 * it connects to a remote engine over LAN (see `src/views/connect/`).
 */
const config: CapacitorConfig = {
  appId: 'dev.bebok.mobile',
  appName: 'Bebok',
  webDir: 'dist/bebok/browser',
  server: {
    // The webview loads the bundled app; the remote engine is chosen at runtime
    // on the connect screen (no fixed URL here).
    androidScheme: 'https',
    cleartext: true, // allow http:// to a LAN engine
  },
};

export default config;
