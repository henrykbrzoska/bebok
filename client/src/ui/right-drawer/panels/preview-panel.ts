/**
 * Right-drawer "Preview" panel (F6-10).
 *
 * Renders whatever file `ExplorerSelectionStore.previewRequest()` last pointed
 * at (set by Explorer's "Open in preview" action, or by a `.md` link clicked
 * in chat text - see `explorer.ts` and `text-part.ts`) through the shared
 * `app-markdown-view` component, with a small in-panel back/forward stack for
 * relative links followed inside the previewed document. File content is
 * fetched with `EngineClient.fsFile` - the same call `explorer.ts` already
 * uses - so there is exactly one file-read path, not two.
 */

import { ChangeDetectionStrategy, Component, computed, effect, inject, signal } from '@angular/core';

import { EngineClient } from '../../../core/engine-client.service';
import { I18nService } from '../../../i18n/i18n.service';
import { MarkdownViewComponent } from '../../markdown-view/markdown-view';
import { ExplorerSelectionStore } from './explorer-selection.store';

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

@Component({
  selector: 'app-preview-panel',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MarkdownViewComponent],
  templateUrl: './preview-panel.html',
  styleUrl: './preview-panel.css',
})
export class PreviewPanel {
  private readonly i18n = inject(I18nService);
  private readonly engine = inject(EngineClient);
  private readonly selection = inject(ExplorerSelectionStore);

  readonly t = this.i18n.t.bind(this.i18n);

  /** Directory captured at the last "open in preview" request (not the mini
   *  Explorer's live selection, so idle tree browsing cannot yank the panel
   *  to a different file mid-read). */
  private readonly activeDirectory = signal<string | null>(null);
  /** In-panel visited-path stack (not browser history, not a route - F6-10). */
  private readonly history = signal<string[]>([]);
  private readonly historyIndex = signal(-1);

  readonly content = signal('');
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  readonly currentPath = computed<string | null>(() => this.history()[this.historyIndex()] ?? null);
  readonly canBack = computed(() => this.historyIndex() > 0);
  readonly canForward = computed(() => this.historyIndex() < this.history().length - 1);
  readonly isMarkdown = computed(() => {
    const p = this.currentPath();
    return !!p && /\.(md|markdown)$/i.test(p);
  });

  private lastNonce = 0;

  constructor() {
    effect(() => {
      const req = this.selection.previewRequest();
      if (!req || req.nonce === this.lastNonce) {
        return;
      }
      this.lastNonce = req.nonce;
      this.activeDirectory.set(req.directory);
      this.history.set([req.path]);
      this.historyIndex.set(0);
      void this.load(req.directory, req.path);
    });
  }

  /** Follow a relative link resolved by `app-markdown-view`. */
  navigate(path: string): Promise<void> {
    const dir = this.activeDirectory();
    if (!dir) {
      return Promise.resolve();
    }
    const trimmed = this.history().slice(0, this.historyIndex() + 1);
    trimmed.push(path);
    this.history.set(trimmed);
    this.historyIndex.set(trimmed.length - 1);
    return this.load(dir, path);
  }

  back(): Promise<void> {
    if (!this.canBack()) {
      return Promise.resolve();
    }
    this.historyIndex.update((i) => i - 1);
    return this.reload();
  }

  forward(): Promise<void> {
    if (!this.canForward()) {
      return Promise.resolve();
    }
    this.historyIndex.update((i) => i + 1);
    return this.reload();
  }

  private reload(): Promise<void> {
    const dir = this.activeDirectory();
    const path = this.currentPath();
    return dir && path ? this.load(dir, path) : Promise.resolve();
  }

  private async load(directory: string, path: string): Promise<void> {
    this.loading.set(true);
    this.error.set(null);
    try {
      const res = await this.engine.fsFile(directory, path);
      this.content.set(res.content);
    } catch (err) {
      // Graceful degrade (F6-10 acceptance): a missing or unreadable target
      // shows a clear message instead of crashing the panel.
      this.content.set('');
      this.error.set(describe(err));
    } finally {
      this.loading.set(false);
    }
  }
}
