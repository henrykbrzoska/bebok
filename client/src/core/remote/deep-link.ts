/**
 * `bebok://pair?…` deep link (WP-M6 / F10-22): the scan-free way into the
 * pairing flow - tap a link on the desktop's pairing card, an e2e hook for
 * WP-M8 (`adb shell am start -a android.intent.action.VIEW -d "bebok://pair?…"`),
 * and the fallback when the camera cannot be used.
 *
 * On Android the manifest's `<intent-filter>` (scheme `bebok`, host `pair`)
 * routes the URL to Capacitor's `App` plugin: `appUrlOpen` while running,
 * `getLaunchUrl()` on a cold start. The plugin is loaded dynamically so the
 * shared bundle has no Capacitor runtime dependency on the desktop.
 *
 * In a browser the same invite can be handed over as `/m/remote?pair=<url>`
 * (URL-encoded `bebok://…`), which the Remote tab reads directly; this
 * service normalises both sources into `pending`.
 */

import { Injectable, inject, signal } from '@angular/core';
import { CanActivateChildFn, Router } from '@angular/router';

import { PairInvite, parsePairUrl } from './pair-protocol';

/** The subset of `@capacitor/app` used here (injectable for specs). */
export interface AppPluginLike {
  getLaunchUrl(): Promise<{ url?: string } | null | undefined>;
  addListener(
    event: 'appUrlOpen',
    listener: (data: { url: string }) => void,
  ): Promise<{ remove: () => Promise<void> }> | { remove: () => Promise<void> };
}

@Injectable({ providedIn: 'root' })
export class PairDeepLinks {
  private readonly router = inject(Router);

  /** Invite waiting to be consumed by the pairing screen. */
  readonly pending = signal<PairInvite | null>(null);
  /** Last deep link that could not be parsed (reason for the UI). */
  readonly lastError = signal<string | null>(null);

  private installed = false;

  /**
   * Load `@capacitor/app` (overridable in specs). The plugin object is a
   * Proxy that throws for unknown members - including `then` - so it must
   * never be returned straight out of an async function (`await` would probe
   * `.then` and reject with "App.then() is not implemented"); wrap it.
   */
  loadPlugin: () => Promise<AppPluginLike> = async () => {
    const mod = await import('@capacitor/app');
    const app = mod.App;
    return {
      getLaunchUrl: () => app.getLaunchUrl(),
      addListener: (event, listener) => app.addListener(event, listener),
    };
  };

  /**
   * Hook the native deep-link events. Safe to call more than once and on
   * non-Capacitor platforms (no-op when the plugin is unavailable).
   */
  async install(): Promise<void> {
    if (this.installed) {
      return;
    }
    this.installed = true;
    let plugin: AppPluginLike;
    try {
      plugin = await this.loadPlugin();
    } catch {
      return;
    }
    try {
      await plugin.addListener('appUrlOpen', (data) => this.handle(data.url));
    } catch {
      /* not native */
    }
    try {
      const launch = await plugin.getLaunchUrl();
      // The launch intent outlives the page: a reload of the WebView would
      // see the same URL again and re-enter pairing with a spent code, so a
      // launch URL is consumed once per app process (sessionStorage).
      if (launch?.url && readConsumed() !== launch.url) {
        markConsumed(launch.url);
        this.handle(launch.url);
      }
    } catch {
      /* not native */
    }
  }

  /**
   * Accept a raw `bebok://pair?…` URL: parse, stash as `pending` and steer
   * the router to the Remote tab. Returns the invite or null.
   */
  handle(url: string): PairInvite | null {
    let invite: PairInvite;
    try {
      invite = parsePairUrl(url);
    } catch (err) {
      this.lastError.set(err instanceof Error ? err.message : String(err));
      this.pending.set(null);
      return null;
    }
    this.lastError.set(null);
    this.pending.set(invite);
    void this.router.navigate(['/m/remote'], { queryParams: { pair: url } });
    return invite;
  }

  /** Take (and clear) the pending invite. */
  consume(): PairInvite | null {
    const invite = this.pending();
    this.pending.set(null);
    return invite;
  }
}

const CONSUMED_KEY = 'bebok.pair.launchUrl';

function readConsumed(): string | null {
  try {
    return sessionStorage.getItem(CONSUMED_KEY);
  } catch {
    return null;
  }
}

function markConsumed(url: string): void {
  try {
    sessionStorage.setItem(CONSUMED_KEY, url);
  } catch {
    /* memory only */
  }
}

/**
 * Route hook (`mobile.routes.ts`): installs the native deep-link listener the
 * first time any `/m/**` route activates, so a cold start from
 * `bebok://pair?…` is picked up (`getLaunchUrl`) no matter which tab the app
 * opens on. Idempotent, never blocks navigation.
 */
export const installPairDeepLinks: CanActivateChildFn = () => {
  void inject(PairDeepLinks).install();
  return true;
};
