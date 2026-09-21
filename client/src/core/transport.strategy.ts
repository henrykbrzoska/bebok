/**
 * Transport strategy (SPEC §7): the single place that decides which engine the
 * UI talks to.
 *
 * - `tauri`   : desktop shell spawned the engine as a sidecar; the Rust shell
 *               exposes `engine_info` (base URL).
 * - `http`    : remote engine (browser dev against a manually started engine,
 *               later the Capacitor/mobile shell).
 *
 * Platform-specific imports are DYNAMIC (`import('@tauri-apps/...')`) and only
 * resolved when the runtime is actually Tauri, never statically - otherwise
 * the shared bundle breaks on shells without the Tauri API (Capacitor, M6).
 *
 * Auth (F0-5): whatever URL a shell hands us may carry the engine's per-launch
 * capability token as `?token=…`. Every path through this file funnels the URL
 * through `adopt()`, which stores the token (`setEngineToken`) and keeps only
 * the clean base URL for request building.
 *
 * Bootstrap (F9-17): in browser mode the page URL itself may carry the engine
 * address as `?engine=<url-encoded BEBOK_READY url>`. A launcher (the root
 * `npm run full-build-dev` orchestrator) opens the dev server that way so the
 * `BEBOK_READY` line never has to be pasted by hand. The parameter is adopted
 * exactly once - persisted like a manual "Connect" - and then stripped from
 * the address bar (`history.replaceState`) so reloads, bookmarks and copied
 * links do not keep re-applying (or leaking) the token.
 *
 * Fixed profile (Phase 2): the client can be configured to always connect to
 * a pre-defined engine (http://localhost:8787 + token). When `profileKind` is
 * `'fixed'` the Start screen skips the address form and the sidebar offers a
 * Connect/Disconnect toggle instead of requiring manual address entry.
 */

import { setEngineToken, splitEngineUrl } from './auth.interceptor';

export type PlatformKind = 'tauri' | 'http';

export type ConnectionProfileKind = 'tauri' | 'fixed' | 'manual';

export interface ConnectionProfile {
  kind: ConnectionProfileKind;
  baseUrl: string;
  token: string | null;
}

export interface EngineConnection {
  kind: PlatformKind;
  /** Base URL of the engine, e.g. `http://127.0.0.1:45617` (never tokenised). */
  baseUrl: string;
}

export interface EngineInfo {
  baseUrl: string;
}

const REMOTE_URL_KEY = 'bebok.remote.baseUrl';
const REMOTE_TOKEN_KEY = 'bebok.remote.token';
const PROFILE_KIND_KEY = 'bebok.remote.profileKind';
const DIRECTORY_KEY = 'bebok.lastDirectory';

const DEFAULT_REMOTE_URL = 'http://127.0.0.1:8787';

/** Query parameter carrying the engine URL (+ token) on first load (F9-17). */
export const BOOTSTRAP_ENGINE_PARAM = 'engine';

/**
 * Read the `?engine=` bootstrap value from a page query string. Returns the
 * decoded engine URL (still carrying its own `?token=` when the engine issued
 * one), or null when the parameter is absent, empty or not an http(s) URL.
 */
export function readBootstrapEngine(search: string): string | null {
  const raw = new URLSearchParams(search ?? '').get(BOOTSTRAP_ENGINE_PARAM);
  const value = (raw ?? '').trim();
  if (!value) {
    return null;
  }
  try {
    const parsed = new URL(value);
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
      return null;
    }
  } catch {
    return null;
  }
  return value;
}

/**
 * The same page URL without the `?engine=` parameter (other query parameters
 * and the hash are kept). Used to rewrite the address bar after adoption.
 */
export function stripBootstrapParam(href: string): string {
  const url = new URL(href);
  url.searchParams.delete(BOOTSTRAP_ENGINE_PARAM);
  return url.toString();
}

export class TransportStrategy {
  private readonly kind: PlatformKind;
  private readonly capacitor: boolean;

  constructor() {
    this.kind = this.detect();
    this.capacitor = this.detectCapacitor();
    if (this.kind === 'http') {
      this.adoptBootstrapParam();
    }
  }

  get platform(): PlatformKind {
    return this.kind;
  }

