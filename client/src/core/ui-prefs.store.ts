/**
 * Chrome preferences (Task 6): collapsible sidebar / compact topbar.
 *
 * Persisted to `localStorage` so layout choices survive reloads. The engine
 * is untouched — this is purely client-side chrome state.
 */

import { Injectable, signal } from '@angular/core';

const KEY_SIDEBAR = 'bebok.ui.sidebarVisible';
const KEY_SIDEBAR_WIDTH = 'bebok.ui.sidebarWidth';
const KEY_TOPBAR = 'bebok.ui.topbarCompact';

export const DEFAULT_SIDEBAR_WIDTH = 230;
export const MIN_SIDEBAR_WIDTH = 150;
export const MAX_SIDEBAR_WIDTH = 480;

function readBool(key: string, fallback: boolean): boolean {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) {
      return fallback;
    }
    return raw === 'true';
  } catch {
    return fallback;
  }
}

function readWidth(): number {
  try {
    const raw = Number(localStorage.getItem(KEY_SIDEBAR_WIDTH));
    if (!Number.isFinite(raw)) {
      return DEFAULT_SIDEBAR_WIDTH;
    }
    return Math.min(MAX_SIDEBAR_WIDTH, Math.max(MIN_SIDEBAR_WIDTH, raw));
  } catch {
    return DEFAULT_SIDEBAR_WIDTH;
  }
}

@Injectable({ providedIn: 'root' })
export class UiPrefsStore {
  /** Chat sidebar visibility (session summary). */
  readonly sidebarVisible = signal(readBool(KEY_SIDEBAR, true));
  /** Chat sidebar width in px (drag handle). */
  readonly sidebarWidth = signal(readWidth());
  /** Compact topbar (smaller padding, hidden session tabs). */
  readonly topbarCompact = signal(readBool(KEY_TOPBAR, false));

  toggleSidebar(): void {
    this.setSidebarVisible(!this.sidebarVisible());
  }

  setSidebarVisible(visible: boolean): void {
    this.sidebarVisible.set(visible);
    try {
      localStorage.setItem(KEY_SIDEBAR, String(visible));
    } catch {
      /* non-critical */
    }
  }

  setSidebarWidth(px: number): void {
    const clamped = Math.min(MAX_SIDEBAR_WIDTH, Math.max(MIN_SIDEBAR_WIDTH, Math.round(px)));
    this.sidebarWidth.set(clamped);
    try {
      localStorage.setItem(KEY_SIDEBAR_WIDTH, String(clamped));
    } catch {
      /* non-critical */
    }
  }

  toggleTopbar(): void {
    this.setTopbarCompact(!this.topbarCompact());
  }

  setTopbarCompact(compact: boolean): void {
    this.topbarCompact.set(compact);
    try {
      localStorage.setItem(KEY_TOPBAR, String(compact));
    } catch {
      /* non-critical */
    }
  }
}
