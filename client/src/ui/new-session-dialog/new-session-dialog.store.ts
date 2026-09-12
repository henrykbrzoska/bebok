/**
 * Open/close state of the "New session" dialog (WP-GIT / F6-16).
 *
 * Both entry points - the sidebar's "+ New" and the Start screen's
 * "New session" - call `openFor(directory, { agent })` instead of hitting
 * `EngineClient.createSession` directly. The dialog component itself is
 * mounted once (inside the always-present sidebar) and reads this store, so
 * it does not matter which screen asked for it. Ephemeral: never persisted.
 */

import { Injectable, signal } from '@angular/core';

export interface NewSessionRequest {
  /** Project directory the session is created in (engine-normalised). */
  directory: string;
  /** Agent preset preselected by the caller (its inline `<select>`), if any. */
  agent?: string;
}

@Injectable({ providedIn: 'root' })
export class NewSessionDialogStore {
  readonly request = signal<NewSessionRequest | null>(null);
  readonly open = () => this.request() !== null;

  openFor(directory: string, options: { agent?: string } = {}): void {
    this.request.set({ directory, agent: options.agent });
  }

  close(): void {
    this.request.set(null);
  }
}
