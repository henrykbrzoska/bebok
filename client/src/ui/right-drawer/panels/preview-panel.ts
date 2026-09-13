/**
 * Right-drawer "Preview" panel (F6-10, reworked in F9-15).
 *
 * Renders whatever file `ExplorerSelectionStore.previewRequest()` last pointed
 * at (set by Explorer's "Open in preview" action, by a `.md` link clicked in
 * chat text - see `explorer.ts` and `text-part.ts` - or by the panel's own
 * "Open file…" picker) through the shared `app-markdown-view` component, with
 * a small in-panel back/forward stack for relative links followed inside the
 * previewed document. File content is fetched with `EngineClient.fsFile` -
 * the same call `explorer.ts` already uses - so there is exactly one
 * file-read path, not two.
 *
 * F9-15 additions (the panel used to be "a file you cannot name, change or
 * refresh"):
 * - header line: file *name* in monospace with the full path as tooltip;
 * - toolbar: back/forward, "Open file…" (compact picker over the project
 *   tree, filtered as you type, previewable text types only), "Open in
 *   Explorer", "Refresh", "Pin";
 * - Pin keeps the current file: while pinned an incoming preview request is
 *   parked as `blockedRequest` and offered with an "Open anyway" action
 *   instead of replacing the document;
 * - a "modified on disk" indicator: while a file is shown and the panel is
 *   visible the content is re-read every `POLL_MS` (there is no fs-watch
 *   event; `/fs/file` has no mtime either) and compared with what was loaded;
 *   a difference shows the badge + a Reload action;
 * - the empty state explains how to fill the panel and carries an inline
 *   "Open file…" button.
 *
 * Read-only by design: editing stays in the full-screen Explorer.
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  computed,
  effect,
  inject,
  signal,
  untracked,
  viewChild,
} from '@angular/core';
import { Router } from '@angular/router';

import { EngineClient } from '../../../core/engine-client.service';
import { ExplorerSelectionStore, PreviewRequest } from '../../../core/explorer-selection.store';
import { I18nService } from '../../../i18n/i18n.service';
import { MarkdownViewComponent } from '../../markdown-view/markdown-view';

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** Extensions the panel can show (text via `/fs/file`; images are binary and
 *  not readable through that endpoint, so they are not offered). */
export const PREVIEWABLE_EXTENSIONS: readonly string[] = [
  'md',
  'markdown',
  'txt',
  'json',
  'jsonc',
  'yaml',
  'yml',
  'toml',
  'csv',
  'xml',
  'html',
  'htm',
  'rst',
  'log',
];

export function isPreviewablePath(path: string): boolean {
  const dot = path.lastIndexOf('.');
  if (dot < 0) {
    return false;
  }
  return PREVIEWABLE_EXTENSIONS.includes(path.slice(dot + 1).toLowerCase());
}

/** Directories never worth walking for the picker even when not gitignored. */
const SKIP_DIRS: ReadonlySet<string> = new Set([
  '.git',
  'node_modules',
  'dist',
  'target',
  '.angular',
  '.bebok',
  '.next',
  '.cache',
]);

