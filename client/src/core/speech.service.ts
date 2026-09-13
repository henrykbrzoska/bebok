/**
 * Voice dictation (WP-M5 / F10-17) on top of
 * `@capacitor-community/speech-recognition`.
 *
 * The Android WebView has no Web Speech API, so the desktop composer's
 * `SpeechRecognition` path never lights up on the phone. This service uses
 * the platform recogniser instead and hands back **text only** - no audio is
 * ever uploaded to the engine or a provider. `probe()` answers whether a
 * recognition service exists at all (devices without Google speech services
 * report `available: false`), and the composer hides its mic entirely in
 * that case rather than showing a control that errors.
 *
 * The plugin is imported dynamically and only on Capacitor.
 */

import { Injectable, signal } from '@angular/core';

import { isCapacitorRuntime } from './secure-store';

/** The slice of the plugin this service uses. */
export interface SpeechBridge {
  available(): Promise<{ available: boolean }>;
  requestPermissions(): Promise<{ speechRecognition: string }>;
  start(options: {
    language: string;
    maxResults: number;
    partialResults: boolean;
    popup: boolean;
  }): Promise<{ matches?: string[] }>;
  stop(): Promise<void>;
}

@Injectable({ providedIn: 'root' })
export class SpeechService {
  /** `null` until `probe()` ran; then whether a recogniser exists. */
  readonly supported = signal<boolean | null>(isCapacitorRuntime() ? null : false);
  readonly listening = signal(false);
  /** Last failure (permission denied, recogniser error) for the UI. */
  readonly error = signal<string | null>(null);

  private loader: () => Promise<SpeechBridge> = defaultLoader;
  private probing: Promise<boolean> | null = null;

  /** Test seam. */
  configure(overrides: { native?: boolean; bridge?: SpeechBridge }): void {
    if (overrides.native !== undefined) {
      this.supported.set(overrides.native ? null : false);
      this.probing = null;
    }
    if (overrides.bridge) {
      const bridge = overrides.bridge;
      this.loader = async () => bridge;
    }
  }

  /** Ask the platform once whether speech recognition is available. */
  probe(): Promise<boolean> {
    const known = this.supported();
    if (known !== null) {
      return Promise.resolve(known);
    }
    this.probing ??= (async () => {
      try {
        const { available } = await (await this.loader()).available();
        this.supported.set(!!available);
        return !!available;
      } catch {
        this.supported.set(false);
        return false;
      }
    })();
    return this.probing;
  }

  /**
   * Listen for one utterance and resolve with its transcript (`null` when
   * nothing was recognised, permission was refused, or dictation was
   * stopped). Never throws into the composer.
   */
  async listen(language: string = navigator.language || 'en-US'): Promise<string | null> {
    if (this.listening() || !(await this.probe())) {
      return null;
    }
    this.error.set(null);
    let bridge: SpeechBridge;
    try {
      bridge = await this.loader();
      const { speechRecognition } = await bridge.requestPermissions();
      if (speechRecognition !== 'granted') {
        this.error.set('permission');
        return null;
      }
    } catch (err) {
      this.error.set(describe(err));
      return null;
    }
    this.listening.set(true);
    try {
      const { matches } = await bridge.start({
        language,
        maxResults: 1,
        partialResults: false,
        popup: false,
      });
      const text = (matches ?? []).map((m) => m.trim()).find((m) => m.length > 0) ?? null;
      return text;
    } catch (err) {
      this.error.set(describe(err));
      return null;
    } finally {
      this.listening.set(false);
    }
  }

  /** Stop an in-progress dictation (the pending `listen()` then resolves). */
  async stop(): Promise<void> {
    if (!this.listening()) {
      return;
    }
    try {
      await (await this.loader()).stop();
    } catch {
      /* already stopped */
    }
  }
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

async function defaultLoader(): Promise<SpeechBridge> {
  const { SpeechRecognition } = await import('@capacitor-community/speech-recognition');
  return SpeechRecognition as unknown as SpeechBridge;
}
