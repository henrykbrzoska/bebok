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
import { FsCompleteItem, FsDefinitionHit, FsEntry, FileDiffHunk, GitLogEntry } from '../../core/engine.dtos';
import { ToastStore } from '../../ui/toast/toast.store';
import { ExplorerSelectionStore } from '../../core/explorer-selection.store';
import { ProjectSessionsStore } from '../../ui/shell/project-sessions.store';
import { I18nService } from '../../i18n/i18n.service';
import { CodeHighlightService } from '../../ui/code-highlight/code-highlight.service';
import { FindStore } from '../../core/find.store';
import { FindBarComponent } from '../../ui/find-bar/find-bar.component';
import { HtmlPreviewComponent } from '../../ui/html-preview/html-preview';
import { DiffViewComponent } from '../../ui/diff-view/diff-view';
import { PreviewPanel, isImagePath } from '../../ui/right-drawer/panels/preview-panel';
import { FindInFilesComponent, type FindInFilesTarget } from './find-in-files';
import { GotoDialogComponent, type GotoTab, type GotoTarget } from './goto-dialog';
import { ExplorerNavBarComponent } from './nav-bar';
import { ShellStore } from '../../ui/shell/shell.store';
import { EventsStore } from '../../core/events.store';
import { ProjectsStore } from '../../core/projects.store';
import { emptyChanges, hunksToLineChanges, type GitLineChanges } from './explorer-changes';
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
import { parseFileDiff, reverseApply, unifiedDiff } from './explorer-git';
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
/** Open editor tabs kept per project as `{tabs, active}` JSON. */
const TABS_KEY = 'bebok.explorer.tabs:';
const TABS_MAX = 20;
const TABS_SAVE_DELAY_MS = 300;
/** Commits fetched per `git log` page in the History column. */
const HISTORY_PAGE = 30;
/** Commits inspected when deriving blame (newest first). */
const BLAME_COMMITS = 25;
/** Commits reverse-applied to rebuild the pre-image for "compare with working". */
const COMPARE_CHAIN_MAX = 10;

/** One line of the blame column (`hash` empty = not attributed by the history). */
export interface BlameLine {
  line: number;
  hash: string;
  short: string;
  author: string;
  subject: string;
  ts: number;
}

