/**
 * Shared "selected file" state for the Explorer (F2-13).
 *
 * The drawer's mini tree and the full-screen Explorer view must agree on which
 * file is selected. Originally added to `ui/right-drawer/panels/` (the
 * handoff target was `core/`, but `core/*` was read-only for that work
 * package); F7-3 moves the real implementation here. The old path re-exports
 * from this file (see `ui/right-drawer/panels/explorer-selection.store.ts`)
 * so importers that still point there - notably `views/chat/parts/text-part.ts`,
 * out of scope for this change - keep working unchanged.
 */

import { Injectable, signal } from '@angular/core';

/**
 * F6-10: one explicit "jump to this file in Preview" request. A plain nonce
 * bump (rather than just re-using `selectedPath`) lets the Preview panel tell
 * a genuine "open in preview" click apart from incidental tree browsing, and
 * re-navigate even when the same file is re-opened.
 */
export interface PreviewRequest {
  directory: string;
  path: string;
  nonce: number;
}

/**
 * F7-3: one explicit "open this file in the full-screen Explorer" request,
 * fired by the right-drawer Explorer panel when a file row is clicked. Same
 * nonce idiom as `PreviewRequest`, consumed once by `ExplorerView` and then
 * cleared (`clearOpenRequest`) so navigating back to `/explorer` on its own -
 * without a fresh click in the drawer - doesn't replay a stale selection.
 */
export interface ExplorerOpenRequest {
  directory: string;
  path: string;
  nonce: number;
}

@Injectable({ providedIn: 'root' })
export class ExplorerSelectionStore {
  /** Project-relative path of the selected file, or null when none. */
  readonly selectedPath = signal<string | null>(null);
  /** Directory the selection belongs to (absolute project root). */
  readonly directory = signal<string | null>(null);
  /** Latest explicit "open in Preview" request, or null before the first one. */
  readonly previewRequest = signal<PreviewRequest | null>(null);
  /** Latest explicit "open in full-screen Explorer" request (F7-3), cleared
   *  once `ExplorerView` has consumed it. */
  readonly openRequest = signal<ExplorerOpenRequest | null>(null);

  private previewNonce = 0;
  private openNonce = 0;

  select(directory: string | null, path: string | null): void {
    this.directory.set(directory);
    this.selectedPath.set(path);
  }

  clear(): void {
    this.selectedPath.set(null);
  }

  /** Select `path` and ask the Preview panel to navigate to it (F2-13's mini
   *  Explorer and the full-screen Explorer view both call this from their
   *  "Open in preview" action). */
  openInPreview(directory: string, path: string): void {
    this.select(directory, path);
    this.previewNonce += 1;
    this.previewRequest.set({ directory, path, nonce: this.previewNonce });
  }

  /** Select `path` and ask the full-screen Explorer to open it (F7-3): the
   *  right-drawer Explorer panel calls this, then navigates to `/explorer`. */
  openInExplorer(directory: string, path: string): void {
    this.select(directory, path);
    this.openNonce += 1;
    this.openRequest.set({ directory, path, nonce: this.openNonce });
  }

  /** Consume the pending "open in Explorer" request so a later, unrelated
   *  visit to `/explorer` doesn't replay it. */
  clearOpenRequest(): void {
    this.openRequest.set(null);
  }
}
