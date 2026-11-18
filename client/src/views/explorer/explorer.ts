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

import {
  Component,
  ElementRef,
  OnInit,
  computed,
  effect,
  inject,
  signal,
  untracked,
  viewChild,
} from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { FsCompleteItem, FsDefinitionHit, FsEntry } from '../../core/engine.dtos';
import { ToastStore } from '../../ui/toast/toast.store';
import { ExplorerSelectionStore } from '../../core/explorer-selection.store';
import { ProjectSessionsStore } from '../../ui/shell/project-sessions.store';
import { I18nService } from '../../i18n/i18n.service';
import { CodeHighlightService } from '../../ui/code-highlight/code-highlight.service';
import { FindStore } from '../../core/find.store';
import { FindBarComponent } from '../../ui/find-bar/find-bar.component';
import { HtmlPreviewComponent } from '../../ui/html-preview/html-preview';
import { PreviewPanel, isImagePath } from '../../ui/right-drawer/panels/preview-panel';
import { FindInFilesComponent, type FindInFilesTarget } from './find-in-files';
import { GotoDialogComponent, type GotoTab, type GotoTarget } from './goto-dialog';
import { ExplorerNavBarComponent } from './nav-bar';
import { ShellStore } from '../../ui/shell/shell.store';
import { ContextMenuDirective } from '../../ui/context-menu/context-menu.directive';
import { ContextMenuService } from '../../ui/context-menu/context-menu.service';
import {
  EXPLORER_EDITOR_PROVIDER,
  EXPLORER_FILE_PROVIDER,
  EXPLORER_PANE_PROVIDER,
  EXPLORER_TREE_PROVIDER,
  type ExplorerEditorPayload,
  type ExplorerFilePayload,
  type ExplorerTreePayload,
  type ExplorerPanePayload,
} from './explorer.menus';
import { lineOf, offsetOf, supportsGoto, symbolAt } from './explorer-editor';
import {
  caretBox,
  filterCompletions,
  isPhpPath,
  popupPlacement,
  prefixAt,
  utf8Offset,
  type CaretBox,
} from './explorer-complete';

/** Idle time after the last keystroke before asking the engine for suggestions. */
const COMPLETE_DEBOUNCE_MS = 150;
/** Items moved per PgUp / PgDn. */
const COMPLETE_PAGE = 8;
/** Two Shift taps within this window open "Go to" on the All tab. */
const DOUBLE_SHIFT_MS = 400;
/** Recently opened files kept per project (Go to File, empty query). */
const RECENT_MAX = 30;
const RECENT_KEY = 'bebok.explorer.recent:';
import { toMarkdownRows, type MarkdownRow } from './explorer-markdown';

interface FsNode {
  name: string;
  path: string;
  is_dir: boolean;
  depth: number;
  /** Directories only: drives the `▾`/`▸` disclosure triangle. */
  expanded: boolean;
}

/** A place in a file (go-to-definition target / back-navigation entry). */
interface EditorLocation {
  path: string;
  line?: number;
  column?: number;
}

interface DirState {
  entries: FsEntry[];
  loaded: boolean;
  expanded: boolean;
}

@Component({
  selector: 'app-explorer',
  imports: [
    FormsModule,
    HtmlPreviewComponent,
    FindBarComponent,
    ContextMenuDirective,
    FindInFilesComponent,
    GotoDialogComponent,
    ExplorerNavBarComponent,
    PreviewPanel,
  ],
  templateUrl: './explorer.html',
  styleUrl: './explorer.css',
  // Ctrl/Cmd+F opens the find bar for the open file instead of the
  // browser's built-in search (the bar paints matches via `wrapMark`).
  host: {
    '(document:keydown)': 'onDocumentKeydown($event)',
    '(document:keyup)': 'onDocumentKeyup($event)',
    '(window:blur)': 'gotoArmed.set(false)',
  },
})
export class ExplorerView implements OnInit {
  private readonly engine = inject(EngineClient);
  private readonly route = inject(ActivatedRoute);
  private readonly i18n = inject(I18nService);
  private readonly selection = inject(ExplorerSelectionStore);
  private readonly project = inject(ProjectSessionsStore);
  private readonly shell = inject(ShellStore);
  private readonly codeHighlight = inject(CodeHighlightService);
  private readonly menus = inject(ContextMenuService);
  private readonly toasts = inject(ToastStore);
  readonly find = inject(FindStore);

  readonly t = this.i18n.t.bind(this.i18n);

  /** Last `ExplorerSelectionStore.openRequest()` nonce this view has already
   *  acted on (F7-3), so a stale/repeated request is never replayed. */
  private lastOpenNonce = 0;

