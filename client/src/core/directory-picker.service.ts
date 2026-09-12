/**
 * Directory picker strategy (F5-4).
 *
 * Why NOT the browser's File System Access API (`showDirectoryPicker()`): it
 * hands back an opaque `FileSystemDirectoryHandle`, and there is no API to
 * recover an absolute filesystem path from one. Bebok's engine is rooted in
 * real path strings (every session, `/fs/*` call and tool invocation takes a
 * path), so a handle cannot be passed to the REST API at all - this is an
 * architectural mismatch, not an inconvenience. Do not "fix" this later by
 * reaching for that API.
 *
 * Strategies:
 * - Tauri desktop -> the native OS dialog (`EngineClient.pickDirectory`),
 *   with the in-app browser as a fallback if that capability is unavailable.
 * - Browser and Capacitor -> the engine-backed `DirectoryBrowser` modal
 *   (F5-5), which lists directories over `GET /fs/browse`.
 *
 * The modal is bridged to this promise-returning service by a pair of signals
 * that the always-mounted `<app-directory-browser />` reads: `openRequest`
 * (title + a resolver) is set by `pick()` and cleared by `resolve()`. That is
 * the least invasive option - no dynamic component creation, no dialog-host
 * service, and the same modal instance can also be embedded elsewhere later.
 */

import { Injectable, inject, signal } from '@angular/core';

import { EngineClient } from './engine-client.service';

interface PickRequest {
  title: string;
  resolve: (path: string | null) => void;
}

@Injectable({ providedIn: 'root' })
export class DirectoryPicker {
  private readonly engine = inject(EngineClient);

  /** Non-null while the in-app browser modal should be open. */
  readonly request = signal<PickRequest | null>(null);

  /** Resolve with an absolute path, or null when the user cancels. */
  async pick(title: string): Promise<string | null> {
    if (this.engine.platform === 'tauri') {
      try {
        return normalize(await this.engine.pickDirectory(title));
      } catch {
        // A desktop build without the dialog capability can still use the
        // engine-backed picker. This keeps selection usable on both OSes.
      }
    }
    return this.pickInApp(title);
  }

  private pickInApp(title: string): Promise<string | null> {
    // Only one picker at a time: a second call cancels the pending one.
    this.request()?.resolve(null);
    return new Promise<string | null>((resolve) => {
      this.request.set({ title, resolve: (path) => resolve(normalize(path)) });
    });
  }

  /** Called by the modal: confirm a path, or cancel with null. */
  resolve(path: string | null): void {
    const pending = this.request();
    this.request.set(null);
    pending?.resolve(path);
  }
}

function normalize(path: string | null): string | null {
  const value = path?.trim();
  return value ? value : null;
}
