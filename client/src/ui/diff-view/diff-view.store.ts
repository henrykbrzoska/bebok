/**
 * Diff rendering mode (F2-7).
 *
 * `diffViewMode` is chat state per the design handoff, but the toggle sits on
 * every individual diff block, so it lives in a tiny root store instead of
 * being threaded through every tool part. The choice is persisted so a session
 * switch keeps the user's preferred layout.
 */

import { Injectable, signal } from '@angular/core';

export type DiffViewMode = 'unified' | 'split';

const KEY = 'bebok.ui.diffViewMode';

function read(): DiffViewMode {
  try {
    return localStorage.getItem(KEY) === 'split' ? 'split' : 'unified';
  } catch {
    return 'unified';
  }
}

@Injectable({ providedIn: 'root' })
export class DiffViewStore {
  readonly mode = signal<DiffViewMode>(read());

  setMode(mode: DiffViewMode): void {
    this.mode.set(mode);
    try {
      localStorage.setItem(KEY, mode);
    } catch {
      /* storage unavailable - keep the choice in memory only */
    }
  }

  toggle(): void {
    this.setMode(this.mode() === 'unified' ? 'split' : 'unified');
  }
}
