/**
 * Engine targets (WP-M2 / F10-7): the list of engines this client can talk
 * to and which one is active.
 *
 * - `sidecar`    : the Tauri desktop shell's own engine (re-resolved from the
 *                  shell on every launch, never persisted).
 * - `embedded`   : the engine bundled in the Android app (launched natively,
 *                  re-resolved on every launch, never persisted).
 * - `remote-url` : an engine address typed by hand / bootstrapped with
 *                  `?engine=` (browser dev). The platform default one is
 *                  owned by `TransportStrategy`'s `bebok.remote.*` keys and is
 *                  therefore registered as `ephemeral`; extra ones persist.
 * - `desktop`    : a paired desktop engine reached over Tailscale/LAN with a
 *                  per-device token issued by the engine's `remote` module
 *                  (WP-M1). Persisted; the token goes through `TargetSecrets`.
 *
 * Persistence: `bebok.targets` holds the persisted targets *without* their
 * tokens; `bebok.targets.active` the active id. Tokens are read/written via
 * the `TargetSecrets` interface, defaulting to `localStorage`; on Capacitor
 * the token's factory picks WP-M5's Android Keystore implementation
 * (`secure-store.ts`), nothing else changes.
 *
 * The store is pure state: `EngineClient.switchTarget(id)` is what actually
 * re-points requests, and `EventsStore` restarts its stream when `activeId`
 * changes. Keeping `EngineClient` out of this file avoids an import cycle.
 */

import { Injectable, InjectionToken, computed, inject, signal } from '@angular/core';

import { SecureStoreTargetSecrets, isCapacitorRuntime } from './secure-store';

export type EngineTargetKind = 'sidecar' | 'embedded' | 'remote-url' | 'desktop' | 'share';

export interface EngineTarget {
  id: string;
  kind: EngineTargetKind;
  /** Human label ("This phone", "Desktop · rafal-pc", "http://…"). */
  label: string;
  /** Clean base URL (never tokenised), e.g. `http://100.64.0.7:8790`. */
  baseUrl: string;
  /**
   * 1.8: every base URL this engine advertised at pairing time (tailnet,
   * LAN, relay). `baseUrl` is the one that worked last; `switchTarget`
   * re-races the list so the phone roams between home Wi-Fi and 5G.
   */
  endpoints?: string[];
  /** `share` targets: the one session the token opens (1.8 share links). */
  sessionId?: string;
  /** Bearer token for this engine, null when it runs with `BEBOK_NO_AUTH=1`. */
  token: string | null;
  /** Last successful request/stream against this target (epoch ms). */
  lastOk?: number;
  /**
   * Not persisted even though the kind normally is - used for the platform
   * default `remote-url` target whose source of truth is `bebok.remote.*`.
   */
  ephemeral?: boolean;
}

/**
 * Where target tokens live. The default keeps them in `localStorage` next to
 * the existing `bebok.remote.token`; the Android build (WP-M5) provides a
 * Keystore-backed implementation through `TARGET_SECRETS`.
 */
export interface TargetSecrets {
  read(id: string): Promise<string | null>;
  write(id: string, token: string | null): Promise<void>;
  remove(id: string): Promise<void>;
}

export const TARGETS_KEY = 'bebok.targets';
export const ACTIVE_TARGET_KEY = 'bebok.targets.active';
const SECRET_PREFIX = 'bebok.targets.secret.';

/** `localStorage`-backed secrets (browser dev and the desktop shell). */
export class LocalStorageTargetSecrets implements TargetSecrets {
  async read(id: string): Promise<string | null> {
    try {
      return localStorage.getItem(SECRET_PREFIX + id);
    } catch {
      return null;
    }
  }

  async write(id: string, token: string | null): Promise<void> {
    try {
      if (token) {
        localStorage.setItem(SECRET_PREFIX + id, token);
      } else {
        localStorage.removeItem(SECRET_PREFIX + id);
      }
    } catch {
      /* storage unavailable - the token lives in memory only */
    }
  }