  /** True when running inside the Capacitor (mobile) shell. */
  get isCapacitor(): boolean {
    return this.capacitor;
  }

  /** Tauri exposes `window.__TAURI_INTERNALS__` (v2) - no other signal needed. */
  private detect(): PlatformKind {
    if (typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window) {
      return 'tauri';
    }
    return 'http';
  }

  /** Capacitor injects `window.Capacitor` and a `Capacitor` UA token. */
  private detectCapacitor(): boolean {
    if (typeof window === 'undefined') {
      return false;
    }
    const ua = navigator.userAgent || '';
    if (ua.includes('Capacitor')) {
      return true;
    }
    return 'Capacitor' in window;
  }

  // -------------------------------------------------------------------------
  // Connection profile (Phase 2)
  // -------------------------------------------------------------------------

  /** Read the persisted connection profile (kind, URL, token). */
  readProfile(): ConnectionProfile {
    let kind: ConnectionProfileKind = 'manual';
    try {
      const stored = localStorage.getItem(PROFILE_KIND_KEY);
      if (stored === 'fixed' || stored === 'tauri') {
        kind = stored;
      }
    } catch {
      /* ignore */
    }
    return {
      kind,
      baseUrl: this.readRemoteUrl(),
      token: this.readRemoteToken(),
    };
  }

  /**
   * Persist a fixed-engine profile: saves URL + token + profile kind.
   * Delegates to `saveRemote` for the URL/token pair so existing bootstrap
   * and adopt paths keep working.
   */
  saveFixedProfile(baseUrl: string, token?: string | null): void {
    this.saveRemote(baseUrl, token);
    try {
      localStorage.setItem(PROFILE_KIND_KEY, 'fixed');
    } catch {
      /* ignore */
    }
  }

  /** Clear the connection profile, reverting to `'manual'` mode. */
  clearProfile(): void {
    try {
      localStorage.removeItem(PROFILE_KIND_KEY);
      localStorage.removeItem(REMOTE_URL_KEY);
      localStorage.removeItem(REMOTE_TOKEN_KEY);
    } catch {
      /* ignore */
    }
  }

  /** True when the user explicitly configured a fixed-engine connection. */
  isFixedProfile(): boolean {
    try {
      return localStorage.getItem(PROFILE_KIND_KEY) === 'fixed';
    } catch {
      return false;
    }
  }

  /**
   * Resolve the engine connection for the current platform. In the desktop
   * shell the engine is already spawned as a sidecar; this waits for its
   * `BEBOK_READY` handshake (Rust blocks until the port is known). On the
   * mobile (Capacitor) shell the embedded engine is launched natively and its
   * local URL is returned.
   */
  async connect(): Promise<EngineConnection> {
    if (this.kind === 'tauri') {
      const { invoke } = await import('@tauri-apps/api/core');
      const info = await invoke<EngineInfo>('engine_info');
      return this.adopt('tauri', info.baseUrl);
    }

    if (this.capacitor) {
      try {
        const { EngineLauncher } = await import('./engine-launcher');
        const info = await EngineLauncher.start();
        return this.adopt('http', info.baseUrl);
      } catch (err) {
        // Embedded engine unavailable (e.g. web build on a phone browser) -
        // fall back to a manually configured remote URL.
        console.error('embedded engine launch failed; falling back to remote URL', err);
      }
    }

    // Browser/remote mode: the engine URL was typed (or restored from a
    // previous session). A token pasted along with it - the engine prints
    // `BEBOK_READY http://host:port/?token=...` - is picked up here; against an
    // engine started with `BEBOK_NO_AUTH=1` there simply is none.
    return this.adopt('http', this.readRemoteUrl(), this.readRemoteToken());
  }

