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
  android: {
    // WP-M6 (F10-28): the app is served from `https://localhost`, so a fetch
    // to the paired desktop's `http://100.x.y.z:8790` is *mixed content* for
    // the WebView and would be blocked regardless of the network-security
    // config (loopback is exempt, which is why the embedded engine never
    // needed this). `MIXED_CONTENT_ALWAYS_ALLOW` is scoped by the same
    // client-side address checks as cleartext (see
    // `res/xml/network_security_config.xml` for the decision and the 1.7
    // TLS upgrade path that removes the need for it).
    allowMixedContent: true,
  },
};

export default config;
