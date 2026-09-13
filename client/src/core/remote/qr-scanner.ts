/**
 * QR scanner (WP-M6 / F10-22) over `@capacitor-mlkit/barcode-scanning`.
 *
 * Uses the plugin's ready-made `scan()` UI (Google's code-scanner module:
 * no camera permission prompt, no WebView overlay to style), guarded by the
 * module-availability check the plugin documents. Only loaded dynamically
 * and only on a native platform - the desktop bundle never references it,
 * and the browser web-shell falls back to manual entry / deep link.
 */

import { Injectable } from '@angular/core';

export type ScanOutcome =
  | { kind: 'scanned'; value: string }
  | { kind: 'cancelled' }
  | { kind: 'unsupported'; reason: string };

/** The subset of the plugin used here (injectable for specs). */
export interface BarcodeScannerLike {
  isSupported(): Promise<{ supported: boolean }>;
  isGoogleBarcodeScannerModuleAvailable(): Promise<{ available: boolean }>;
  installGoogleBarcodeScannerModule(): Promise<void>;
  scan(options?: { formats?: string[] }): Promise<{ barcodes: { rawValue: string; displayValue: string }[] }>;
}

@Injectable({ providedIn: 'root' })
export class QrScanner {
  /** Load the plugin (overridable in specs). */
  loadPlugin: () => Promise<BarcodeScannerLike> = async () => {
    const mod = await import('@capacitor-mlkit/barcode-scanning');
    return mod.BarcodeScanner as unknown as BarcodeScannerLike;
  };

  /** True inside the Capacitor shell (the only place the plugin works). */
  isNative: () => boolean = () => {
    try {
      const w = window as unknown as { Capacitor?: { isNativePlatform?: () => boolean } };
      return w.Capacitor?.isNativePlatform?.() === true;
    } catch {
      return false;
    }
  };

  async scan(): Promise<ScanOutcome> {
    if (!this.isNative()) {
      return { kind: 'unsupported', reason: 'not_native' };
    }
    let plugin: BarcodeScannerLike;
    try {
      plugin = await this.loadPlugin();
    } catch (err) {
      return { kind: 'unsupported', reason: describe(err) };
    }
    try {
      const { supported } = await plugin.isSupported();
      if (!supported) {
        return { kind: 'unsupported', reason: 'no_camera' };
      }
      const { available } = await plugin.isGoogleBarcodeScannerModuleAvailable();
      if (!available) {
        await plugin.installGoogleBarcodeScannerModule();
      }
      const { barcodes } = await plugin.scan({ formats: ['QR_CODE'] });
      const value = barcodes.map((b) => b.rawValue || b.displayValue).find((v) => !!v);
      return value ? { kind: 'scanned', value } : { kind: 'cancelled' };
    } catch (err) {
      const message = describe(err);
      if (/cancel/i.test(message)) {
        return { kind: 'cancelled' };
      }
      return { kind: 'unsupported', reason: message };
    }
  }
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