  /**
   * F9-17: adopt `?engine=<url>` from the page address (browser mode only).
   * Behaves like a manual "Connect" with that URL: the base URL and token are
   * persisted so the normal `connect()` path (and the Start view's address
   * form) pick them up. A bootstrap URL without a token (engine started with
   * `BEBOK_NO_AUTH=1`) also drops any token remembered from an earlier engine,
   * which would otherwise be sent - harmlessly, but confusingly - as a stale
   * bearer. Finally the parameter is removed from the address bar.
   */
  private adoptBootstrapParam(): void {
    if (typeof window === 'undefined' || !window.location) {
      return;
    }
    const raw = readBootstrapEngine(window.location.search);
    if (!raw) {
      return;
    }
    const { baseUrl, token } = splitEngineUrl(raw);
    if (!baseUrl) {
      return;
    }
    this.saveRemote(baseUrl, token);
    if (!token) {
      this.clearRemoteToken();
    }
    try {
      window.history.replaceState(
        window.history.state,
        '',
        stripBootstrapParam(window.location.href),
      );
    } catch {
      /* history API unavailable (tests / odd embeds) - the param is simply left in place */
    }
  }

  private clearRemoteToken(): void {
    try {
      localStorage.removeItem(REMOTE_TOKEN_KEY);
    } catch {
      /* ignore */
    }
  }

  /**
   * Adopt a manually configured remote engine (connect view): persist URL and
   * token, then return the connection with a clean base URL.
   */
  adoptRemote(conn: EngineConnection): EngineConnection {
    const { baseUrl, token } = splitEngineUrl(conn.baseUrl);
    this.saveRemote(baseUrl, token);
    return this.adopt(conn.kind, baseUrl, token ?? this.readRemoteToken());
  }

  /**
   * Take an engine URL as announced by a shell, remember the capability token
   * it carries and return the connection with a clean base URL. The single
   * place the token enters the client.
   */
  adopt(kind: PlatformKind, rawUrl: string, fallbackToken?: string | null): EngineConnection {
    const { baseUrl, token } = splitEngineUrl(rawUrl);
    setEngineToken(token ?? fallbackToken ?? null);
    return { kind, baseUrl };
  }

  /**
   * Native directory picker (Tauri dialog). In plain browser/http mode there
   * is no safe directory picker - the caller falls back to typing a path.
   */
  async pickDirectory(title: string): Promise<string | null> {
    if (this.kind !== 'tauri') {
      return null;
    }
    const { open } = await import('@tauri-apps/plugin-dialog');
    const picked = await open({
      directory: true,
      multiple: false,
      title,
    });
    return typeof picked === 'string' ? picked : null;
  }

  /**
   * Native file picker (Tauri dialog), used to browse for executable paths.
   * Null in browser/http mode (no safe arbitrary-path file picker).
   */
  async pickFile(title: string): Promise<string | null> {
    if (this.kind !== 'tauri') {
      return null;
    }
    const { open } = await import('@tauri-apps/plugin-dialog');
    const picked = await open({
      directory: false,
      multiple: false,
      title,
    });
    return typeof picked === 'string' ? picked : null;
  }

  /**
   * Persist the remote engine location for browser/mobile sessions. A token is
   * persisted alongside it (browser/remote mode only) so a page reload does not
   * force the user to paste the `BEBOK_READY` URL again; the Tauri and
   * Capacitor shells never take this path - they receive a fresh token from the
   * engine handshake on every launch.
   */
  saveRemote(baseUrl: string, token?: string | null): void {
    const clean = splitEngineUrl(baseUrl);
    try {
      localStorage.setItem(REMOTE_URL_KEY, clean.baseUrl || DEFAULT_REMOTE_URL);
      const effective = token ?? clean.token;
      if (effective) {
        localStorage.setItem(REMOTE_TOKEN_KEY, effective);
      }
    } catch {
      /* localStorage unavailable - keep defaults in memory only */
    }
  }

  readRemoteUrl(): string {
    try {
      return localStorage.getItem(REMOTE_URL_KEY) ?? DEFAULT_REMOTE_URL;
    } catch {
      return DEFAULT_REMOTE_URL;
    }
  }

  /** Capability token remembered for the manually configured remote engine. */
  readRemoteToken(): string | null {
    try {
      return localStorage.getItem(REMOTE_TOKEN_KEY);
    } catch {
      return null;
    }
  }

  /** Remember the last picked directory across GUI restarts. */
  saveDirectory(directory: string): void {
    try {
      localStorage.setItem(DIRECTORY_KEY, directory);
    } catch {
      /* ignore */
    }
  }

  readLastDirectory(): string | null {
    try {
      return localStorage.getItem(DIRECTORY_KEY);
    } catch {
      return null;
    }
  }
}
