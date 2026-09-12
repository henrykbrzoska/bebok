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

@Injectable({ providedIn: 'root' })
export class ExplorerSelectionStore {
  /** Project-relative path of the selected file, or null when none. */
  readonly selectedPath = signal<string | null>(null);
  /** Directory the selection belongs to (absolute project root). */
  readonly directory = signal<string | null>(null);

  select(directory: string | null, path: string | null): void {
    this.directory.set(directory);
    this.selectedPath.set(path);
  }

  clear(): void {
    this.selectedPath.set(null);
  }
}