  constructor() {
    this.menus.register(EXPLORER_FILE_PROVIDER);
    this.menus.register(EXPLORER_PANE_PROVIDER);
    this.menus.register(EXPLORER_EDITOR_PROVIDER);
    this.menus.register(EXPLORER_TREE_PROVIDER);
    // While the preview column is open it follows the opened file (the
    // panel's own Pin still keeps a document in place).
    effect(() => {
      const path = this.selectedPath();
      const dir = this.directory();
      if (this.sidePreview() && path && dir) {
        untracked(() => this.selection.openInPreview(dir, path));
      }
    });
    // F7-3: the right-drawer Explorer panel points here via `openRequest`
    // (directory + path + nonce) and navigates to `/explorer`. Reading
    // `directory()` as a dependency means this waits, without polling, for
    // `ngOnInit` to set it from the matching query param before opening the
    // file - whichever of the two settles last re-triggers the effect.
    effect(
      () => {
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
      },
      { allowSignalWrites: true },
    );
    // A project switch while Explorer is open reloads the tree for the new
    // project (otherwise it keeps showing the previous project's files).
    effect(
      () => {
        const dir = this.project.directory();
        if (dir && dir !== this.directory()) {
          this.directory.set(dir);
          this.resetState();
          void this.loadRoot();
        }
      },
      { allowSignalWrites: true },
    );
  }

