/**
 * Remote-mode state (WP-M6 / F10-24, F10-27): the paired desktop(s), whether
 * the phone is currently pointed at one, and what the link looks like.
 *
 * - `desktops`  : every persisted `desktop` target (`EngineTargetStore`).
 * - `active`    : the desktop the client is switched to right now, or null
 *                 when the active target is the phone's own engine.
 * - `link`      : `live` (stream up) / `offline` (desktop unreachable, cache
 *                 + queue in effect) / `connecting`.
 * - `revoked`   : set when the desktop removed this device. The engine ends
 *                 the remote stream with `event: revoked` and answers every
 *                 further request with 401; `EventsStore` parks itself in the
 *                 `unauthorized` state, which - for a `desktop` target - can
 *                 only mean revocation (the token is per device, never
 *                 rotated). The store then forgets the target, drops its
 *                 offline cache and switches back to the phone's own engine
 *                 (if any), leaving a "device removed" message for the tab.
 *
 * A heartbeat (`POST /remote/heartbeat`, allow-listed for the remote scope)
 * runs every `HEARTBEAT_MS` while a desktop is active and live so the
 * desktop's device list shows "last seen" without waiting for user traffic.
 */

import { Injectable, computed, effect, inject, signal, untracked } from '@angular/core';

import { authFetch } from '../auth.interceptor';
import { ENGINE_API } from '../engine-api';
import { EngineTarget, EngineTargetStore } from '../engine-target.store';
import { EventsStore } from '../events.store';
import { OfflineCache } from './offline-cache';

export const HEARTBEAT_MS = 60_000;
/** Tailscale's Android package (F10-27: "open the Tailscale app"). */
export const TAILSCALE_PACKAGE = 'com.tailscale.ipn';

export type RemoteLink = 'none' | 'connecting' | 'live' | 'offline' | 'revoked';

/** The bit of Capacitor's `AppLauncher` plugin the store needs (injectable for specs). */
export interface AppLauncherLike {
  openUrl(options: { url: string }): Promise<{ completed: boolean }>;
}

@Injectable({ providedIn: 'root' })
export class RemoteStore {
  private readonly engine = inject(ENGINE_API);
  private readonly events = inject(EventsStore);
  private readonly targets = inject(EngineTargetStore);
  private readonly cache = inject(OfflineCache);

  readonly desktops = computed<EngineTarget[]>(() =>
    this.targets.targets().filter((t) => t.kind === 'desktop'),
  );

  /** The active target when it is a paired desktop. */
  readonly active = computed<EngineTarget | null>(() => {
    const target = this.targets.active();
    return target && target.kind === 'desktop' ? target : null;
  });

  /** First paired desktop (1.6 pairs one; the list is kept general). */
  readonly desktop = computed<EngineTarget | null>(() => this.active() ?? this.desktops()[0] ?? null);

  /** Label of the device the desktop removed (cleared by `acknowledgeRevoked`). */
  readonly revoked = signal<string | null>(null);

  readonly link = computed<RemoteLink>(() => {
    if (this.revoked()) {
      return 'revoked';
    }
    if (!this.active()) {
      return 'none';
    }
    switch (this.events.state()) {
      case 'live':
        return 'live';
      case 'connecting':
        return 'connecting';
      default:
        return 'offline';
    }
  });

  readonly offline = computed(() => this.link() === 'offline');

  /** Set by the last probe that failed at the network level (Tailscale-off hint). */
  readonly noRoute = signal(false);

  /** Overrides for specs. */
  heartbeatFetch: typeof authFetch = authFetch;
  /** Loads Capacitor's AppLauncher on a native platform (null in a browser). */
  appLauncher: (() => Promise<AppLauncherLike>) | null = isNativePlatform()
    ? async () => {
        const mod = await import('@capacitor/app-launcher');
        // Wrapped: the plugin Proxy throws on `.then`, which `await` probes.
        const launcher = mod.AppLauncher;
        return { openUrl: (options) => launcher.openUrl(options) };
      }
    : null;

  private heartbeat: ReturnType<typeof setInterval> | null = null;
  private switching = false;