  async remove(id: string): Promise<void> {
    try {
      localStorage.removeItem(SECRET_PREFIX + id);
    } catch {
      /* ignore */
    }
  }
}

export const TARGET_SECRETS = new InjectionToken<TargetSecrets>('TARGET_SECRETS', {
  providedIn: 'root',
  // WP-M5 (F10-20): the Capacitor shell gets the Android Keystore-backed
  // store; browser dev and the desktop shell keep `localStorage`.
  factory: () =>
    isCapacitorRuntime() ? new SecureStoreTargetSecrets() : new LocalStorageTargetSecrets(),
});

/** Kinds that survive a reload (the others are re-resolved by the platform). */
export function isPersistedTarget(target: EngineTarget): boolean {
  return (
    (target.kind === 'desktop' || target.kind === 'remote-url' || target.kind === 'share') &&
    !target.ephemeral
  );
}

type StoredTarget = Omit<EngineTarget, 'token' | 'ephemeral'>;

@Injectable({ providedIn: 'root' })
export class EngineTargetStore {
  private readonly secrets = inject(TARGET_SECRETS);

  readonly targets = signal<EngineTarget[]>([]);
  readonly activeId = signal<string | null>(null);
  readonly active = computed<EngineTarget | null>(() => {
    const id = this.activeId();
    return id === null ? null : (this.targets().find((t) => t.id === id) ?? null);
  });

  /**
   * Why the platform's own engine could not be resolved (the embedded engine
   * failed to launch on the phone, the sidecar handshake failed). Onboarding
   * (WP-M5) shows it instead of the old silent fall-through to a saved URL.
   */
  readonly platformError = signal<string | null>(null);

  /**
   * F10-31: a paired `desktop` target talks to the engine's *remote*
   * listener with a device token whose scope (WP-M1 `remote/scope.rs`) 403s
   * everything a phone does not need to follow a session - `GET /config`,
   * `GET /tools/safety`, `fs/*`, ... Views consult this before issuing such
   * calls, so the desktop's engine log is not filled with
   * `remote scope denied` every time the phone opens a session. `remote-url`
   * targets (an engine address typed in) use the engine's own launch token
   * and keep the full local scope.
   */
  readonly remoteScope = computed(() => {
    const kind = this.active()?.kind;
    return kind === 'desktop' || kind === 'share';
  });
  /** 1.8: joined share links, newest first. */
  readonly shares = computed(() => this.targets().filter((t) => t.kind === 'share'));

  /** Resolves once persisted targets (and their tokens) are back in memory. */
  readonly ready: Promise<void>;

  constructor() {
    this.ready = this.hydrate();
  }

  /**
   * Add or replace a target (matched by id). Persisted kinds are written
   * through immediately; the token goes to `TargetSecrets`. Returns the
   * stored entry.
   */
  upsert(target: EngineTarget): EngineTarget {
    const entry: EngineTarget = { ...target };
    this.targets.update((list) => {
      const idx = list.findIndex((t) => t.id === entry.id);
      if (idx < 0) {
        return [...list, entry];
      }
      const next = list.slice();
      const merged = { ...list[idx], ...entry };
      // Never lose known endpoints: union, the fresh order first.
      const known = [...(entry.endpoints ?? []), ...(list[idx].endpoints ?? [])];
      if (known.length > 0) {
        merged.endpoints = known.filter((url, i) => known.indexOf(url) === i);
      }
      next[idx] = merged;
      return next;
    });
    if (isPersistedTarget(entry)) {
      this.persist();
      void this.secrets.write(entry.id, entry.token);
    }
    return entry;
  }