/** Picker walk limits: the list is a quick chooser, not a full index. */
const PICKER_MAX_FILES = 1500;
const PICKER_MAX_DEPTH = 8;
const PICKER_MAX_RESULTS = 40;
/** Re-walk the tree when the picker is opened again after this long. */
const PICKER_CACHE_MS = 30_000;
/** "Modified on disk" poll period (visible panel with a loaded file only). */
export const POLL_MS = 5_000;

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
  private readonly router = inject(Router);
  private readonly destroyRef = inject(DestroyRef);
  private readonly host = inject<ElementRef<HTMLElement>>(ElementRef);

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

  /** F9-15: pin - keep the current document when links would replace it. */
  readonly pinned = signal(false);
  /** F9-15: the request a pin refused (offered as "Open anyway"). */
  readonly blockedRequest = signal<PreviewRequest | null>(null);
  /** F9-15: the file on disk differs from what is shown. */
  readonly modified = signal(false);

  /** F9-15: "Open file…" picker state. */
  readonly pickerOpen = signal(false);
  readonly pickerQuery = signal('');
  readonly pickerLoading = signal(false);
  readonly pickerError = signal<string | null>(null);
  readonly pickerIndex = signal(0);
  private readonly pickerFiles = signal<string[]>([]);
  private pickerLoadedAt = 0;
  private pickerLoadedFor: string | null = null;
  private readonly pickerInput = viewChild<ElementRef<HTMLInputElement>>('pickerInput');

  readonly currentPath = computed<string | null>(() => this.history()[this.historyIndex()] ?? null);
  /** Last path segment - the header shows this, the tooltip the full path. */
  readonly currentName = computed(() => {
    const p = this.currentPath();
    if (!p) {
      return '';
    }
    const idx = Math.max(p.lastIndexOf('/'), p.lastIndexOf('\\'));
    return idx >= 0 ? p.slice(idx + 1) : p;
  });
  readonly canBack = computed(() => this.historyIndex() > 0);
  readonly canForward = computed(() => this.historyIndex() < this.history().length - 1);
  readonly isMarkdown = computed(() => {
    const p = this.currentPath();
    return !!p && /\.(md|markdown)$/i.test(p);
  });
  /** Directory the picker/explorer actions apply to: the previewed file's,
   *  else the project the chat is bound to. */
  readonly directory = computed(
    () => this.activeDirectory() ?? this.selection.directory() ?? this.engine.readLastDirectory(),
  );

  readonly pickerResults = computed<string[]>(() => {
    const q = this.pickerQuery().trim().toLowerCase();
    const files = this.pickerFiles();
    const list = q ? files.filter((f) => fuzzyMatch(f.toLowerCase(), q)) : files;
    return list.slice(0, PICKER_MAX_RESULTS);
  });

  private lastNonce = 0;
  private pollTimer: number | undefined;

  constructor() {
    effect(() => {
      const req = this.selection.previewRequest();
      if (!req || req.nonce === this.lastNonce) {
        return;
      }
      this.lastNonce = req.nonce;
      untracked(() => {
        if (this.pinned() && this.currentPath() && req.path !== this.currentPath()) {
          this.blockedRequest.set(req);
          return;
        }
        this.openRequest(req);
      });
    });
    this.pollTimer = window.setInterval(() => void this.checkModified(), POLL_MS);
    this.destroyRef.onDestroy(() => window.clearInterval(this.pollTimer));
  }

  /** Follow a relative link resolved by `app-markdown-view`. */
  navigate(path: string): Promise<void> {
    const dir = this.activeDirectory();
    if (!dir) {
      return Promise.resolve();
    }
    if (this.pinned() && this.currentPath() && path !== this.currentPath()) {
      this.blockedRequest.set({ directory: dir, path, nonce: -1 });
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

  /** F9-15: re-read the current file (also clears the modified badge). */
  refresh(): Promise<void> {
    return this.reload();
  }

  /** F9-15: keep/unkeep the current document. */
  togglePin(): void {
    this.pinned.update((p) => !p);
    if (!this.pinned()) {
      this.blockedRequest.set(null);
    }
  }

  /** F9-15: the pin refused a request - open it after all (and unpin). */
  openBlocked(): void {
    const req = this.blockedRequest();
    if (!req) {
      return;
    }
    this.pinned.set(false);
    this.blockedRequest.set(null);
    if (req.nonce === -1) {
      // A relative link inside the document: keep it in the history stack.
      void this.navigate(req.path);
      return;
    }
    this.openRequest(req);
  }

  dismissBlocked(): void {
    this.blockedRequest.set(null);
  }

  /** F9-15: jump to the file in the full-screen Explorer (editing lives there). */
  async openInExplorer(): Promise<void> {
    const dir = this.activeDirectory();
    const path = this.currentPath();
    if (!dir || !path) {
      return;
    }
    this.selection.openInExplorer(dir, path);
    await this.router.navigate(['/explorer'], { queryParams: { directory: dir } });
  }

  // --- F9-15: "Open file…" picker ------------------------------------------

  openPicker(): void {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    this.pickerOpen.set(true);
    this.pickerQuery.set('');
    this.pickerIndex.set(0);
    this.pickerError.set(null);
    const stale = Date.now() - this.pickerLoadedAt > PICKER_CACHE_MS;
    if (this.pickerLoadedFor !== dir || stale) {
      void this.loadPickerFiles(dir);
    }
    // Focus after the input renders (zoneless: the view updates on the next tick).
    window.setTimeout(() => this.pickerInput()?.nativeElement.focus(), 0);
  }

  closePicker(): void {
    this.pickerOpen.set(false);
  }

  onPickerQuery(value: string): void {
    this.pickerQuery.set(value);
    this.pickerIndex.set(0);
  }

  onPickerKey(event: KeyboardEvent): void {
    const results = this.pickerResults();
    switch (event.key) {
      case 'ArrowDown':
        event.preventDefault();
        this.pickerIndex.set(Math.min(results.length - 1, this.pickerIndex() + 1));
        break;
      case 'ArrowUp':
        event.preventDefault();
        this.pickerIndex.set(Math.max(0, this.pickerIndex() - 1));
        break;
      case 'Enter': {
        event.preventDefault();
        const pick = results[this.pickerIndex()];
        if (pick) {
          this.pickFile(pick);
        }
        break;
      }
      case 'Escape':
        event.preventDefault();
        this.closePicker();
        break;
      default:
        break;
    }
  }

  pickFile(path: string): void {
    const dir = this.directory();
    this.closePicker();
    if (!dir) {
      return;
    }
    // A deliberate pick always wins over the pin.
    this.pinned.set(false);
    this.blockedRequest.set(null);
    // Goes through the shared request path so the drawer reveal + history
    // reset behave exactly like an Explorer "Open in preview" (F9-3).
    this.selection.openInPreview(dir, path);
  }

  /** Visible for the spec: the walk that feeds the picker. */
  async loadPickerFiles(dir: string): Promise<void> {
    this.pickerLoading.set(true);
    this.pickerError.set(null);
    const files: string[] = [];
    try {
      // Breadth-first over `/fs/tree` (lazy, gitignore-aware on the engine
      // side) with hard caps so a huge repo cannot stall the panel.
      let frontier: { path: string; depth: number }[] = [{ path: '', depth: 0 }];
      while (frontier.length > 0 && files.length < PICKER_MAX_FILES) {
        const next: { path: string; depth: number }[] = [];
        for (const node of frontier) {
          if (files.length >= PICKER_MAX_FILES) {
            break;
          }
          const res = await this.engine.fsTree(dir, node.path || undefined);
          if (this.directory() !== dir) {
            return; // project switched mid-walk
          }
          for (const entry of res.entries) {
            if (entry.is_dir) {
              if (!SKIP_DIRS.has(entry.name) && node.depth + 1 <= PICKER_MAX_DEPTH) {
                next.push({ path: entry.path, depth: node.depth + 1 });
              }
            } else if (isPreviewablePath(entry.path)) {
              files.push(entry.path);
            }
          }
        }
        frontier = next;
      }
      files.sort(byDepthThenName);
      this.pickerFiles.set(files);
      this.pickerLoadedFor = dir;
      this.pickerLoadedAt = Date.now();
    } catch (err) {
      this.pickerError.set(describe(err));
    } finally {
      this.pickerLoading.set(false);
    }
  }

  // --- internals --------------------------------------------------------------

  private openRequest(req: PreviewRequest): void {
    this.blockedRequest.set(null);
    this.activeDirectory.set(req.directory);
    this.history.set([req.path]);
    this.historyIndex.set(0);
    void this.load(req.directory, req.path);
  }

  private reload(): Promise<void> {
    const dir = this.activeDirectory();
    const path = this.currentPath();
    return dir && path ? this.load(dir, path) : Promise.resolve();
  }

  private async load(directory: string, path: string): Promise<void> {
    this.loading.set(true);
    this.error.set(null);
    this.modified.set(false);
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

  /** F9-15: compare the file on disk with what is shown (the poll tick;
   *  public so the spec can drive it without a fake clock). `force` skips
   *  the visibility gate. */
  async checkModified(force = false): Promise<void> {
    const dir = this.activeDirectory();
    const path = this.currentPath();
    if (!dir || !path || this.loading() || this.error() || this.modified()) {
      return;
    }
    if (!force && (document.hidden || this.host.nativeElement.getClientRects().length === 0)) {
      return; // collapsed panel / hidden drawer / background tab: do not poll
    }
    try {
      const res = await this.engine.fsFile(dir, path);
      if (this.currentPath() === path && res.content !== this.content()) {
        this.modified.set(true);
      }
    } catch {
      /* transient read error: the next tick retries */
    }
  }
}

/** Every query character appears in order (subsequence match), or the query
 *  is a plain substring - cheap, good enough for a few hundred paths. */
function fuzzyMatch(haystack: string, query: string): boolean {
  if (haystack.includes(query)) {
    return true;
  }
  let i = 0;
  for (const ch of haystack) {
    if (ch === query[i]) {
      i += 1;
      if (i === query.length) {
        return true;
      }
    }
  }
  return false;
}

function byDepthThenName(a: string, b: string): number {
  const da = a.split('/').length;
  const db = b.split('/').length;
  return da - db || a.localeCompare(b);
}
