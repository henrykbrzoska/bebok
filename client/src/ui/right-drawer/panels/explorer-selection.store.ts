/**
 * Shared "selected file" state for the Explorer (F2-13).
 *
 * The drawer's mini tree and the full-screen Explorer view must agree on which
 * file is selected. The handoff puts that signal in `core/`, but `core/*` is
 * read-only for this work package, so it lives here for now: it is a plain
 * root-provided store with no dependencies, so WP-TOOLS-UI can move the file
 * to `core/explorer-selection.store.ts` and re-point both imports without any
 * behaviour change.
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

@Injectable({ providedIn: 'root' })
export class ExplorerSelectionStore {
  /** Project-relative path of the selected file, or null when none. */
  readonly selectedPath = signal<string | null>(null);
  /** Directory the selection belongs to (absolute project root). */
  readonly directory = signal<string | null>(null);
  /** Latest explicit "open in Preview" request, or null before the first one. */
  readonly previewRequest = signal<PreviewRequest | null>(null);

  private previewNonce = 0;

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
}
