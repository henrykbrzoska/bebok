/**
 * QR scanner (WP-M6 / F10-22) over `@capacitor-mlkit/barcode-scanning`.
 *
 * Scans in-app: the plugin runs ML Kit in-process on a CameraX preview
 * drawn *behind* the WebView, so the page must go transparent while a scan
 * is active (`body.barcode-scanner-active` in styles.css; the pair view
 * keeps only its overlay visible). This replaced Google's code-scanner
 * activity (`scan()`), which lives in Play Services and fails on some
 * devices with a bare "Failed to scan code." after a few seconds.
 *
 * Only loaded dynamically and only on a native platform - the desktop
 * bundle never references it, and the browser web-shell falls back to
 * manual entry / deep link.
 */

import { Injectable, signal } from '@angular/core';

export type ScanOutcome =
  | { kind: 'scanned'; value: string }
  | { kind: 'cancelled' }
  | { kind: 'unsupported'; reason: string };

export const SCANNER_ACTIVE_CLASS = 'barcode-scanner-active';

interface ListenerHandle {
  remove(): Promise<void>;
}

/** The subset of the plugin used here (injectable for specs). */
export interface BarcodeScannerLike {
  isSupported(): Promise<{ supported: boolean }>;
  requestPermissions(): Promise<{ camera: string }>;
  startScan(options?: { formats?: string[] }): Promise<void>;
  stopScan(): Promise<void>;
  onBarcodes(handler: (values: string[]) => void): Promise<ListenerHandle>;
  onError(handler: (message: string) => void): Promise<ListenerHandle>;
}

@Injectable({ providedIn: 'root' })
export class QrScanner {
  /** True while the camera preview is up (the pair view shows its overlay). */
  readonly active = signal(false);
  private cancelCurrent: (() => void) | null = null;

  /** Load the plugin (overridable in specs). */
  loadPlugin: () => Promise<BarcodeScannerLike> = async () => {
    const mod = await import('@capacitor-mlkit/barcode-scanning');
    // Never return the plugin Proxy itself from an async function: `await`
    // probes `.then` and the Proxy throws "not implemented" for it.
    const scanner = mod.BarcodeScanner;
    return {
      isSupported: () => scanner.isSupported(),
      requestPermissions: () => scanner.requestPermissions(),
      startScan: (options) => scanner.startScan(options as Parameters<typeof scanner.startScan>[0]),
      stopScan: () => scanner.stopScan(),
      onBarcodes: (handler) =>
        scanner.addListener('barcodesScanned', (event) =>
          handler(event.barcodes.map((b) => b.rawValue || b.displayValue).filter((v) => !!v)),
        ),
      onError: (handler) => scanner.addListener('scanError', (event) => handler(event.message)),
    };
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

  /** Close the preview without a result (the overlay's Cancel button, back). */
  cancel(): void {
    this.cancelCurrent?.();
  }

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
      const { camera } = await plugin.requestPermissions();
      if (camera !== 'granted' && camera !== 'limited') {
        return { kind: 'unsupported', reason: 'camera_denied' };
      }
    } catch (err) {
      return { kind: 'unsupported', reason: describe(err) };
    }

    const handles: ListenerHandle[] = [];
    const cleanup = async () => {
      this.cancelCurrent = null;
      this.active.set(false);
      document.body.classList.remove(SCANNER_ACTIVE_CLASS);
      for (const h of handles.splice(0)) {
        await h.remove().catch(() => undefined);
      }
      await plugin.stopScan().catch(() => undefined);
    };

    const outcome = await new Promise<ScanOutcome>((resolve) => {
      let done = false;
      const finish = (o: ScanOutcome) => {
        if (!done) {
          done = true;
          resolve(o);
        }
      };
      this.cancelCurrent = () => finish({ kind: 'cancelled' });
      void (async () => {
        try {
          handles.push(
            await plugin.onBarcodes((values) => {
              const value = values.find((v) => v.startsWith('bebok:')) ?? values[0];
              if (value) {
                finish({ kind: 'scanned', value });
              }
            }),
          );
          handles.push(
            await plugin.onError((message) => finish({ kind: 'unsupported', reason: message })),
          );
          document.body.classList.add(SCANNER_ACTIVE_CLASS);
          this.active.set(true);
          await plugin.startScan({ formats: ['QR_CODE'] });
        } catch (err) {
          finish({ kind: 'unsupported', reason: describe(err) });
        }
      })();
    });
    await cleanup();
    return outcome;
  }
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