  /** Forget a target. The active id is cleared when it pointed at it. */
  remove(id: string): void {
    const existing = this.targets().find((t) => t.id === id);
    this.targets.update((list) => list.filter((t) => t.id !== id));
    if (this.activeId() === id) {
      this.activeId.set(null);
      this.persistActive();
    }
    if (existing && isPersistedTarget(existing)) {
      this.persist();
      void this.secrets.remove(id);
    }
  }

  /** Mark `id` active. Unknown ids are ignored (returns false). */
  setActive(id: string): boolean {
    if (!this.targets().some((t) => t.id === id)) {
      return false;
    }
    if (this.activeId() !== id) {
      this.activeId.set(id);
      this.persistActive();
    }
    return true;
  }

  /** Stamp `lastOk` on a target after a successful request/stream. */
  markOk(id: string, at: number = Date.now()): void {
    let changed = false;
    this.targets.update((list) =>
      list.map((t) => {
        if (t.id !== id) {
          return t;
        }
        changed = true;
        return { ...t, lastOk: at };
      }),
    );
    if (changed) {
      this.persist();
    }
  }

  byId(id: string): EngineTarget | null {
    return this.targets().find((t) => t.id === id) ?? null;
  }

  /** Token for `id`: the in-memory value, else whatever `TargetSecrets` holds. */
  async tokenFor(id: string): Promise<string | null> {
    const target = this.byId(id);
    if (target?.token) {
      return target.token;
    }
    return this.secrets.read(id);
  }

  // ---------------------------------------------------------------------------
  // persistence
  // ---------------------------------------------------------------------------

  private async hydrate(): Promise<void> {
    let stored: StoredTarget[] = [];
    let active: string | null = null;
    try {
      const raw = localStorage.getItem(TARGETS_KEY);
      const parsed: unknown = raw ? JSON.parse(raw) : [];
      if (Array.isArray(parsed)) {
        stored = parsed.filter(isStoredTarget);
      }
      active = localStorage.getItem(ACTIVE_TARGET_KEY);
    } catch {
      stored = [];
    }
    if (stored.length === 0) {
      return;
    }
    const restored: EngineTarget[] = [];
    for (const entry of stored) {
      restored.push({ ...entry, token: await this.secrets.read(entry.id) });
    }
    // Targets registered by the platform while we were reading win over the
    // stored copy of the same id (they carry fresher URLs/tokens).
    this.targets.update((list) => {
      const ids = new Set(list.map((t) => t.id));
      return [...list, ...restored.filter((t) => !ids.has(t.id))];
    });
    if (active && this.activeId() === null && this.targets().some((t) => t.id === active)) {
      this.activeId.set(active);
    }
  }

  private persist(): void {
    const stored: StoredTarget[] = this.targets()
      .filter(isPersistedTarget)
      .map(({ token: _token, ephemeral: _ephemeral, ...rest }) => rest);
    try {
      localStorage.setItem(TARGETS_KEY, JSON.stringify(stored));
    } catch {
      /* storage unavailable - in-memory only */
    }
  }

  private persistActive(): void {
    try {
      const id = this.activeId();
      if (id === null) {
        localStorage.removeItem(ACTIVE_TARGET_KEY);
      } else {
        localStorage.setItem(ACTIVE_TARGET_KEY, id);
      }
    } catch {
      /* ignore */
    }
  }
}

function isStoredTarget(value: unknown): value is StoredTarget {
  if (!value || typeof value !== 'object') {
    return false;
  }
  const v = value as Record<string, unknown>;
  return (
    typeof v['id'] === 'string' &&
    typeof v['baseUrl'] === 'string' &&
    typeof v['label'] === 'string' &&
    (v['kind'] === 'desktop' || v['kind'] === 'remote-url' || v['kind'] === 'share') &&
    (v['sessionId'] === undefined || typeof v['sessionId'] === 'string') &&
    (v['endpoints'] === undefined ||
      (Array.isArray(v['endpoints']) && v['endpoints'].every((e) => typeof e === 'string')))
  );
}
