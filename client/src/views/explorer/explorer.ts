/**
 * Explorer view (M6 + Task 6): a lazy, gitignore-aware file tree + file
 * preview, both served through the engine API (`GET /fs/tree`, `GET /fs/file`)
 * - not the client's raw filesystem, so one permission model covers
 * everything. Task 6: right-click a `.html` file to preview it sandboxed.
 *
 * F7-3: also opens on demand with a file pre-selected and its content
 * loaded, when navigated to from the right-drawer Explorer panel (see the
 * constructor's `openRequest` effect and `ui/right-drawer/panels/explorer-panel.ts`).
 */

import { Component, OnInit, computed, effect, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { FsEntry } from '../../core/engine.dtos';
import { ExplorerSelectionStore } from '../../core/explorer-selection.store';
import { I18nService } from '../../i18n/i18n.service';
import { CodeHighlightService } from '../../ui/code-highlight/code-highlight.service';
import { HtmlPreviewComponent } from '../../ui/html-preview/html-preview';
import { isImagePath } from '../../ui/right-drawer/panels/preview-panel';
import { ShellStore } from '../../ui/shell/shell.store';
import { toMarkdownRows, type MarkdownRow } from './explorer-markdown';

interface FsNode {
  name: string;
  path: string;
  is_dir: boolean;
  depth: number;
  /** Directories only: drives the `▾`/`▸` disclosure triangle. */
  expanded: boolean;
}

interface DirState {
  entries: FsEntry[];
  loaded: boolean;
  expanded: boolean;
}

@Component({
  selector: 'app-explorer',
  imports: [FormsModule, HtmlPreviewComponent],
  templateUrl: './explorer.html',
  styleUrl: './explorer.css',
})
export class ExplorerView implements OnInit {
  private readonly engine = inject(EngineClient);
  private readonly route = inject(ActivatedRoute);
  private readonly i18n = inject(I18nService);
  private readonly selection = inject(ExplorerSelectionStore);
  private readonly shell = inject(ShellStore);
  private readonly codeHighlight = inject(CodeHighlightService);

  readonly t = this.i18n.t.bind(this.i18n);

  /** Last `ExplorerSelectionStore.openRequest()` nonce this view has already
   *  acted on (F7-3), so a stale/repeated request is never replayed. */
  private lastOpenNonce = 0;

  constructor() {
    // F7-3: the right-drawer Explorer panel points here via `openRequest`
    // (directory + path + nonce) and navigates to `/explorer`. Reading
    // `directory()` as a dependency means this waits, without polling, for
    // `ngOnInit` to set it from the matching query param before opening the
    // file - whichever of the two settles last re-triggers the effect.
    effect(() => {
      const req = this.selection.openRequest();
      const dir = this.directory();
      if (!req || req.nonce === this.lastOpenNonce || dir !== req.directory) {
        return;
      }
      this.lastOpenNonce = req.nonce;
      this.selection.clearOpenRequest();
      void this.openFile({ name: '', path: req.path, is_dir: false, depth: 0, expanded: false });
      // E2E R9: also expand the tree down to the file so it is highlighted in
      // context instead of every folder staying collapsed.
      void this.revealPath(req.path);
    });
  }

  readonly directory = signal<string | null>(null);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);
  readonly selectedPath = signal<string | null>(null);
  readonly fileContent = signal<string>('');
  readonly fileLoading = signal(false);
  /** Edit mode: textarea bound to `draftContent`. */
  readonly editing = signal(false);
  readonly draftContent = signal('');
  readonly saving = signal(false);
  /** Sandboxed HTML preview (right-click a .html file). */
  readonly htmlPreview = signal<string | null>(null);

  /** Directory state keyed by path ('' = root). */
  private readonly dirs = signal<Record<string, DirState>>({});
  private readonly version = signal(0);

  /** Flattened, visible tree rows. */
  readonly rows = computed<FsNode[]>(() => {
    this.version();
    const dirs = this.dirs();
    const out: FsNode[] = [];
    this.flatten(dirs, '', 0, out);
    return out;
  });

  readonly selectedName = computed(() => {
    const p = this.selectedPath();
    // Engine paths use the host separator, so split on both (Windows: `a\b`).
    return p ? p.split(/[\\/]/).pop() || p : null;
  });

  readonly isHtmlSelection = computed(() => {
    const p = this.selectedPath();
    return !!p && /\.html?$/i.test(p);
  });

  /** Markdown files get the light "reading view" instead of raw monospace. */
  readonly isMarkdownSelection = computed(() => {
    const p = this.selectedPath();
    return !!p && /\.(md|markdown)$/i.test(p);
  });

  /** Image preview (binary fetch, data-URL rendering). */
  readonly imageData = signal<string | null>(null);
  readonly imageMime = signal<string | null>(null);
  readonly isImageSelection = computed(() => this.imageData() !== null);
  readonly imageUrl = computed(() => {
    const data = this.imageData();
    const mime = this.imageMime();
    if (!data || !mime) {
      return '';
    }
    return `data:${mime};base64,${data}`;
  });

  readonly markdownRows = computed<MarkdownRow[]>(() =>
    this.isMarkdownSelection() ? toMarkdownRows(this.fileContent()) : [],
  );

  /**
   * F7-4: syntax-highlighted HTML for the plain-text content view (raw
   * files - markdown/html get their own dedicated views above). Language is
   * resolved from the selected file's extension; `CodeHighlightService`
   * already escapes and skips highlighting for files over its size guard, so
   * `html` is always safe to bind with `[innerHTML]` (no wrapping
   * `<pre>`/`<code>` - the template keeps its own for the existing
   * `.file-body.file-content` layout).
   */
  readonly highlightedFileContent = computed(() =>
    this.codeHighlight.highlight(this.fileContent(), { filename: this.selectedPath() }),
  );

  async ngOnInit(): Promise<void> {
    this.directory.set(
      this.route.snapshot.queryParamMap.get('directory') ?? this.engine.readLastDirectory(),
    );
    if (!this.engine.connected()) {
      try {
        await this.engine.connect();
      } catch (err) {
        this.error.set(this.describe(err));
        return;
      }
    }
    await this.loadRoot();
  }

  async loadRoot(): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      this.error.set(this.i18n.t('explorer.noDirectory'));
      return;
    }
    this.loading.set(true);
    this.error.set(null);
    try {
      const res = await this.engine.fsTree(dir);
      this.dirs.update((d) => ({
        ...d,
        '': { entries: res.entries, loaded: true, expanded: true },
      }));
      this.version.update((v) => v + 1);
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.loading.set(false);
    }
  }

  async toggle(node: FsNode): Promise<void> {
    if (!node.is_dir) {
      await this.openFile(node);
      return;
    }
    const state = this.dirs()[node.path];
    const dir = this.directory();
    if (!dir) {
      return;
    }
    if (state?.loaded) {
      this.dirs.update((d) => ({
        ...d,
        [node.path]: { ...state, expanded: !state.expanded },
      }));
      this.version.update((v) => v + 1);
      return;
    }
    // Lazy-load the directory's children from the engine.
    try {
      const res = await this.engine.fsTree(dir, node.path);
      this.dirs.update((d) => ({
        ...d,
        [node.path]: { entries: res.entries, loaded: true, expanded: true },
      }));
      this.version.update((v) => v + 1);
    } catch (err) {
      this.error.set(this.describe(err));
    }
  }

  /**
   * Expand every ancestor directory of `path` (lazy-loading the ones the
   * engine has not listed yet) so the file's row is visible in the tree.
   */
  async revealPath(path: string): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    const segments = path.split('/').filter((s) => s.length > 0);
    segments.pop(); // the file itself
    let ancestor = '';
    for (const segment of segments) {
      ancestor = ancestor ? `${ancestor}/${segment}` : segment;
      const state = this.dirs()[ancestor];
      if (state?.loaded) {
        if (!state.expanded) {
          this.dirs.update((d) => ({ ...d, [ancestor]: { ...state, expanded: true } }));
          this.version.update((v) => v + 1);
        }
        continue;
      }
      try {
        const res = await this.engine.fsTree(dir, ancestor);
        this.dirs.update((d) => ({
          ...d,
          [ancestor]: { entries: res.entries, loaded: true, expanded: true },
        }));
        this.version.update((v) => v + 1);
      } catch {
        return; // a missing/unlistable ancestor: leave the tree as it is
      }
    }
  }

  async openFile(node: FsNode): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    this.selectedPath.set(node.path);
    this.editing.set(false);
    this.htmlPreview.set(null);
    this.fileLoading.set(true);
    this.error.set(null);
    try {
      if (isImagePath(node.path)) {
        const res = await this.engine.fsFileBinary(dir, node.path);
        this.imageData.set(res.content);
        this.imageMime.set(res.media_type ?? 'image/png');
        this.fileContent.set('');
        this.draftContent.set('');
      } else {
        const res = await this.engine.fsFile(dir, node.path);
        this.fileContent.set(res.content);
        this.draftContent.set(res.content);
        this.imageData.set(null);
        this.imageMime.set(null);
      }
    } catch (err) {
      this.fileContent.set('');
      this.imageData.set(null);
      this.imageMime.set(null);
      this.error.set(this.describe(err));
    } finally {
      this.fileLoading.set(false);
    }
  }

  /** Right-click a row: `.html` files open a sandboxed preview. */
  onRowContextMenu(event: MouseEvent, node: FsNode): void {
    if (node.is_dir || !/\.html?$/i.test(node.path)) {
      return;
    }
    event.preventDefault();
    void this.openFile(node).then(() => {
      if (this.selectedPath() === node.path) {
        this.htmlPreview.set(this.fileContent());
      }
    });
  }

  /** Right-click the preview header: toggle the sandboxed HTML preview. */
  onPreviewContextMenu(event: MouseEvent): void {
    if (!this.isHtmlSelection() || this.fileLoading()) {
      return;
    }
    event.preventDefault();
    this.htmlPreview.set(this.htmlPreview() === null ? this.fileContent() : null);
  }

  /**
   * F6-11: "Open in preview" - point the right-drawer Preview panel (F6-10)
   * at the currently selected file and open it (the drawer itself only
   * renders on the Chat screen; from here this just arms the state, same as
   * every other `RightDrawerPanels` toggle - see `preview-panel.ts`).
   */
  openInPreview(): void {
    const dir = this.directory();
    const path = this.selectedPath();
    if (!dir || !path) {
      return;
    }
    this.selection.openInPreview(dir, path);
    if (!this.shell.rightDrawerPanels().preview) {
      this.shell.toggleRightDrawerPanel('preview');
    }
    if (!this.shell.rightDrawerOpen()) {
      this.shell.toggleRightDrawer();
    }
  }

  startEdit(): void {
    this.draftContent.set(this.fileContent());
    this.editing.set(true);
  }

  cancelEdit(): void {
    this.editing.set(false);
  }

  async saveEdit(): Promise<void> {
    const dir = this.directory();
    const path = this.selectedPath();
    if (!dir || !path || this.saving()) {
      return;
    }
    this.saving.set(true);
    this.error.set(null);
    try {
      await this.engine.fsFileWrite(dir, path, this.draftContent());
      this.fileContent.set(this.draftContent());
      this.editing.set(false);
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.saving.set(false);
    }
  }

  private flatten(dirs: Record<string, DirState>, path: string, depth: number, out: FsNode[]): void {
    const state = dirs[path];
    if (!state || !state.expanded) {
      return;
    }
    for (const child of state.entries) {
      out.push({
        name: child.name,
        path: child.path,
        is_dir: child.is_dir,
        depth,
        expanded: child.is_dir ? dirs[child.path]?.expanded === true : false,
      });
      if (child.is_dir && dirs[child.path]?.expanded) {
        this.flatten(dirs, child.path, depth + 1, out);
      }
    }
  }

  describe(err: unknown): string {
    return err instanceof Error ? err.message : String(err);
  }
}
