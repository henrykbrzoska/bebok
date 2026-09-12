/**
 * Opens the browser viewer window (WP-BROWSER2 / F7-6).
 *
 * The viewer is the shell-less `/browser-view?session=<id>` route rendered in
 * its own window: a second Tauri `WebviewWindow` on the desktop (created by
 * the `open_browser_viewer` command, positioned right of the main window), a
 * plain `window.open` popup everywhere else. Both load the same bundle; the
 * page connects to the engine on its own (`EngineClient.connect()`), so
 * nothing has to be handed over except the session id in the URL.
 */

import { Injectable, inject } from '@angular/core';

import { EngineClient } from './engine-client.service';

export const VIEWER_ROUTE = '/browser-view';
export const VIEWER_WIDTH = 1360;
export const VIEWER_HEIGHT = 980;

/** URL of the viewer page for a session, relative to the app origin. */
export function viewerPath(sessionId: string): string {
  return `${VIEWER_ROUTE}?session=${encodeURIComponent(sessionId)}`;
}

/** Stable per-session window name so re-opening focuses instead of duplicating. */
export function viewerWindowName(sessionId: string): string {
  return `bebok-browser-${sessionId.replace(/[^a-zA-Z0-9-]/g, '_')}`;
}

/** `window.open` feature string sized for a 1280x800 page plus toolbar. */
export function viewerWindowFeatures(
  screen: { availWidth?: number; availHeight?: number } = {},
): string {
  const width = Math.min(VIEWER_WIDTH, screen.availWidth ?? VIEWER_WIDTH);
  const height = Math.min(VIEWER_HEIGHT, screen.availHeight ?? VIEWER_HEIGHT);
  return `popup=yes,width=${width},height=${height},resizable=yes,scrollbars=no`;
}

@Injectable({ providedIn: 'root' })
export class BrowserViewerService {
  private readonly engine = inject(EngineClient);

  /**
   * Open (or focus) the viewer window for `sessionId`. Returns `false` when
   * the window could not be opened (popup blocked, Tauri command failed).
   */
  async open(sessionId: string): Promise<boolean> {
    if (this.engine.isTauri()) {
      try {
        const { invoke } = await import('@tauri-apps/api/core');
        await invoke('open_browser_viewer', { sessionId });
        return true;
      } catch (err) {
        console.error('open_browser_viewer failed; falling back to window.open', err);
      }
    }
    return this.openPopup(sessionId);
  }

  private openPopup(sessionId: string): boolean {
    if (typeof window === 'undefined') {
      return false;
    }
    const url = new URL(viewerPath(sessionId), window.location.origin).toString();
    const popup = window.open(
      url,
      viewerWindowName(sessionId),
      viewerWindowFeatures(window.screen),
    );
    if (!popup) {
      return false;
    }
    try {
      popup.focus();
    } catch {
      /* cross-window focus is best effort */
    }
    return true;
  }
}
