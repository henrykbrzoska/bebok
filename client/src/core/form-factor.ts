/**
 * Form factor (WP-M2 / F10-8): which shell the app renders.
 *
 * `isMobile` is true when
 * - the runtime is the Capacitor (Android) shell, or
 * - the viewport is phone-sized (`(max-width: 767px)`), or
 * - the page was opened with `?ff=mobile` (browser screenshots, Playwright);
 *   the override is remembered for the tab in `sessionStorage` so in-app
 *   navigation (which rewrites the URL) keeps the shell. `?ff=desktop`
 *   clears it.
 *
 * The media query is observed, so shrinking a browser window below 768px
 * swaps `AppShell` for `MobileShell` (and the route guards move the URL to
 * its `/m/**` twin). The Tauri desktop shell never switches: its window can
 * be narrow without being a phone, and WP-M2 must leave desktop behaviour
 * untouched.
 *
 * Every browser API here is wrapped: `matchMedia`/`sessionStorage` throw in
 * some WebView embeds and in Karma.
 */

import { Injectable, Signal, inject, signal } from '@angular/core';

import { EngineClient } from './engine-client.service';

export const FORM_FACTOR_PARAM = 'ff';
export const FORM_FACTOR_SESSION_KEY = 'bebok.ff';
export const MOBILE_MEDIA_QUERY = '(max-width: 767px)';

export type FormFactorOverride = 'mobile' | 'desktop' | null;

/** Read `?ff=` from a query string (null when absent or unknown). */
export function readFormFactorOverride(search: string): FormFactorOverride {
  let value: string | null;
  try {
    value = new URLSearchParams(search ?? '').get(FORM_FACTOR_PARAM);
  } catch {
    return null;
  }
  return value === 'mobile' || value === 'desktop' ? value : null;
}

function readSessionOverride(): FormFactorOverride {
  try {
    const value = sessionStorage.getItem(FORM_FACTOR_SESSION_KEY);
    return value === 'mobile' || value === 'desktop' ? value : null;
  } catch {
    return null;
  }
}

function writeSessionOverride(value: FormFactorOverride): void {
  try {
    if (value) {
      sessionStorage.setItem(FORM_FACTOR_SESSION_KEY, value);
    } else {
      sessionStorage.removeItem(FORM_FACTOR_SESSION_KEY);
    }
  } catch {
    /* sessionStorage unavailable - the override lives for this page load only */
  }
}

@Injectable({ providedIn: 'root' })
export class FormFactor {
  private readonly engine = inject(EngineClient);

  private readonly narrow = signal(false);
  private readonly override = signal<FormFactorOverride>(null);
  private readonly mobile = signal(false);

  /** True when the phone shell (`MobileShell`, `/m/**` routes) is in use. */
  readonly isMobile: Signal<boolean> = this.mobile.asReadonly();

  constructor() {
    // Query param wins over the remembered value; a plain load keeps it.
    const fromUrl =
      typeof window !== 'undefined' && window.location
        ? readFormFactorOverride(window.location.search)
        : null;
    if (fromUrl) {
      writeSessionOverride(fromUrl);
      this.override.set(fromUrl);
    } else {
      this.override.set(readSessionOverride());
    }
    this.watchMedia();
    this.recompute();
  }

  /** Force a form factor for this tab (used by tests and the `?ff=` param). */
  setOverride(value: FormFactorOverride): void {
    writeSessionOverride(value);
    this.override.set(value);
    this.recompute();
  }

  private watchMedia(): void {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') {
      return;
    }
    try {
      const mql = window.matchMedia(MOBILE_MEDIA_QUERY);
      this.narrow.set(mql.matches);
      const onChange = (ev: MediaQueryListEvent) => {
        this.narrow.set(ev.matches);
        this.recompute();
      };
      if (typeof mql.addEventListener === 'function') {
        mql.addEventListener('change', onChange);
      } else if (typeof mql.addListener === 'function') {
        mql.addListener(onChange);
      }
    } catch {
      /* matchMedia unavailable (Karma / odd embeds): stay on the desktop shell */
    }
  }

  private recompute(): void {
    const override = this.override();
    if (override) {
      this.mobile.set(override === 'mobile');
      return;
    }
    if (this.engine.isCapacitor) {
      this.mobile.set(true);
      return;
    }
    // A narrow Tauri window is still the desktop app.
    this.mobile.set(!this.engine.isTauri() && this.narrow());
  }
}
