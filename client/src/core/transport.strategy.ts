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
 */

export type PlatformKind = 'tauri' | 'http';

export interface EngineConnection {
  kind: PlatformKind;
  /** Base URL of the engine, e.g. `http://127.0.0.1:45617`. */
  baseUrl: string;
}

export interface EngineInfo {
  baseUrl: string;
}

const REMOTE_URL_KEY = 'bebok.remote.baseUrl';
const DIRECTORY_KEY = 'bebok.lastDirectory';

const DEFAULT_REMOTE_URL = 'http://127.0.0.1:8787';

export class TransportStrategy {
  private readonly kind: PlatformKind;
  private readonly capacitor: boolean;

  constructor() {
    this.kind = this.detect();
    this.capacitor = this.detectCapacitor();
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
      return { kind: 'tauri', baseUrl: info.baseUrl };
    }

    if (this.capacitor) {
      try {
        const { EngineLauncher } = await import('./engine-launcher');
        const info = await EngineLauncher.start();
        return { kind: 'http', baseUrl: info.baseUrl };
      } catch (err) {
        // Embedded engine unavailable (e.g. web build on a phone browser) -
        // fall back to a manually configured remote URL.
        console.error('embedded engine launch failed; falling back to remote URL', err);
      }
    }

    return {
      kind: 'http',
      baseUrl: this.readRemoteUrl(),
    };
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

  /** Persist the remote engine location for browser/mobile sessions. */
  saveRemote(baseUrl: string): void {
    try {
      localStorage.setItem(REMOTE_URL_KEY, baseUrl.trim() || DEFAULT_REMOTE_URL);
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