/** One open editor tab. `scrollTop` / `cursor` are reserved for restoring the view. */
export interface OpenTab {
  path: string;
  scrollTop?: number;
  cursor?: { line: number; column: number };
}
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
    DiffViewComponent,
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
  private readonly projects = inject(ProjectsStore);
  private readonly events = inject(EventsStore);
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
    // Minimap: re-render when content changes
    effect(() => {
      const text = this.editing() ? this.draftContent() : this.fileContent();
      // track dependency
      void text;
      requestAnimationFrame(() => this.renderMinimap());
    });
  }

  readonly directory = signal<string | null>(null);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);
  /** Open editor tabs, in tab-strip order. */
  readonly openTabs = signal<OpenTab[]>([]);
  /** Path shown in the editor (source of truth; `selectedPath` derives from it). */
  readonly activePath = signal<string | null>(null);
  readonly selectedPath = computed(() => this.activePath());
  /** Saved text + draft of tabs that are not on screen, keyed by path. */
  private readonly buffers = new Map<string, { saved: string; draft: string }>();
  /** Tab waiting on the unsaved-changes dialog before it closes. */
  readonly pendingClose = signal<string | null>(null);
  private tabsSave: { key: string; value: string } | null = null;
  private tabsSaveTimer: ReturnType<typeof setTimeout> | null = null;
  /** Directory whose saved tabs were already restored (cleared on project switch). */
  private restoredDir: string | null = null;
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
  /** Locations undone by Alt+Left (Alt+Right replays them). */
  private readonly navForward: EditorLocation[] = [];

  // Git panes above the editor: Code | History | Blame.
  readonly historyOpen = signal(false);
  readonly blameOpen = signal(false);
  /** Commits touching the open file, newest first (paged). */
  readonly fileHistory = signal<GitLogEntry[]>([]);
  readonly historyLoading = signal(false);
  readonly historyHasMore = signal(false);
  /** Per-line attribution of the open file, index 0 = line 1. */
  readonly fileBlame = signal<BlameLine[]>([]);
  readonly blameLoading = signal(false);
  /** History: the commit whose diff is on screen. */
  readonly selectedCommit = signal<GitLogEntry | null>(null);
  readonly commitDiff = signal('');
  readonly commitLoading = signal(false);
  /** True while the pane shows the working-copy diff instead of the commit's. */
  readonly comparing = signal(false);
  /** Diff of the working copy against the selected commit (client-side). */
  readonly workingDiff = signal('');

  readonly editorArea = viewChild<ElementRef<HTMLTextAreaElement>>('editorArea');
  readonly editorLayer = viewChild<ElementRef<HTMLElement>>('editorLayer');
  readonly canGoto = computed(() => supportsGoto(this.selectedPath()));

  /** Directory state keyed by path ('' = root). */
  private readonly dirs = signal<Record<string, DirState>>({});
  private readonly version = signal(0);

  /** Close the History / Blame panes and drop their data. */
  private resetGitPanes(): void {
    this.historyOpen.set(false);
    this.blameOpen.set(false);
    this.fileHistory.set([]);
    this.historyHasMore.set(false);
    this.fileBlame.set([]);
    this.selectedCommit.set(null);
    this.commitDiff.set('');
    this.clearCompare();
  }

  /** Drop the tree + file state of the previous project on a switch. */
  private resetState(): void {
    this.flushTabs();
    this.restoredDir = null;
    this.openTabs.set([]);
    this.buffers.clear();
    this.pendingClose.set(null);
    this.dirs.set({});
    this.version.update((v) => v + 1);
    this.activePath.set(null);
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
    this.resetGitPanes();
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

  readonly draftLineNumbers = computed(() => lineNumbersFor(this.draftContent()));
  readonly fileLineNumbers = computed(() => lineNumbersFor(this.fileContent()));
  readonly gutterEl = viewChild<ElementRef<HTMLElement>>('gutterEl');

  /** Git gutter: per-line change sets for the open file (empty = clean). */
  readonly gitHunks = signal<GitLineChanges>(emptyChanges());
  /** Raw unified=0 hunks behind `gitHunks` (line -> hunk mapping for the dialog). */
  readonly gitRawHunks = signal<FileDiffHunk[]>([]);
  /** Hunk preview dialog state (null = closed). */
  readonly hunkDialog = signal<{
    line: number;
    kind: 'added' | 'removed' | 'modified';
    oldLines: string[];
    newLines: string[];
    pureAdd: boolean;
    loading: boolean;
  } | null>(null);
  /** Gutter on/off, persisted in localStorage. */
  readonly gutterOn = signal<boolean>(this.readGutterPref());
  private gitUnsub: (() => void) | null = null;
  private gitSeq = 0;

  private readGutterPref(): boolean {
    try {
      return localStorage.getItem('bebok.explorer.gutter') !== 'off';
    } catch {
      return true;
    }
  }

  toggleGutter(): void {
    const next = !this.gutterOn();
    this.gutterOn.set(next);
    try {
      localStorage.setItem('bebok.explorer.gutter', next ? 'on' : 'off');
    } catch { /* ignore */ }
  }

  /** CSS class for a 1-based gutter line ('' when the gutter is off). */
  gutterClass(line: number): string {
    if (!this.gutterOn()) return '';
    const ch = this.gitHunks();
    if (ch.added.has(line)) return 'gutter-add';
    if (ch.removed.has(line)) return 'gutter-del';
    if (ch.modified.has(line)) return 'gutter-mod';
    return '';
  }

  /** Project id for the current directory (worktree dirs map to their root). */
  private matchProjectId(directory: string | null): string | null {
    if (!directory) return null;
    const direct = this.projects.findByPath(directory);
    if (direct) return direct.id;
    const normalized = directory.replace(/\\/g, '/');
    const marker = '/.bebok/worktrees/';
    const idx = normalized.indexOf(marker);
    if (idx > 0) {
      return this.projects.findByPath(normalized.slice(0, idx))?.id ?? null;
    }
    return null;
  }

  /** Reload per-line git changes for the open file (no-op when gutter off). */
  async reloadChangedLines(): Promise<void> {
    const seq = ++this.gitSeq;
    if (!this.gutterOn()) {
      this.gitHunks.set(emptyChanges());
      this.gitRawHunks.set([]);
      return;
    }
    const dir = this.directory();
    const path = this.selectedPath();
    if (!dir || !path) {
      this.gitHunks.set(emptyChanges());
      this.gitRawHunks.set([]);
      return;
    }
    const id = this.matchProjectId(dir);
    if (!id) {
      this.gitHunks.set(emptyChanges());
      this.gitRawHunks.set([]);
      return;
    }
    try {
      const hunks = await this.engine.gitFileDiff(id, path);
      if (seq !== this.gitSeq) return;
      this.gitRawHunks.set(hunks);
      this.gitHunks.set(hunksToLineChanges(hunks));
    } catch {
      if (seq === this.gitSeq) {
        this.gitRawHunks.set([]);
        this.gitHunks.set(emptyChanges());
      }
    }
  }

  /** Hunk covering a 1-based new-file line (null when the line is clean). */
  hunkForLine(line: number): FileDiffHunk | null {
    for (const h of this.gitRawHunks()) {
      const ns = h.newStart ?? h.new_start ?? 0;
      const nl = h.newLines ?? h.new_lines ?? 0;
      if (nl <= 0) {
        if (line === Math.max(1, ns)) return h;
      } else if (line >= ns && line < ns + nl) {
        return h;
      }
    }
    return null;
  }

  /** Click on a gutter line: open the "what was before" dialog. */
  async openHunkDialog(line: number): Promise<void> {
    if (!this.gutterOn()) return;
    const hunk = this.hunkForLine(line);
    if (!hunk) return;
    const ch = this.gitHunks();
    const kind: 'added' | 'removed' | 'modified' = ch.added.has(line)
      ? 'added'
      : ch.removed.has(line)
        ? 'removed'
        : 'modified';
    const ol = hunk.oldLines ?? hunk.old_lines ?? 0;
    const nl = hunk.newLines ?? hunk.new_lines ?? 0;
    const pureAdd = ol <= 0;
    const current = (this.editing() ? this.draftContent() : this.fileContent()).split('\n');
    const newStart = hunk.newStart ?? hunk.new_start ?? 1;
    const newLines =
      nl <= 0
        ? []
        : current.slice(Math.max(0, newStart - 1), newStart - 1 + nl);
    this.hunkDialog.set({ line, kind, oldLines: [], newLines, pureAdd, loading: !pureAdd });
    if (pureAdd) return;
    const dir = this.directory();
    const path = this.selectedPath();
    const id = this.matchProjectId(dir);
    if (!dir || !path || !id) {
      this.hunkDialog.update((d) => (d ? { ...d, loading: false } : d));
      return;
    }
    try {
      const res = await this.engine.gitFileContent(id, path, 'HEAD');
      const head = (res.content ?? '').split('\n');
      const oldStart = hunk.oldStart ?? hunk.old_start ?? 1;
      const start = Math.max(0, oldStart - 1);
      const oldLines = head.slice(start, start + ol);
      this.hunkDialog.update((d) => (d && d.line === line ? { ...d, oldLines, loading: false } : d));
    } catch (err) {
      this.error.set(this.describe(err));
      this.hunkDialog.update((d) => (d && d.line === line ? { ...d, loading: false } : d));
    }
  }

  closeHunkDialog(): void {
    this.hunkDialog.set(null);
  }

  /** Replace the dialog's hunk new-side lines with the HEAD old-side lines. */
  restoreHunkFragment(): void {
    const dlg = this.hunkDialog();
    if (!dlg || dlg.pureAdd || dlg.loading) return;
    const hunk = this.hunkForLine(dlg.line);
    if (!hunk) return;
    const nl = hunk.newLines ?? hunk.new_lines ?? 0;
    if (nl <= 0) return; // pure deletion: nothing to restore in place
    const text = this.editing() ? this.draftContent() : this.fileContent();
    const lines = text.split('\n');
    const newStart = hunk.newStart ?? hunk.new_start ?? 1;
    const start = Math.max(0, newStart - 1);
    lines.splice(start, nl, ...dlg.oldLines);
    const next = lines.join('\n');
    if (this.editing()) {
      this.onDraftChange(next);
    } else {
      this.draftContent.set(next);
      this.fileContent.set(next);
      this.dirty.set(false);
      void this.saveEdit();
    }
    this.closeHunkDialog();
    void this.reloadChangedLines();
  }


  // Minimap (Sublime-style)
  readonly minimapCanvas = viewChild<ElementRef<HTMLCanvasElement>>('minimapCanvas');
  readonly minimapCanvasRO = viewChild<ElementRef<HTMLCanvasElement>>('minimapCanvasRO');
  readonly minimapViewportTop = signal(0);
  readonly minimapViewportHeight = signal(20);
  private minimapDragging = false;

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
    this.gitUnsub = this.events.onEvent((ev) => {
      if (ev.type === 'git.changed') {
        void this.reloadChangedLines();
      }
    });
    this.events.start();
    void this.reloadChangedLines();
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
    if (this.restoredDir !== dir) {
      this.restoredDir = dir;
      this.restoreTabs(dir);
    }
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
    this.stashActive();
    this.addTab(node.path);
    this.resetGitPanes();
    this.find.closeFind();
    this.htmlPreview.set(null);
    this.mdPreview.set(false);
    this.gotoChoices.set(null);
    this.error.set(null);
    const kept = this.buffers.get(node.path);
    this.buffers.delete(node.path);
    if (kept && kept.draft !== kept.saved && !isImagePath(node.path)) {
      // Unsaved edits from an earlier visit: bring them back without re-reading disk.
      this.fileContent.set(kept.saved);
      this.draftContent.set(kept.draft);
      this.imageData.set(null);
      this.imageMime.set(null);
      this.editing.set(true);
      this.dirty.set(true);
      return;
    }
    this.editing.set(false);
    this.dirty.set(false);
    this.fileLoading.set(true);
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
    void this.reloadChangedLines();
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

  // --- Git: file History / Blame column --------------------------------------

  /** The three panes above the editor: content, commit history, blame. */
  setViewMode(mode: 'code' | 'history' | 'blame'): void {
    this.historyOpen.set(mode === 'history');
    this.blameOpen.set(mode === 'blame');
    const path = this.selectedPath();
    if (!path) {
      return;
    }
    if (mode === 'history') {
      this.selectedCommit.set(null);
      this.commitDiff.set('');
      this.clearCompare();
      void this.loadFileHistory(path);
    } else if (mode === 'blame') {
      void this.loadFileBlame(path);
    }
  }

  /**
   * The file's commit history (`GET /projects/{id}/git/log?path=`) - first
   * page; `loadMoreHistory()` appends the next one using `skip`.
   */
  async loadFileHistory(path: string): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    const id = this.matchProjectId(dir);
    if (!id) {
      this.fileHistory.set([]);
      this.historyHasMore.set(false);
      return;
    }
    this.historyLoading.set(true);
    this.error.set(null);
    try {
      const res = await this.engine.gitLog(id, HISTORY_PAGE, 0, path);
      if (this.selectedPath() !== path) {
        return; // the user moved on while this was in flight
      }
      this.fileHistory.set(res.entries);
      this.historyHasMore.set(res.has_more);
    } catch (err) {
      this.fileHistory.set([]);
      this.historyHasMore.set(false);
      this.error.set(this.describe(err));
    } finally {
      this.historyLoading.set(false);
    }
  }

  /** Next history page: `skip` = how many commits are already listed. */
  async loadMoreHistory(): Promise<void> {
    const dir = this.directory();
    const path = this.selectedPath();
    if (!dir || !path || this.historyLoading() || !this.historyHasMore()) {
      return;
    }
    const id = this.matchProjectId(dir);
    if (!id) {
      return;
    }
    this.historyLoading.set(true);
    try {
      const res = await this.engine.gitLog(id, HISTORY_PAGE, this.fileHistory().length, path);
      if (this.selectedPath() !== path) {
        return;
      }
      this.fileHistory.update((rows) => [...rows, ...res.entries]);
      this.historyHasMore.set(res.has_more);
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.historyLoading.set(false);
    }
  }

  /** Show one commit's unified diff (`GET /projects/{id}/git/show`). */
  async showCommit(entry: GitLogEntry): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    const id = this.matchProjectId(dir);
    if (!id) {
      return;
    }
    this.selectedCommit.set(entry);
    this.clearCompare();
    this.commitLoading.set(true);
    this.commitDiff.set('');
    try {
      const show = await this.engine.gitShow(id, entry.hash, this.selectedPath());
      if (this.selectedCommit()?.hash !== entry.hash) {
        return;
      }
      this.commitDiff.set(show.diff);
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.commitLoading.set(false);
    }
  }

  /**
   * Diff of the working copy against the selected commit. First tries
   * `GET /projects/{id}/git/file/diff?path=&rev=` (one call, working tree
   * against the commit); when every hunk is a pure addition the pre-image is
   * exact (strip the added lines) and the unified diff is built locally.
   * Anything else (deletions/modifications need old-side content, or an old
   * engine without the route) falls back to rebuilding the pre-image on the
   * client by reverse-applying this commit and every newer one that touched
   * the file (see `explorer-git.ts`); uncommitted edits stay in the diff.
   */
  async compareWithWorking(): Promise<void> {
    const dir = this.directory();
    const path = this.selectedPath();
    const entry = this.selectedCommit();
    if (!dir || !path || !entry) {
      return;
    }
    this.commitLoading.set(true);
    this.workingDiff.set('');
    this.comparing.set(true);
    try {
      const working = this.editing() ? this.draftContent() : this.fileContent();
      const label = `@${entry.hash.slice(0, 7)}`;
      const id = this.matchProjectId(dir);
      let done = false;
      if (id) {
        try {
          const hunks = await this.engine.gitFileDiff(id, path, entry.hash);
          if (this.selectedPath() !== path || this.selectedCommit()?.hash !== entry.hash) {
            return;
          }
          if (hunks.length === 0) {
            this.workingDiff.set('');
            done = true;
          } else if (hunks.every((h) => ((h.oldLines ?? h.old_lines ?? 0) === 0))) {
            const drop = new Set<number>();
            for (const h of hunks) {
              const hs = (h.newStart ?? h.new_start ?? 1);
              for (let i = 0; i < (h.newLines ?? h.new_lines ?? 0); i++) {
                drop.add(hs + i);
              }
            }
            const before = working
              .split('\n')
              .filter((_, i) => !drop.has(i + 1))
              .join('\n');
            this.workingDiff.set(unifiedDiff(before, working, label, path, 3));
            done = true;
          }
        } catch {
          // Old engine without the route (or a failed diff): chain fallback below.
        }
      }
      if (!done) {
        const rows = this.fileHistory();
        const idx = rows.findIndex((row) => row.hash === entry.hash);
        const chain = [...(idx > 0 ? rows.slice(0, idx) : []), entry].slice(0, COMPARE_CHAIN_MAX);
        let content = working;
        const cid = this.matchProjectId(dir);
        if (!cid) {
          this.error.set(this.describe(new Error('explorer.noProject')));
          return;
        }
        for (const commit of chain) {
          const show = await this.engine.gitShow(cid, commit.hash, path);
          content = reverseApply(content, parseFileDiff(show.diff, path).hunks);
        }
        const caretLabel = `@${entry.hash.slice(0, 7)}^`;
        this.workingDiff.set(unifiedDiff(content, working, caretLabel, path, 3));
      }
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.commitLoading.set(false);
    }
  }

  /**
   * Per-line attribution for the open file. First tries
   * `GET /projects/{id}/git/file/blame?path=`; when the engine has no blame
   * endpoint the attribution is derived from the file's history (best-effort
   * fallback, kept): newest commit first, each commit's diff says which
   * new-side lines it added; the first commit that added a line owns it.
   * Lines edited but not added stay on the older owner, which is why the
   * fallback column is announced as best-effort.
   */
  async loadFileBlame(path: string): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    this.blameLoading.set(true);
    this.error.set(null);
    try {
      const id = this.matchProjectId(dir);
      if (id) {
        try {
          const rows = await this.engine.gitFileBlame(id, path);
          if (this.selectedPath() !== path) {
            return;
          }
          this.fileBlame.set(
            rows.map((row) => {
              const uncommitted = !row.hash || /^0+$/.test(row.hash);
              return {
                line: row.line,
                hash: uncommitted ? '' : row.hash,
                short: uncommitted ? '' : row.hash.slice(0, 7),
                author: row.author ?? '',
                subject: row.summary ?? '',
                ts: row.ts ?? 0,
              };
            }),
          );
          return;
        } catch {
          // Old engine without the route: best-effort fallback below.
        }
      }
      await this.loadFileBlameBestEffort(path);
    } finally {
      this.blameLoading.set(false);
    }
  }

  /**
   * Best-effort blame fallback for engines without `git/file/blame`: newest
   * commit first, each commit's diff says which new-side lines it added; the
   * first commit that added a line owns it.
   */
  private async loadFileBlameBestEffort(path: string): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    const id = this.matchProjectId(dir);
    if (!id) {
      return;
    }
    try {
      const total = Math.max(1, (this.fileContent() || this.draftContent()).split('\n').length);
      const log = await this.engine.gitLog(id, BLAME_COMMITS, 0, path);
      const owners: (GitLogEntry | null)[] = new Array<GitLogEntry | null>(total).fill(null);
      let remaining = total;
      for (const entry of log.entries) {
        if (remaining <= 0 || this.selectedPath() !== path) {
          break;
        }
        const show = await this.engine.gitShow(id, entry.hash, path);
        for (const line of parseFileDiff(show.diff, path).added) {
          if (line >= 1 && line <= total && !owners[line - 1]) {
            owners[line - 1] = entry;
            remaining--;
          }
        }
      }
      if (this.selectedPath() !== path) {
        return;
      }
      this.fileBlame.set(
        owners.map((owner, i) => ({
          line: i + 1,
          hash: owner?.hash ?? '',
          short: owner ? owner.hash.slice(0, 7) : '',
          author: owner?.author ?? '',
          subject: owner?.subject ?? '',
          ts: owner?.ts ?? 0,
        })),
      );
    } catch (err) {
      this.fileBlame.set([]);
      this.error.set(this.describe(err));
    } finally {
      this.blameLoading.set(false);
    }
  }

  /** Back from the working-copy diff to the commit's own diff. */
  clearCompare(): void {
    this.comparing.set(false);
    this.workingDiff.set('');
  }

  /** The blame column: one `hash author` label per file line. */
  readonly blameText = computed(() => {
    const rows = this.fileBlame();
    const total = this.fileLineNumbers().split('\n').length;
    const out: string[] = [];
    for (let i = 0; i < total; i++) {
      out.push(this.blameLabel(rows[i]));
    }
    return out.join('\n');
  });

  /** Blame text of one line, as shown in the blame column. */
  blameLabel(row: BlameLine | undefined): string {
    if (!row || !row.hash) {
      return this.i18n.t('explorer.blameUnknown');
    }
    return `${row.short} ${row.author}`;
  }

  /** Commit timestamp (`ts` is seconds since epoch) as a short local date. */
  formatCommitDate(ts: number): string {
    if (!ts) {
      return '';
    }
    return new Date(ts * 1000).toLocaleString(this.i18n.lang(), {
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
    });
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

  openFindInFiles(query?: string): void {
    if (!this.directory()) {
      return;
    }
    const area = this.editorArea()?.nativeElement;
    let prefill = '';
    if (query) {
      prefill = query;
    } else if (area && area.selectionEnd > area.selectionStart) {
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
      void this.reloadChangedLines();
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
    // Each tab keeps its own unsaved draft, so moving to another file never
    // loses edits; the unsaved-changes dialog is only for closing a tab.
    await this.navigate(loc);
  }

  async saveAndContinue(): Promise<void> {
    const closing = this.pendingClose();
    if (closing) {
      await this.saveAndClose(closing);
      return;
    }
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
    const closing = this.pendingClose();
    if (closing) {
      this.pendingClose.set(null);
      this.dropTabs((p) => p === closing);
      return;
    }
    const loc = this.pendingNav();
    this.pendingNav.set(null);
    this.cancelEdit();
    if (loc) {
      await this.navigate(loc);
    }
  }

  cancelNav(): void {
    this.pendingNav.set(null);
    this.pendingClose.set(null);
  }

  // --- Editor tabs -------------------------------------------------------------

  private fileNode(path: string): FsNode {
    return { name: '', path, is_dir: false, depth: 0, expanded: false };
  }

  tabName(path: string): string {
    return path.slice(path.lastIndexOf('/') + 1);
  }

  isTabDirty(path: string): boolean {
    if (path === this.activePath()) {
      return this.dirty();
    }
    const buf = this.buffers.get(path);
    return !!buf && buf.draft !== buf.saved;
  }

  /** Keep the text of the tab being left, so switching back does not lose edits. */
  private stashActive(): void {
    const path = this.activePath();
    if (!path || this.fileLoading() || this.imageData() !== null) {
      return;
    }
    this.buffers.set(path, { saved: this.fileContent(), draft: this.draftContent() });
  }

  private addTab(path: string): void {
    let tabs = this.openTabs();
    if (!tabs.some((t) => t.path === path)) {
      if (tabs.length >= TABS_MAX) {
        // Over the cap: evict the oldest tab that holds no unsaved edits.
        const victim = tabs.find((t) => !this.isTabDirty(t.path));
        if (victim) {
          tabs = tabs.filter((t) => t !== victim);
          this.buffers.delete(victim.path);
        }
      }
      this.openTabs.set([...tabs, { path }]);
    }
    this.activePath.set(path);
    this.persistTabs();
  }

  /** Show the tab for `path`; its unsaved edits, if any, come back with it. */
  activateTab(path: string): void {
    if (path !== this.activePath()) {
      void this.openFile(this.fileNode(path));
    }
  }

  /** Tab "x", middle-click and Ctrl/Cmd+W. Asks first when the tab has unsaved edits. */
  closeTab(path: string): void {
    if (this.isTabDirty(path)) {
      this.pendingClose.set(path);
      return;
    }
    this.dropTabs((p) => p === path);
  }

  onTabAux(event: MouseEvent, path: string): void {
    if (event.button === 1) {
      event.preventDefault();
      this.closeTab(path);
    }
  }

  /** Remove every tab matching `match`; if the active one goes, show a neighbour. */
  private dropTabs(match: (path: string) => boolean): void {
    const tabs = this.openTabs();
    if (!tabs.some((t) => match(t.path))) {
      return;
    }
    const idx = tabs.findIndex((t) => t.path === this.activePath());
    const activeGone = idx >= 0 && match(tabs[idx].path);
    const remaining = tabs.filter((t) => !match(t.path));
    this.openTabs.set(remaining);
    for (const t of tabs) {
      if (match(t.path)) {
        this.buffers.delete(t.path);
      }
    }
    if (activeGone) {
      this.clearViewer();
      const next = remaining[Math.min(idx, remaining.length - 1)];
      if (next) {
        void this.openFile(this.fileNode(next.path));
      }
    }
    this.persistTabs();
  }

  private clearViewer(): void {
    this.activePath.set(null);
    this.fileContent.set('');
    this.draftContent.set('');
    this.editing.set(false);
    this.dirty.set(false);
    this.htmlPreview.set(null);
    this.mdPreview.set(false);
    this.gotoChoices.set(null);
    this.imageData.set(null);
    this.imageMime.set(null);
  }

  /** Follow a rename/move in the tree for open tabs, buffers and the active file. */
  private renameTabs(from: string, to: string): void {
    const map = (p: string): string =>
      p === from ? to : p.startsWith(from + '/') ? to + p.slice(from.length) : p;
    this.openTabs.update((tabs) => tabs.map((t) => ({ ...t, path: map(t.path) })));
    for (const [p, buf] of [...this.buffers]) {
      if (map(p) !== p) {
        this.buffers.delete(p);
        this.buffers.set(map(p), buf);
      }
    }
    const active = this.activePath();
    if (active) {
      this.activePath.set(map(active));
    }
    this.persistTabs();
  }

  /** Save the tab's draft (active or background), then close it. */
  private async saveAndClose(path: string): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    if (path === this.activePath()) {
      await this.saveEdit();
      if (this.dirty()) {
        return; // save failed: the error banner shows why and the dialog stays open
      }
    } else {
      const buf = this.buffers.get(path);
      if (buf && buf.draft !== buf.saved) {
        try {
          await this.engine.fsFileWrite(dir, path, buf.draft);
        } catch (err) {
          this.error.set(this.describe(err));
          return;
        }
      }
    }
    this.pendingClose.set(null);
    this.dropTabs((p) => p === path);
  }

  /** Ctrl/Cmd+W closes the active tab; Ctrl/Cmd+Tab (+Shift) cycles through tabs. */
  private handleTabKeys(event: KeyboardEvent, mod: boolean): boolean {
    if (!mod || this.pendingClose() || this.pendingNav() || this.openTabs().length === 0) {
      return false;
    }
    if (event.key.toLowerCase() === 'w' && !event.shiftKey && !event.altKey) {
      const active = this.activePath();
      if (!active) {
        return false;
      }
      event.preventDefault();
      this.closeTab(active);
      return true;
    }
    if (event.key === 'Tab') {
      const tabs = this.openTabs();
      if (tabs.length < 2) {
        return false;
      }
      event.preventDefault();
      const idx = tabs.findIndex((t) => t.path === this.activePath());
      const step = event.shiftKey ? -1 : 1;
      this.activateTab(tabs[(idx + step + tabs.length) % tabs.length].path);
      return true;
    }
    return false;
  }

  /** Debounced write of the tab list for the project it belongs to. */
  private persistTabs(): void {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    const key = TABS_KEY + dir;
    if (this.tabsSave && this.tabsSave.key !== key) {
      this.flushTabs();
    }
    this.tabsSave = {
      key,
      value: JSON.stringify({
        tabs: this.openTabs().map((t) => t.path),
        active: this.activePath(),
      }),
    };
    if (this.tabsSaveTimer) {
      clearTimeout(this.tabsSaveTimer);
    }
    this.tabsSaveTimer = setTimeout(() => this.flushTabs(), TABS_SAVE_DELAY_MS);
  }

  private flushTabs(): void {
    if (this.tabsSaveTimer) {
      clearTimeout(this.tabsSaveTimer);
      this.tabsSaveTimer = null;
    }
    const pending = this.tabsSave;
    this.tabsSave = null;
    if (!pending) {
      return;
    }
    try {
      localStorage.setItem(pending.key, pending.value);
    } catch {
      /* storage full / disabled: tabs are just not remembered */
    }
  }

  /** Re-open the tabs saved for `dir`; the active one is loaded, the rest on demand. */
  private restoreTabs(dir: string): void {
    let raw: string | null = null;
    try {
      raw = localStorage.getItem(TABS_KEY + dir);
    } catch {
      return;
    }
    if (!raw) {
      return;
    }
    let saved: { tabs?: unknown; active?: unknown } | null = null;
    try {
      saved = JSON.parse(raw) as { tabs?: unknown; active?: unknown } | null;
    } catch {
      return;
    }
    const stored = Array.isArray(saved?.tabs)
      ? (saved?.tabs as unknown[]).filter((p): p is string => typeof p === 'string')
      : [];
    const paths = stored.slice(0, TABS_MAX);
    if (paths.length === 0) {
      return;
    }
    // Files already opened before the restore (e.g. from the Preview panel) keep their tab.
    const merged = [...new Set([...paths, ...this.openTabs().map((t) => t.path)])];
    this.openTabs.set(merged.map((path) => ({ path })));
    if (this.activePath()) {
      this.persistTabs();
      return;
    }
    const active =
      typeof saved?.active === 'string' && paths.includes(saved.active)
        ? saved.active
        : paths[paths.length - 1];
    void this.openFile(this.fileNode(active));
    void this.revealPath(active);
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
      // The buffer travels along: locals/params/destructured consts, imports
      // and unsaved edits resolve, and the engine knows the caret's scope.
      const res = await this.engine.fsDefinitionAt(dir, {
        path,
        symbol,
        text,
        offset: utf8Offset(text, offset),
      });
      if (res.on_declaration) {
        // IntelliJ: Ctrl+click on a declaration shows its usages.
        this.openFindInFiles(symbol);
        return;
      }
      const here = lineOf(text, offset);
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
    this.navForward.length = 0;
    await this.requestOpen({ path: hit.path, line: hit.line, column: hit.column });
  }

  /** Alt+Left: back to the location before the last jump. */
  async goBack(): Promise<void> {
    const loc = this.navHistory.pop();
    if (loc) {
      const here = this.currentLocation();
      if (here) {
        this.navForward.push(here);
      }
      await this.requestOpen(loc);
    }
  }

  /** Alt+Right: redo the jump undone by Alt+Left. */
  async goForward(): Promise<void> {
    const loc = this.navForward.pop();
    if (loc) {
      const here = this.currentLocation();
      if (here) {
        this.navHistory.push(here);
      }
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

  /** Keep the highlighted layer aligned with the textarea's scroll.
   *  Uses a compositor `transform` instead of `scrollTop`: setting scrollTop
   *  on the layer forces WebKitGTK (the Tauri webview) to repaint thousands
   *  of highlight spans synchronously on every wheel tick, which throttled
   *  wheel scrolling ~11x (measured 6.8 px/wheel vs 85 px/wheel on a plain
   *  div). A transform just re-composites the already-painted layer. */
  syncLayer(area: HTMLTextAreaElement): void {
    const layer = this.editorLayer()?.nativeElement;
    if (layer) {
      const code = layer.firstElementChild as HTMLElement | null;
      const target = code ?? layer;
      target.style.transform = `translate(${-area.scrollLeft}px, ${-area.scrollTop}px)`;
    }
    const gutter = this.gutterEl()?.nativeElement;
    if (gutter) {
      gutter.scrollTop = area.scrollTop;
    }
  }

  /** The find bar scrolls the layer to a match: follow it with the textarea. */
  syncArea(layer: HTMLElement): void {
    const area = this.editorArea()?.nativeElement;
    if (area && (area.scrollTop !== layer.scrollTop || area.scrollLeft !== layer.scrollLeft)) {
      area.scrollTop = layer.scrollTop;
      area.scrollLeft = layer.scrollLeft;
      this.syncLayer(area);
    }
  }

  // --- Minimap (Sublime-style) -------------------------------------------

  renderMinimap(): void {
    const text = this.editing() ? this.draftContent() : this.fileContent();
    const canvas = (this.editing() ? this.minimapCanvas() : this.minimapCanvasRO())?.nativeElement
      ?? this.minimapCanvas()?.nativeElement
      ?? this.minimapCanvasRO()?.nativeElement;
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    const rect = canvas.getBoundingClientRect();
    const w = 64 * dpr;
    const h = Math.max(100, rect.height * dpr);
    canvas.width = w;
    canvas.height = h;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    ctx.scale(dpr, dpr);
    const cssW = 64;
    const cssH = h / dpr;
    ctx.clearRect(0, 0, cssW, cssH);
    ctx.fillStyle = getComputedStyle(document.documentElement).getPropertyValue('--surface-1').trim() || '#1c1e24';
    ctx.fillRect(0, 0, cssW, cssH);
    const lines = text.split('\n');
    const totalLines = lines.length || 1;
    const lineH = cssH / totalLines;
    // Draw each line as a thin bar proportional to its length
    for (let i = 0; i < lines.length; i++) {
      const len = lines[i].length;
      if (len === 0) continue;
      const barW = Math.min(cssW - 4, Math.max(2, (len / 120) * (cssW - 4)));
      // Color by content type
      const trimmed = lines[i].trim();
      if (trimmed.startsWith('//') || trimmed.startsWith('#') || trimmed.startsWith('/*') || trimmed.startsWith('*')) {
        ctx.fillStyle = 'rgba(107,114,128,0.6)';
      } else if (trimmed.startsWith('import') || trimmed.startsWith('export') || trimmed.startsWith('from')) {
        ctx.fillStyle = 'rgba(232,147,90,0.5)';
      } else if (trimmed === '' || trimmed === '{' || trimmed === '}') {
        ctx.fillStyle = 'rgba(107,114,128,0.25)';
      } else {
        ctx.fillStyle = 'rgba(199,203,211,0.55)';
      }
      const y = i * lineH;
      const bh = Math.max(1, lineH * 0.7);
      ctx.fillRect(2, y, barW, bh);
    }
    this.updateMinimapViewport();
  }

  updateMinimapViewport(): void {
    const area = this.editing() ? this.editorArea()?.nativeElement : null;
    const wrap = !this.editing() ? document.querySelector('.file-content-wrap') as HTMLElement | null : null;
    const scrollEl = area ?? wrap;
    if (!scrollEl) return;
    const canvas = (this.editing() ? this.minimapCanvas() : this.minimapCanvasRO())?.nativeElement
      ?? this.minimapCanvas()?.nativeElement
      ?? this.minimapCanvasRO()?.nativeElement;
    if (!canvas) return;
    const rect = canvas.getBoundingClientRect();
    const cssH = rect.height || 400;
    const text = this.editing() ? this.draftContent() : this.fileContent();
    const totalLines = (text.split('\n').length) || 1;
    const scrollHeight = scrollEl.scrollHeight;
    const clientHeight = scrollEl.clientHeight;
    const scrollTop = scrollEl.scrollTop;
    if (scrollHeight <= clientHeight) {
      this.minimapViewportTop.set(0);
      this.minimapViewportHeight.set(cssH);
      return;
    }
    const ratio = cssH / scrollHeight;
    // Approximate: map scroll position to minimap
    const top = (scrollTop / scrollHeight) * cssH;
    const height = (clientHeight / scrollHeight) * cssH;
    this.minimapViewportTop.set(top);
    this.minimapViewportHeight.set(Math.max(12, height));
    void totalLines;
  }

  onMinimapClick(event: MouseEvent): void {
    const canvas = (event.currentTarget as HTMLElement).querySelector('canvas') ?? event.currentTarget as HTMLElement;
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    const y = event.clientY - rect.top;
    const ratio = y / rect.height;
    const area = this.editing() ? this.editorArea()?.nativeElement : null;
    const wrap = !this.editing() ? document.querySelector('.file-content-wrap') as HTMLElement | null : null;
    const scrollEl = area ?? wrap;
    if (!scrollEl) return;
    scrollEl.scrollTop = ratio * scrollEl.scrollHeight - scrollEl.clientHeight / 2;
    if (area) this.syncLayer(area);
    this.updateMinimapViewport();
  }

  onMinimapDragStart(event: MouseEvent): void {
    this.minimapDragging = true;
    const onMove = (e: MouseEvent) => {
      if (!this.minimapDragging) return;
      const target = event.currentTarget as HTMLElement;
      const rect = target.getBoundingClientRect();
      const y = e.clientY - rect.top;
      const ratio = Math.max(0, Math.min(1, y / rect.height));
      const area = this.editing() ? this.editorArea()?.nativeElement : null;
      const wrap = !this.editing() ? document.querySelector('.file-content-wrap') as HTMLElement | null : null;
      const scrollEl = area ?? wrap;
      if (!scrollEl) return;
      scrollEl.scrollTop = ratio * scrollEl.scrollHeight - scrollEl.clientHeight / 2;
      if (area) this.syncLayer(area);
      this.updateMinimapViewport();
    };
    const onUp = () => {
      this.minimapDragging = false;
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
    event.preventDefault();
  }

  /** Tab inserts two spaces instead of leaving the editor; the completion
   *  popup, when open, owns the navigation keys and Enter/Tab. */
  onEditorKeydown(event: KeyboardEvent, area: HTMLTextAreaElement): void {
    // Go to declaration from the keyboard: Ctrl/Cmd+B (IntelliJ), F12 (VS Code).
    const gotoKey =
      (event.key === 'F12' && !event.shiftKey && !event.altKey && !event.ctrlKey && !event.metaKey) ||
      ((event.ctrlKey || event.metaKey) && !event.shiftKey && !event.altKey && event.code === 'KeyB');
    if (gotoKey && this.canGoto()) {
      event.preventDefault();
      void this.gotoDefinitionAt(area.selectionStart ?? 0);
      return;
    }
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
        this.renameTabs(from, to);
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
      this.dropTabs((p) => p === path || p.startsWith(path + '/'));
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
    if (event.altKey && event.key === 'ArrowRight' && this.navForward.length > 0) {
      event.preventDefault();
      void this.goForward();
      return;
    }
    const mod = event.ctrlKey || event.metaKey;
    if (this.findInFilesOpen() || this.gotoOpen()) {
      return; // the dialog owns the keyboard
    }
    if (this.handleTabKeys(event, mod)) {
      return;
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

function lineNumbersFor(text: string): string {
  if (!text) {
    return '1';
  }
  const n = text.split('\n').length;
  let out = '';
  for (let i = 1; i <= n; i++) {
    out += i + (i < n ? '\n' : '');
  }
  return out;
}