  constructor() {
    // Revocation: a desktop target that answers 401 was removed on the desktop.
    effect(() => {
      const state = this.events.state();
      const active = this.active();
      untracked(() => {
        if (state === 'unauthorized' && active) {
          void this.onRevoked(active);
        }
      });
    });
    effect(() => {
      const live = this.link() === 'live';
      untracked(() => (live ? this.startHeartbeat() : this.stopHeartbeat()));
    });
  }

  /** Point the client at the paired desktop (no-op when already there). */
  async connect(id?: string): Promise<void> {
    const target = id ? this.targets.byId(id) : this.desktop();
    if (!target || target.kind !== 'desktop' || this.switching) {
      return;
    }
    if (this.targets.activeId() === target.id) {
      return;
    }
    this.switching = true;
    try {
      this.revoked.set(null);
      await this.switchAndResume(target.id);
    } finally {
      this.switching = false;
    }
  }

  /** Back to the phone's own engine (embedded / sidecar / typed address). */
  async disconnect(): Promise<void> {
    const platform = this.targets.targets().find((t) => t.kind !== 'desktop');
    if (!platform || this.switching) {
      return;
    }
    this.switching = true;
    try {
      await this.switchAndResume(platform.id);
    } finally {
      this.switching = false;
    }
  }

  /**
   * `switchTarget` plus a stream restart when `EventsStore` had parked itself
   * on a 401: its target effect only restarts a *running* stream, so after a
   * revoke (or any unauthorized state) the switch alone would leave the SSE
   * stream dead.
   */
  private async switchAndResume(id: string): Promise<void> {
    const parked = this.events.state() === 'unauthorized';
    await this.engine.switchTarget(id);
    if (parked) {
      this.events.restart();
    }
  }

  /** Forget a desktop: token, persisted target and its offline cache. */
  async unpair(id: string): Promise<void> {
    const wasActive = this.targets.activeId() === id;
    if (wasActive) {
      await this.disconnect();
    }
    this.targets.remove(id);
    await this.cache.evictTarget(id);
    if (wasActive && this.targets.activeId() === id) {
      // No platform engine to fall back to: leave nothing active.
      this.engine.unauthorized.set(false);
    }
  }

  acknowledgeRevoked(): void {
    this.revoked.set(null);
  }

  /**
   * F10-27: hand over to the Tailscale app (Android intent through
   * Capacitor's AppLauncher; on the web it opens the download page).
   */
  async openTailscale(): Promise<boolean> {
    if (this.appLauncher) {
      try {
        const launcher = await this.appLauncher();
        const result = await launcher.openUrl({ url: TAILSCALE_PACKAGE });
        if (result.completed) {
          return true;
        }
      } catch {
        /* fall through to the web page */
      }
    }
    try {
      window.open('https://tailscale.com/download/android', '_blank', 'noopener');
    } catch {
      return false;
    }
    return true;
  }

  private async onRevoked(target: EngineTarget): Promise<void> {
    this.revoked.set(target.label);
    this.targets.remove(target.id);
    void this.cache.evictTarget(target.id);
    const platform = this.targets.targets().find((t) => t.kind !== 'desktop');
    if (platform) {
      try {
        await this.switchAndResume(platform.id);
      } catch {
        /* the shell shows the connection state */
      }
    } else {
      this.engine.unauthorized.set(false);
      this.events.stop();
    }
  }

  private startHeartbeat(): void {
    if (this.heartbeat !== null) {
      return;
    }
    this.heartbeat = setInterval(() => void this.beat(), HEARTBEAT_MS);
  }

  private stopHeartbeat(): void {
    if (this.heartbeat !== null) {
      clearInterval(this.heartbeat);
      this.heartbeat = null;
    }
  }

  private async beat(): Promise<void> {
    const conn = this.engine.connection();
    if (!conn || !this.active()) {
      return;
    }
    try {
      await this.heartbeatFetch(`${conn.baseUrl}/remote/heartbeat`, { method: 'POST', body: '{}' });
    } catch {
      /* the stream state already reflects reachability */
    }
  }
}

function isNativePlatform(): boolean {
  try {
    const w = window as unknown as { Capacitor?: { isNativePlatform?: () => boolean } };
    return w.Capacitor?.isNativePlatform?.() === true;
  } catch {
    return false;
  }
}
