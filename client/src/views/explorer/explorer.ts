/**
 * Explorer view (M6 + Task 6): a lazy, gitignore-aware file tree + file
 * preview, both served through the engine API (`GET /fs/tree`, `GET /fs/file`)
 * - not the client's raw filesystem, so one permission model covers
 * everything. Task 6: right-click a `.html` file to preview it sandboxed.
 */

import { Component, OnInit, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { FsEntry } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';
import { HtmlPreviewComponent } from '../../ui/html-preview/html-preview';
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

  readonly t = this.i18n.t.bind(this.i18n);

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
    return p ? p.split('/').pop() ?? p : null;
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

  readonly markdownRows = computed<MarkdownRow[]>(() =>
    this.isMarkdownSelection() ? toMarkdownRows(this.fileContent()) : [],
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
      const res = await this.engine.fsFile(dir, node.path);
      this.fileContent.set(res.content);
      this.draftContent.set(res.content);
    } catch (err) {
      this.fileContent.set('');
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