  readonly directory = signal<string | null>(null);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);
  readonly selectedPath = signal<string | null>(null);
  readonly fileContent = signal<string>('');
  readonly fileLoading = signal(false);
  /** Edit mode (the default for every text file): textarea bound to `draftContent`. */
  readonly editing = signal(false);
  readonly draftContent = signal('');
  readonly saving = signal(false);
  /** True once the draft differs from the last saved content. */
  readonly dirty = signal(false);
  /** Sandboxed HTML preview (right-click a .html file). */
  readonly htmlPreview = signal<string | null>(null);
  /** Inline tree input: create file/folder inside `inlineParent`, or rename `inlineTarget`. */
  readonly inlineMode = signal<'file' | 'folder' | 'rename' | null>(null);
  readonly inlineParent = signal<string>('');
  readonly inlineTarget = signal<string | null>(null);
  readonly inlineName = signal('');
  readonly inlineError = signal<string | null>(null);
  /** Delete confirmation for `pendingDelete` (recursive checkbox for dirs). */
  readonly pendingDelete = signal<string | null>(null);
  readonly pendingDeleteIsDir = signal(false);
  readonly deleteRecursive = signal(false);
  /** Markdown reading view toggled over the editor. */
  readonly mdPreview = signal(false);
  /** "Open in preview": the Preview panel shown as a column inside Explorer
   *  (the right drawer only exists on the Chat screen). */
  readonly sidePreview = signal(false);
  /** Ctrl/Cmd+Shift+F "Find in Files" dialog (prefill = editor selection). */
  readonly findInFilesOpen = signal(false);
  readonly findInFilesQuery = signal('');
  /** IntelliJ "Go to" popup: Ctrl/Cmd+Shift+N files, Ctrl/Cmd+N classes,
   *  Ctrl/Cmd+Alt+Shift+N symbols, Shift Shift everything. */
  readonly gotoOpen = signal(false);
  readonly gotoTab = signal<GotoTab>('files');
  readonly gotoQuery = signal('');
  /** Recently opened files of this project, most recent first. */
  readonly recentFiles = signal<string[]>([]);
  private lastShiftAt = 0;
  readonly navBar = viewChild(ExplorerNavBarComponent);
  /** Ctrl/Cmd is held over the editor: symbols become clickable. */
  readonly gotoArmed = signal(false);
  readonly gotoBusy = signal(false);
  /** Several definitions matched: the picker lists them. */
  readonly gotoChoices = signal<FsDefinitionHit[] | null>(null);
  readonly gotoSymbol = signal('');
  /** Completion popup: ranked suggestions for the caret, empty when closed. */
  readonly completeItems = signal<FsCompleteItem[]>([]);
  readonly completeIndex = signal(0);
  /** Popup position inside `.code-editor` (top or bottom, plus left). */
  readonly completePos = signal<{ left: number; top?: number; bottom?: number } | null>(null);
  private completeTimer: ReturnType<typeof setTimeout> | null = null;
  /** Bumped on every request/close so late responses are dropped. */
  private completeSeq = 0;
  /** Set while the popup itself writes into the textarea (accept). */
  private completeAccepting = false;
  /** Navigation blocked by unsaved edits: Save / Discard / Cancel. */
  readonly pendingNav = signal<EditorLocation | null>(null);
  /** Back stack for Alt+Left after a jump. */
  private readonly navHistory: EditorLocation[] = [];

  readonly editorArea = viewChild<ElementRef<HTMLTextAreaElement>>('editorArea');
  readonly editorLayer = viewChild<ElementRef<HTMLElement>>('editorLayer');
  readonly canGoto = computed(() => supportsGoto(this.selectedPath()));

  /** Directory state keyed by path ('' = root). */
  private readonly dirs = signal<Record<string, DirState>>({});
  private readonly version = signal(0);

  /** Drop the tree + file state of the previous project on a switch. */
  private resetState(): void {
    this.dirs.set({});
    this.version.update((v) => v + 1);
    this.selectedPath.set(null);
    this.fileContent.set('');
    this.draftContent.set('');
    this.editing.set(false);
    this.dirty.set(false);
    this.htmlPreview.set(null);
    this.mdPreview.set(false);
    this.gotoChoices.set(null);
    this.pendingNav.set(null);
    this.navHistory.length = 0;
    this.imageData.set(null);
    this.imageMime.set(null);
    this.error.set(null);
    this.cancelInline();
    this.pendingDelete.set(null);
  }

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
    this.isMarkdownSelection()
      ? toMarkdownRows(this.editing() ? this.draftContent() : this.fileContent())
      : [],
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

  /** Highlighted layer under the editor textarea. A trailing newline gets a
   *  space so the `<pre>` keeps the empty last line the textarea shows. */
  readonly highlightedDraft = computed(() => {
    const text = this.draftContent();
    const { html } = this.codeHighlight.highlight(text, { filename: this.selectedPath() });
    return text.endsWith('\n') || text === '' ? `${html} ` : html;
  });

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
    this.recentFiles.set(readRecent(dir));
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
      await this.requestOpen({ path: node.path });
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
    this.find.closeFind();
    this.editing.set(false);
    this.dirty.set(false);
    this.htmlPreview.set(null);
    this.mdPreview.set(false);
    this.gotoChoices.set(null);
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
        // Text files open straight in the editor (IDE behaviour).
        this.editing.set(true);
      }
      this.rememberRecent(node.path);
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

  /** Payload for the `explorer.file` provider (tree rows). */
  fileMenuData(node: FsNode): ExplorerFilePayload {
    return {
      path: node.path,
      name: node.name,
      isDir: node.is_dir,
      expanded: node.expanded,
      actions: {
        open: () => void this.toggle(node),
        toggleExpand: () => void this.toggle(node),
        openInPreview: () => {
          if (node.is_dir) {
            return;
          }
          void this.openFile(node).then(() => this.openInPreview());
        },
        startEdit: () => {
          if (node.is_dir) {
            return;
          }
          if (this.selectedPath() !== node.path) {
            void this.openFile(node).then(() => this.startEdit());
          } else {
            this.startEdit();
          }
        },
        copyPath: () => void this.menus.copyText(node.path),
        copyName: () => void this.menus.copyText(node.name),
        copyRelativePath: () => void this.menus.copyText(node.path),
        newFile: () => this.beginInline('file', node.is_dir ? node.path : parentOf(node.path)),
        newFolder: () => this.beginInline('folder', node.is_dir ? node.path : parentOf(node.path)),
        startRename: () => this.beginInline('rename', parentOf(node.path), node.path),
        requestDelete: () => this.requestDelete(node),
      },
    };
  }

  /** Payload for the `explorer.pane` provider (content header). */
  paneMenuData(): ExplorerPanePayload {
    return {
      path: this.selectedPath(),
      isHtml: this.isHtmlSelection(),
      actions: {
        openInPreview: () => this.openInPreview(),
        startEdit: () => this.startEdit(),
        copyPath: () => void this.menus.copyText(this.selectedPath() ?? ''),
        copyContent: () => void this.menus.copyText(this.fileContent()),
        copyRelativePath: () => void this.menus.copyText(this.selectedPath() ?? ''),
        revealInTree: () => {
          const p = this.selectedPath();
          if (p) {
            void this.revealPath(p);
          }
        },
      },
    };
  }

  /** Payload for the `explorer.tree` provider (empty tree background). */
  treeMenuData(): ExplorerTreePayload {
    return {
      actions: {
        newFile: () => this.beginInline('file', ''),
        newFolder: () => this.beginInline('folder', ''),
        refresh: () => void this.loadRoot(),
        expandAll: () => void this.expandAll(),
        collapseAll: () => this.collapseAll(),
        findInFiles: () => this.openFindInFiles(),
      },
    };
  }

  /** Expand every directory in the tree, lazy-loading levels as needed. */
  private async expandAll(): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    // Load the root first so there is something to expand.
    if (!this.dirs()['']?.loaded) {
      await this.loadRoot();
    }
    // Breadth-first: each freshly loaded level reveals subdirectories that
    // still need loading — a single pass over the initial keys misses them.
    const queue = Object.keys(this.dirs()).filter((p) => !this.dirs()[p]?.loaded);
    const seen = new Set<string>(queue);
    let guard = 0;
    while (queue.length > 0 && guard++ < 500) {
      const path = queue.shift()!;
      try {
        const res = await this.engine.fsTree(dir, path || undefined);
        this.dirs.update((d) => ({
          ...d,
          [path]: { entries: res.entries, loaded: true, expanded: true },
        }));
        for (const child of res.entries) {
          if (child.is_dir && !seen.has(child.path)) {
            seen.add(child.path);
            queue.push(child.path);
          }
        }
      } catch {
        /* leave unloadable directories collapsed */
      }
    }
    this.dirs.update((d) =>
      Object.fromEntries(Object.entries(d).map(([k, v]) => [k, { ...v, expanded: true }])),
    );
    this.version.update((v) => v + 1);
  }

  /** Collapse every directory back to the root level. */
  private collapseAll(): void {
    this.dirs.update((d) =>
      Object.fromEntries(
        Object.entries(d).map(([k, v]) => [k, { ...v, expanded: k === '' }]),
      ),
    );
    this.version.update((v) => v + 1);
  }

  /** Right-click the preview header: toggle the sandboxed HTML preview. */
  onPreviewContextMenu(event: MouseEvent): void {
    if (!this.isHtmlSelection() || this.fileLoading()) {
      return;
    }
    event.preventDefault();
    this.htmlPreview.set(this.htmlPreview() === null ? this.draftContent() : null);
  }

  /**
   * F6-11: "Open in preview" - point the Preview panel (F6-10) at the
   * selected file. The right drawer only renders on the Chat screen, so the
   * Explorer shows the same panel as its own column next to the editor (it
   * re-reads the file every few seconds, so a save shows up there). The
   * drawer state is armed too, so the file is waiting there in Chat.
   */
  openInPreview(): void {
    const dir = this.directory();
    const path = this.selectedPath();
    if (!dir || !path) {
      return;
    }
    if (!this.shell.rightDrawerPanels().preview) {
      this.shell.toggleRightDrawerPanel('preview');
    }
    this.selection.openInPreview(dir, path);
    if (!this.sidePreview()) {
      this.sidePreview.set(true);
    }
  }

  // --- IDE: Go to File / Class / Symbol (Ctrl/Cmd+Shift+N, Ctrl/Cmd+N) --------

  openGoto(tab: GotoTab): void {
    if (!this.directory()) {
      return;
    }
    if (!this.gotoOpen()) {
      const area = this.editorArea()?.nativeElement;
      let prefill = '';
      if (area && area.selectionEnd > area.selectionStart) {
        prefill = area.value.slice(area.selectionStart, area.selectionEnd);
      }
      this.gotoQuery.set(prefill.includes('\n') ? '' : prefill.trim().slice(0, 200));
      this.findInFilesOpen.set(false);
    }
    this.gotoTab.set(tab);
    this.gotoOpen.set(true);
  }

  /** A "Go to" result was picked: open it (Alt+Left comes back). */
  async onGotoOpen(target: GotoTarget): Promise<void> {
    this.gotoOpen.set(false);
    if (target.line !== undefined) {
      await this.jumpTo({
        path: target.path,
        line: target.line,
        column: target.column ?? 1,
        kind: '',
        snippet: '',
      });
      return;
    }
    const from = this.currentLocation();
    if (from && target.path !== from.path) {
      this.navHistory.push(from);
      if (this.navHistory.length > 50) {
        this.navHistory.shift();
      }
    }
    await this.requestOpen({ path: target.path });
  }

  /** Navigation bar picked a file. */
  async onNavOpen(path: string): Promise<void> {
    await this.onGotoOpen({ path });
  }

  /** Navigation bar segment double-clicked: show it in the tree. */
  onNavReveal(path: string): void {
    if (path) {
      void this.revealPath(this.selectedPath() === path ? path : `${path}/_`);
    }
  }

  private rememberRecent(path: string): void {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    const next = [path, ...this.recentFiles().filter((p) => p !== path)].slice(0, RECENT_MAX);
    this.recentFiles.set(next);
    try {
      localStorage.setItem(RECENT_KEY + dir, JSON.stringify(next));
    } catch {
      /* storage full / disabled: the list just stays in memory */
    }
  }

  // --- IDE: Find in Files (Ctrl/Cmd+Shift+F) -----------------------------------

  openFindInFiles(): void {
    if (!this.directory()) {
      return;
    }
    const area = this.editorArea()?.nativeElement;
    let prefill = '';
    if (area && area.selectionEnd > area.selectionStart) {
      prefill = area.value.slice(area.selectionStart, area.selectionEnd);
    } else if (area && this.editing()) {
      prefill = symbolAt(area.value, area.selectionStart ?? 0) ?? '';
    }
    this.findInFilesQuery.set(prefill.includes('\n') ? '' : prefill.slice(0, 200));
    this.findInFilesOpen.set(true);
  }

  /** A Find in Files hit was picked: jump there (Alt+Left comes back). */
  async onFindInFilesOpen(target: FindInFilesTarget): Promise<void> {
    this.findInFilesOpen.set(false);
    await this.jumpTo({ ...target, kind: '', snippet: '' });
  }

  /** Bring the editor up (it is the default view) and focus it. */
  startEdit(): void {
    if (this.isImageSelection()) {
      return;
    }
    if (!this.editing()) {
      this.draftContent.set(this.fileContent());
      this.dirty.set(false);
      this.editing.set(true);
    }
    this.htmlPreview.set(null);
    this.mdPreview.set(false);
    setTimeout(() => this.editorArea()?.nativeElement.focus());
  }

  /** Discard unsaved edits (the editor stays open). */
  cancelEdit(): void {
    this.draftContent.set(this.fileContent());
    this.dirty.set(false);
  }

  /** Called on every keystroke in the editor — marks the buffer dirty. */
  onDraftChange(value: string): void {
    this.draftContent.set(value);
    this.dirty.set(value !== this.fileContent());
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
      this.dirty.set(false);
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.saving.set(false);
    }
  }

  // --- IDE: navigation guard ---------------------------------------------------

  /**
   * Open `loc` (and jump to its line), asking first when the current file
   * has unsaved edits. Same-file targets never need the guard.
   */
  async requestOpen(loc: EditorLocation): Promise<void> {
    if (this.dirty() && loc.path !== this.selectedPath()) {
      this.pendingNav.set(loc);
      return;
    }
    await this.navigate(loc);
  }

  async saveAndContinue(): Promise<void> {
    const loc = this.pendingNav();
    await this.saveEdit();
    if (this.dirty()) {
      return; // save failed: keep the dialog's choice open via the error banner
    }
    this.pendingNav.set(null);
    if (loc) {
      await this.navigate(loc);
    }
  }

  async discardAndContinue(): Promise<void> {
    const loc = this.pendingNav();
    this.pendingNav.set(null);
    this.cancelEdit();
    if (loc) {
      await this.navigate(loc);
    }
  }

  cancelNav(): void {
    this.pendingNav.set(null);
  }

  private async navigate(loc: EditorLocation): Promise<void> {
    if (loc.path !== this.selectedPath() || this.fileLoading()) {
      await this.openFile({ name: '', path: loc.path, is_dir: false, depth: 0, expanded: false });
      void this.revealPath(loc.path);
    }
    if (loc.line !== undefined && this.selectedPath() === loc.path) {
      if (!this.editing()) {
        this.startEdit();
      }
      this.htmlPreview.set(null);
      this.mdPreview.set(false);
      // Wait for the textarea of a freshly opened file to render.
      setTimeout(() => this.revealLocation(loc.line ?? 1, loc.column ?? 1));
    }
  }

  /** Put the caret on `line`/`column` and scroll it to the upper third. */
  private revealLocation(line: number, column: number): void {
    const area = this.editorArea()?.nativeElement;
    if (!area) {
      return;
    }
    const offset = offsetOf(area.value, line, column);
    area.focus();
    area.setSelectionRange(offset, offset);
    const lh = parseFloat(getComputedStyle(area).lineHeight) || 20;
    area.scrollTop = Math.max(0, (line - 1) * lh - area.clientHeight / 3);
    this.syncLayer(area);
  }

  // --- IDE: go to definition (Ctrl/Cmd+click) --------------------------------

  /** Ctrl/Cmd+click in the editor: the click already moved the caret. */
  onEditorClick(event: MouseEvent, area: HTMLTextAreaElement): void {
    if (!(event.ctrlKey || event.metaKey) || !this.canGoto()) {
      return;
    }
    event.preventDefault();
    void this.gotoDefinitionAt(area.selectionStart ?? 0);
  }

  async gotoDefinitionAt(offset: number): Promise<void> {
    const dir = this.directory();
    const path = this.selectedPath();
    const text = this.draftContent();
    const symbol = symbolAt(text, offset);
    if (!dir || !path || !symbol || this.gotoBusy()) {
      return;
    }
    this.gotoBusy.set(true);
    this.gotoChoices.set(null);
    try {
      const res = await this.engine.fsDefinition(dir, path, symbol);
      const here = lineOf(text, offset);
      // Clicking the declaration itself should not "jump" onto itself.
      const hits = res.results.filter((h) => !(h.path === path && h.line === here));
      if (hits.length === 0) {
        this.toasts.show(this.i18n.t('explorer.gotoNotFound', { symbol }), { kind: 'info' });
      } else if (hits.length === 1) {
        await this.jumpTo(hits[0]);
      } else {
        this.gotoSymbol.set(symbol);
        this.gotoChoices.set(hits);
      }
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.gotoBusy.set(false);
    }
  }

  /** Jump to a definition, remembering where we came from (Alt+Left). */
  async jumpTo(hit: FsDefinitionHit): Promise<void> {
    this.gotoChoices.set(null);
    const from = this.currentLocation();
    if (from) {
      this.navHistory.push(from);
      if (this.navHistory.length > 50) {
        this.navHistory.shift();
      }
    }
    await this.requestOpen({ path: hit.path, line: hit.line, column: hit.column });
  }

  /** Alt+Left: back to the location before the last jump. */
  async goBack(): Promise<void> {
    const loc = this.navHistory.pop();
    if (loc) {
      await this.requestOpen(loc);
    }
  }

  private currentLocation(): EditorLocation | null {
    const path = this.selectedPath();
    if (!path) {
      return null;
    }
    const area = this.editorArea()?.nativeElement;
    if (!area) {
      return { path };
    }
    const offset = area.selectionStart ?? 0;
    const line = lineOf(area.value, offset);
    const lineStart = area.value.lastIndexOf('\n', offset - 1) + 1;
    return { path, line, column: offset - lineStart + 1 };
  }

  // --- IDE: editor plumbing ------------------------------------------------------

  /** Keep the highlighted layer aligned with the textarea's scroll. */
  syncLayer(area: HTMLTextAreaElement): void {
    const layer = this.editorLayer()?.nativeElement;
    if (layer) {
      layer.scrollTop = area.scrollTop;
      layer.scrollLeft = area.scrollLeft;
    }
  }

  /** The find bar scrolls the layer to a match: follow it with the textarea. */
  syncArea(layer: HTMLElement): void {
    const area = this.editorArea()?.nativeElement;
    if (area && (area.scrollTop !== layer.scrollTop || area.scrollLeft !== layer.scrollLeft)) {
      area.scrollTop = layer.scrollTop;
      area.scrollLeft = layer.scrollLeft;
    }
  }

  /** Tab inserts two spaces instead of leaving the editor; the completion
   *  popup, when open, owns the navigation keys and Enter/Tab. */
  onEditorKeydown(event: KeyboardEvent, area: HTMLTextAreaElement): void {
    if ((event.ctrlKey || event.metaKey) && event.key === ' ') {
      event.preventDefault();
      void this.requestCompletion(area);
      return;
    }
    if (this.completeItems().length > 0 && this.handleCompleteKey(event, area)) {
      return;
    }
    if (event.key === 'Tab' && !event.ctrlKey && !event.metaKey && !event.altKey) {
      event.preventDefault();
      this.insertText(area, '  ');
    }
  }

  // --- IDE: completion popup (POST /fs/complete) ----------------------------------

  /** Keys handled while the popup is open. Returns true when consumed. */
  private handleCompleteKey(event: KeyboardEvent, area: HTMLTextAreaElement): boolean {
    const n = this.completeItems().length;
    const move = (delta: number) => {
      const next = (this.completeIndex() + delta + n) % n;
      this.completeIndex.set(next);
      event.preventDefault();
      return true;
    };
    switch (event.key) {
      case 'ArrowDown':
        return move(1);
      case 'ArrowUp':
        return move(-1);
      case 'PageDown':
        return move(COMPLETE_PAGE);
      case 'PageUp':
        return move(-COMPLETE_PAGE);
      case 'Enter':
      case 'Tab': {
        if (event.ctrlKey || event.metaKey || event.altKey) {
          return false;
        }
        event.preventDefault();
        this.acceptCompletion(area, this.completeItems()[this.completeIndex()]);
        return true;
      }
      case 'Escape':
        event.preventDefault();
        this.closeCompletion();
        return true;
      default:
        return false;
    }
  }

  /** `input` on the textarea: narrow the open list at once, then ask the
   *  engine again after a short pause (typing an identifier). */
  onEditorInput(area: HTMLTextAreaElement): void {
    if (this.completeAccepting) {
      return;
    }
    if (!this.canComplete()) {
      this.closeCompletion();
      return;
    }
    const php = isPhpPath(this.selectedPath());
    const prefix = prefixAt(area.value, area.selectionStart ?? 0, php);
    if (!prefix || area.selectionStart !== area.selectionEnd) {
      this.closeCompletion();
      return;
    }
    if (this.completeItems().length > 0) {
      const kept = filterCompletions(this.completeItems(), prefix);
      this.completeItems.set(kept);
      this.completeIndex.set(0);
      if (kept.length === 0) {
        this.completePos.set(null);
      }
    }
    if (this.completeTimer !== null) {
      clearTimeout(this.completeTimer);
    }
    this.completeTimer = setTimeout(() => {
      this.completeTimer = null;
      void this.requestCompletion(area);
    }, COMPLETE_DEBOUNCE_MS);
  }

  /** Ask the engine for suggestions at the caret; ignores stale answers. */
  async requestCompletion(area: HTMLTextAreaElement): Promise<void> {
    const dir = this.directory();
    const path = this.selectedPath();
    if (!dir || !path || !this.canComplete()) {
      return;
    }
    const seq = ++this.completeSeq;
    const text = area.value;
    const offset = area.selectionStart ?? 0;
    try {
      const res = await this.engine.fsComplete(dir, {
        path,
        text,
        offset: utf8Offset(text, offset),
        limit: 50,
      });
      if (seq !== this.completeSeq || area.value !== text) {
        return; // superseded, closed, or the buffer changed meanwhile
      }
      if (res.items.length === 0) {
        this.closeCompletion();
        return;
      }
      const caret: CaretBox = caretBox(area, offset);
      const box = { width: area.clientWidth, height: area.clientHeight };
      this.completeItems.set(res.items);
      this.completeIndex.set(0);
      this.completePos.set(popupPlacement(caret, box));
    } catch {
      // Suggestions are best-effort: a failed request just shows nothing.
      this.closeCompletion();
    }
  }

  /** Replace the identifier before the caret with the chosen label (one undo step). */
  acceptCompletion(area: HTMLTextAreaElement, item: FsCompleteItem | undefined): void {
    if (!item) {
      return;
    }
    const caret = area.selectionStart ?? 0;
    const prefix = prefixAt(area.value, caret, isPhpPath(this.selectedPath()));
    this.closeCompletion();
    area.setSelectionRange(caret - prefix.length, caret);
    this.completeAccepting = true;
    try {
      this.insertText(area, item.label);
    } finally {
      this.completeAccepting = false;
    }
  }

  /** Mouse pick: mousedown keeps the textarea focused, so accept there. */
  onCompletePick(event: MouseEvent, index: number, area: HTMLTextAreaElement): void {
    event.preventDefault();
    this.acceptCompletion(area, this.completeItems()[index]);
  }

  closeCompletion(): void {
    this.completeSeq++;
    if (this.completeTimer !== null) {
      clearTimeout(this.completeTimer);
      this.completeTimer = null;
    }
    if (this.completeItems().length > 0) {
      this.completeItems.set([]);
    }
    this.completePos.set(null);
  }

  /** Suggestions only for files the engine can parse. */
  canComplete(): boolean {
    return this.canGoto();
  }

  /** Insert at the caret keeping the native undo stack when possible. */
  private insertText(area: HTMLTextAreaElement, text: string): void {
    area.focus();
    if (!document.execCommand?.('insertText', false, text)) {
      area.setRangeText(text, area.selectionStart, area.selectionEnd, 'end');
      this.onDraftChange(area.value);
    }
  }

  /** Payload for the `explorer.editor` provider (read at right-click time). */
  editorMenuData(area: HTMLTextAreaElement): ExplorerEditorPayload {
    const selected = () => area.value.slice(area.selectionStart, area.selectionEnd);
    return {
      state: () => ({
        hasSelection: area.selectionEnd > area.selectionStart,
        symbol: this.canGoto() ? symbolAt(area.value, area.selectionStart) : null,
        dirty: this.dirty(),
      }),
      actions: {
        gotoDefinition: () => void this.gotoDefinitionAt(area.selectionStart),
        cut: () => {
          const text = selected();
          if (!text) {
            return;
          }
          void this.menus.copyText(text);
          this.insertText(area, '');
        },
        copy: () => void this.menus.copyText(selected()),
        paste: () =>
          void this.menus.readText().then((text) => {
            if (text) {
              this.insertText(area, text);
            }
          }),
        selectAll: () => {
          area.focus();
          area.select();
        },
        save: () => void this.saveEdit(),
        find: () => this.find.openFind('explorer'),
      },
    };
  }

  // --- IDE: inline create / rename -----------------------------------------

  private beginInline(mode: 'file' | 'folder' | 'rename', parent: string, target?: string): void {
    this.inlineMode.set(mode);
    this.inlineParent.set(parent);
    this.inlineTarget.set(target ?? null);
    this.inlineName.set(mode === 'rename' && target ? baseName(target) : '');
    this.inlineError.set(null);
  }

  cancelInline(): void {
    this.inlineMode.set(null);
    this.inlineParent.set('');
    this.inlineTarget.set(null);
    this.inlineName.set('');
    this.inlineError.set(null);
  }

  /** Commit the inline input (Enter): create or rename, then refresh. */
  async commitInline(): Promise<void> {
    const dir = this.directory();
    const mode = this.inlineMode();
    const name = this.inlineName().trim();
    if (!dir || !mode) {
      return;
    }
    if (!name || name.includes('/') || name.includes('\\')) {
      this.inlineError.set(this.i18n.t('explorer.invalidName'));
      return;
    }
    this.inlineError.set(null);
    try {
      if (mode === 'rename') {
        const from = this.inlineTarget();
        if (!from) {
          return;
        }
        const to = this.inlineParent() ? `${this.inlineParent()}/${name}` : name;
        if (to === from) {
          this.cancelInline();
          return;
        }
        await this.engine.fsRename(dir, from, to);
        if (this.selectedPath() === from) {
          this.selectedPath.set(to);
        }
        await this.refreshDir(this.inlineParent());
      } else {
        const rel = this.inlineParent() ? `${this.inlineParent()}/${name}` : name;
        if (mode === 'file') {
          await this.engine.fsFileCreate(dir, rel);
        } else {
          await this.engine.fsDirCreate(dir, rel);
        }
        await this.refreshDir(this.inlineParent());
        // New folders open so the user can keep creating inside them.
        if (mode === 'folder') {
          await this.ensureExpanded(rel);
        }
      }
      this.cancelInline();
    } catch (err) {
      this.inlineError.set(this.describe(err));
    }
  }

  // --- IDE: delete with confirmation ----------------------------------------

  requestDelete(node: FsNode): void {
    this.pendingDelete.set(node.path);
    this.pendingDeleteIsDir.set(node.is_dir);
    this.deleteRecursive.set(false);
  }

  cancelDelete(): void {
    this.pendingDelete.set(null);
    this.deleteRecursive.set(false);
  }

  async confirmDelete(): Promise<void> {
    const dir = this.directory();
    const path = this.pendingDelete();
    if (!dir || !path) {
      return;
    }
    try {
      await this.engine.fsPathDelete(dir, path, this.deleteRecursive());
      if (this.selectedPath() === path) {
        this.selectedPath.set(null);
        this.fileContent.set('');
        this.draftContent.set('');
        this.editing.set(false);
        this.dirty.set(false);
      }
      await this.refreshDir(parentOf(path));
      this.cancelDelete();
    } catch (err) {
      this.error.set(this.describe(err));
      this.cancelDelete();
    }
  }

  /** Re-read one directory node from the engine (create/rename/delete). */
  private async refreshDir(path: string): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    try {
      const res = await this.engine.fsTree(dir, path || undefined);
      this.dirs.update((d) => ({
        ...d,
        [path]: { entries: res.entries, loaded: true, expanded: true },
      }));
      this.version.update((v) => v + 1);
    } catch (err) {
      this.error.set(this.describe(err));
    }
  }

  /** Expand `path` in the tree (used after creating a folder). */
  private async ensureExpanded(path: string): Promise<void> {
    const state = this.dirs()[path];
    if (state?.expanded) {
      return;
    }
    await this.refreshDir(path);
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

  /**
   * Ctrl/Cmd+F over an open plain-text file: open the find bar for this
   * file instead of the browser's built-in search. Markdown/image/HTML
   * previews and edit mode are out of scope (the bar paints the highlighted
   * `<code>` block only). Ctrl/Cmd+S saves while editing.
   */
  onDocumentKeydown(event: KeyboardEvent): void {
    if (event.key === 'Control' || event.key === 'Meta') {
      this.gotoArmed.set(this.canGoto());
    }
    if (this.handleGotoKeys(event)) {
      return;
    }
    if (event.key === 'Escape' && this.gotoChoices()) {
      this.gotoChoices.set(null);
      return;
    }
    if (event.altKey && event.key === 'ArrowLeft' && this.navHistory.length > 0) {
      event.preventDefault();
      void this.goBack();
      return;
    }
    const mod = event.ctrlKey || event.metaKey;
    if (this.findInFilesOpen() || this.gotoOpen()) {
      return; // the dialog owns the keyboard
    }
    if (event.altKey && !mod && event.key === 'Home') {
      event.preventDefault();
      this.navBar()?.openAt(-1);
      return;
    }
    if (mod && event.shiftKey && event.key.toLowerCase() === 'f') {
      event.preventDefault();
      this.openFindInFiles();
      return;
    }
    if (mod && event.key.toLowerCase() === 's' && this.editing()) {
      event.preventDefault();
      void this.saveEdit();
      return;
    }
    if (!mod || event.key.toLowerCase() !== 'f') {
      return;
    }
    if (!this.selectedPath() || this.fileLoading() || this.isImageSelection()) {
      return;
    }
    if (this.htmlPreview() !== null || this.mdPreview()) {
      return;
    }
    if (!this.editing() && (this.isHtmlSelection() || this.isMarkdownSelection())) {
      return;
    }
    event.preventDefault();
    this.find.openFind('explorer');
  }

  /**
   * "Go to" shortcuts (IntelliJ keymap). Returns true when consumed. Note:
   * desktop Chrome reserves Ctrl+N / Ctrl+Shift+N for itself, so in a plain
   * browser tab Shift Shift (or the nav bar button) is the way in; the Tauri
   * app gets every shortcut.
   */
  private handleGotoKeys(event: KeyboardEvent): boolean {
    const mod = event.ctrlKey || event.metaKey;
    if (event.key === 'Shift') {
      if (event.repeat || mod || event.altKey) {
        this.lastShiftAt = 0;
        return false;
      }
      const now = Date.now();
      if (now - this.lastShiftAt < DOUBLE_SHIFT_MS) {
        this.lastShiftAt = 0;
        if (!this.findInFilesOpen() && !this.pendingNav() && !this.pendingDelete()) {
          if (this.gotoOpen()) {
            this.gotoTab.set('all');
          } else {
            this.openGoto('all');
          }
          return true;
        }
        return false;
      }
      this.lastShiftAt = now;
      return false;
    }
    this.lastShiftAt = 0;
    if (!mod || event.code !== 'KeyN' || this.findInFilesOpen()) {
      return false;
    }
    event.preventDefault();
    if (event.shiftKey && event.altKey) {
      this.openGoto('symbols');
    } else if (event.shiftKey) {
      this.openGoto('files');
    } else {
      this.openGoto('classes');
    }
    return true;
  }

  onDocumentKeyup(event: KeyboardEvent): void {
    if (event.key === 'Control' || event.key === 'Meta') {
      this.gotoArmed.set(false);
    }
  }

  /** Element the find bar paints its matches into (the editor's highlighted
   *  layer in edit mode, the read-only `<code>` block otherwise). */
  findContainer(): HTMLElement | null {
    return this.editing()
      ? document.querySelector('.explorer .code-editor .editor-layer code')
      : document.querySelector('.explorer .file-body.file-content code');
  }
}

/** Recently opened files saved for `dir` (empty on missing / bad data). */
function readRecent(dir: string): string[] {
  try {
    const raw = localStorage.getItem(RECENT_KEY + dir);
    const list: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(list) ? list.filter((p): p is string => typeof p === 'string') : [];
  } catch {
    return [];
  }
}

/** Parent directory of an explorer path (`''` for top-level entries). */
function parentOf(path: string): string {
  const idx = path.lastIndexOf('/');
  return idx < 0 ? '' : path.slice(0, idx);
}

/** Last segment of an explorer path. */
function baseName(path: string): string {
  const idx = path.lastIndexOf('/');
  return idx < 0 ? path : path.slice(idx + 1);
}
