/**
 * "Go to" dialog (Explorer) - IntelliJ-style navigation popup.
 *
 * - Ctrl/Cmd+Shift+N - Files (fuzzy over every project file, `GET /fs/files`,
 *   `dir/name` and `name:line[:col]` patterns),
 * - Ctrl/Cmd+N        - Classes (types from the engine symbol cache),
 * - Ctrl/Cmd+Alt+Shift+N - Symbols (every definition),
 * - Shift, Shift      - All (grouped: classes, files, symbols).
 *
 * Tab / Shift+Tab cycles the tabs, Up/Down/PgUp/PgDn move, Enter opens,
 * Esc closes. An empty query lists the recently opened files.
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
  untracked,
  viewChild,
} from '@angular/core';

import { EngineClient } from '../../core/engine-client.service';
import { FsSymbolHit } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';
import {
  fuzzyMatch,
  highlightRuns,
  isClassKind,
  kindLabel,
  parseGotoQuery,
  rankFiles,
} from './goto-match';

export type GotoTab = 'all' | 'classes' | 'files' | 'symbols';

export const GOTO_TABS: readonly GotoTab[] = ['all', 'classes', 'files', 'symbols'];

/** Where to open a picked result. */
export interface GotoTarget {
  path: string;
  line?: number;
  column?: number;
}

type Run = { text: string; hit: boolean };

export interface GotoRow {
  kind: 'file' | 'symbol';
  /** Group header shown above this row (All tab). */
  group?: string;
  label: Run[];
  /** Directory / location shown dimmed after the label. */
  detail: Run[];
  badge: string;
  target: GotoTarget;
}

const DEBOUNCE_MS = 120;
const FILE_LIMIT = 200;
const SYMBOL_LIMIT = 200;
const ALL_LIMITS = { classes: 8, files: 15, symbols: 8 };
/** Reopening within this window reuses the file list (instant popup). */
const FILES_TTL_MS = 30_000;

const filesCache = new Map<string, { at: number; files: string[] }>();

function dirOf(path: string): string {
  const i = path.lastIndexOf('/');
  return i < 0 ? '' : path.slice(0, i);
}

function nameOf(path: string): string {
  const i = path.lastIndexOf('/');
  return i < 0 ? path : path.slice(i + 1);
}

@Component({
  selector: 'app-goto-dialog',
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './goto-dialog.html',
  styleUrl: './goto-dialog.css',
  host: {
    '(document:keydown)': 'onKeydown($event)',
  },
})
export class GotoDialogComponent {
  private readonly engine = inject(EngineClient);
  private readonly i18n = inject(I18nService);
  private readonly destroyRef = inject(DestroyRef);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly directory = input.required<string>();
  readonly initialTab = input<GotoTab>('files');
  readonly initialQuery = input('');
  /** Recently opened files, most recent first (shown for an empty query). */
  readonly recent = input<readonly string[]>([]);

  readonly opened = output<GotoTarget>();
  readonly closed = output<void>();

  readonly tabs = GOTO_TABS;
  readonly tab = signal<GotoTab>('files');
  readonly query = signal('');
  readonly index = signal(0);

  readonly files = signal<string[] | null>(null);
  readonly filesTruncated = signal(false);
  readonly symbols = signal<FsSymbolHit[]>([]);
  readonly symbolsLoading = signal(false);
  /** Query the current `symbols()` answer belongs to. */
  private symbolsFor = '';
  readonly error = signal<string | null>(null);

  private readonly queryInput = viewChild<ElementRef<HTMLInputElement>>('queryInput');
  private readonly listEl = viewChild<ElementRef<HTMLElement>>('list');

  private timer: number | undefined;
  private seq = 0;

  readonly parsed = computed(() => parseGotoQuery(this.query()));

  private readonly fileRows = computed<GotoRow[]>(() => {
    const files = this.files();
    const { pattern, line, column } = this.parsed();
    if (!files) {
      return [];
    }
    if (!pattern) {
      const known = new Set(files);
      return this.recent()
        .filter((p) => known.has(p))
        .slice(0, FILE_LIMIT)
        .map((p) => this.fileRow(p, [], [], line, column));
    }
    return rankFiles(files, pattern, FILE_LIMIT).map((m) =>
      this.fileRow(m.path, m.nameIndices, m.dirIndices, line, column),
    );
  });

  private readonly rankedSymbols = computed(() => {
    const { pattern } = this.parsed();
    if (!pattern) {
      return [];
    }
    const out: { hit: FsSymbolHit; score: number; indices: number[] }[] = [];
    for (const hit of this.symbols()) {
      const m = fuzzyMatch(pattern, hit.name);
      if (m) {
        const exact = hit.name.toLowerCase() === pattern.toLowerCase() ? 30 : 0;
        out.push({ hit, score: m.score + exact, indices: m.indices });
      }
    }
    out.sort(
      (a, b) =>
        b.score - a.score ||
        a.hit.name.length - b.hit.name.length ||
        a.hit.path.localeCompare(b.hit.path) ||
        a.hit.line - b.hit.line,
    );
    return out;
  });

  private readonly classRows = computed<GotoRow[]>(() =>
    this.rankedSymbols()
      .filter((s) => isClassKind(s.hit.kind))
      .map((s) => this.symbolRow(s.hit, s.indices)),
  );

  private readonly symbolRows = computed<GotoRow[]>(() =>
    this.rankedSymbols().map((s) => this.symbolRow(s.hit, s.indices)),
  );

  readonly rows = computed<GotoRow[]>(() => {
    switch (this.tab()) {
      case 'files':
        return this.fileRows();
      case 'classes':
        return this.classRows();
      case 'symbols':
        return this.symbolRows();
      case 'all': {
        const classes = this.classRows().slice(0, ALL_LIMITS.classes);
        const shown = new Set(classes.map((r) => `${r.target.path}:${r.target.line}`));
        const files = this.fileRows().slice(0, ALL_LIMITS.files);
        const symbols = this.symbolRows()
          .filter((r) => !shown.has(`${r.target.path}:${r.target.line}`))
          .slice(0, ALL_LIMITS.symbols);
        return [
          ...this.grouped(classes, this.t('goto.tabClasses')),
          ...this.grouped(files, this.t('goto.tabFiles')),
          ...this.grouped(symbols, this.t('goto.tabSymbols')),
        ];
      }
    }
  });

  readonly selected = computed<GotoRow | null>(() => this.rows()[this.index()] ?? null);

  readonly loading = computed(
    () =>
      (this.tab() !== 'symbols' && this.tab() !== 'classes' && this.files() === null) ||
      (this.tab() !== 'files' && this.symbolsLoading()),
  );

  readonly emptyText = computed(() => {
    if (this.loading()) {
      return this.t('goto.loading');
    }
    if (!this.parsed().pattern) {
      return this.tab() === 'files' || this.tab() === 'all'
        ? this.t('goto.noRecent')
        : this.t('goto.typeToSearch');
    }
    return this.t('goto.noResults');
  });

  constructor() {
    effect(() => {
      const tab = this.initialTab();
      untracked(() => {
        this.tab.set(tab);
        this.index.set(0);
        this.queueSymbols(0);
      });
    });
    effect(() => {
      const q = this.initialQuery();
      untracked(() => {
        if (q && !this.query()) {
          this.query.set(q);
          this.queueSymbols(0);
        }
      });
    });
    effect(() => {
      const dir = this.directory();
      untracked(() => void this.loadFiles(dir));
    });
    queueMicrotask(() => {
      const el = this.queryInput()?.nativeElement;
      el?.focus();
      el?.select();
    });
    this.destroyRef.onDestroy(() => window.clearTimeout(this.timer));
  }

  private async loadFiles(dir: string): Promise<void> {
    const cached = filesCache.get(dir);
    if (cached) {
      this.files.set(cached.files);
      if (Date.now() - cached.at < FILES_TTL_MS) {
        return;
      }
    }
    try {
      const res = await this.engine.fsFiles(dir);
      filesCache.set(dir, { at: Date.now(), files: res.files });
      if (this.directory() === dir) {
        this.files.set(res.files);
        this.filesTruncated.set(res.truncated);
      }
    } catch (err) {
      if (!cached) {
        this.files.set([]);
        this.error.set(err instanceof Error ? err.message : String(err));
      }
    }
  }

  onQuery(value: string): void {
    this.query.set(value);
    this.index.set(0);
    this.queueSymbols();
  }

  setTab(tab: GotoTab): void {
    this.tab.set(tab);
    this.index.set(0);
    this.queueSymbols(0);
    this.queryInput()?.nativeElement.focus();
  }

  /** Symbols are only fetched for tabs that show them. */
  private queueSymbols(delay = DEBOUNCE_MS): void {
    window.clearTimeout(this.timer);
    if (this.tab() === 'files') {
      return;
    }
    const pattern = this.parsed().pattern;
    if (!pattern) {
      this.seq++;
      this.symbols.set([]);
      this.symbolsFor = '';
      this.symbolsLoading.set(false);
      return;
    }
    if (pattern === this.symbolsFor) {
      return;
    }
    this.symbolsLoading.set(true);
    this.timer = window.setTimeout(() => void this.fetchSymbols(pattern), delay);
  }

  private async fetchSymbols(pattern: string): Promise<void> {
    const seq = ++this.seq;
    try {
      const res = await this.engine.fsSymbols(this.directory(), pattern, SYMBOL_LIMIT);
      if (seq !== this.seq) {
        return;
      }
      this.symbols.set(res.symbols);
      this.symbolsFor = pattern;
      this.error.set(null);
    } catch (err) {
      if (seq === this.seq) {
        this.symbols.set([]);
        this.error.set(err instanceof Error ? err.message : String(err));
      }
    } finally {
      if (seq === this.seq) {
        this.symbolsLoading.set(false);
      }
    }
  }

  private fileRow(
    path: string,
    nameIdx: number[],
    dirIdx: number[],
    line?: number,
    column?: number,
  ): GotoRow {
    return {
      kind: 'file',
      label: highlightRuns(nameOf(path), nameIdx),
      detail: highlightRuns(dirOf(path), dirIdx),
      badge: (nameOf(path).split('.').pop() ?? '').slice(0, 4),
      target: { path, line, column },
    };
  }

  private symbolRow(hit: FsSymbolHit, indices: number[]): GotoRow {
    return {
      kind: 'symbol',
      label: highlightRuns(hit.name, indices),
      detail: [{ text: `${hit.path}:${hit.line}`, hit: false }],
      badge: kindLabel(hit.kind),
      target: { path: hit.path, line: hit.line, column: hit.column },
    };
  }

  private grouped(rows: GotoRow[], title: string): GotoRow[] {
    return rows.map((r, i) => (i === 0 ? { ...r, group: title } : r));
  }

  select(i: number): void {
    this.index.set(i);
  }

  open(row: GotoRow | null = this.selected()): void {
    if (row) {
      this.opened.emit(row.target);
    }
  }

  close(): void {
    this.closed.emit();
  }

  onKeydown(event: KeyboardEvent): void {
    const n = this.rows().length;
    switch (event.key) {
      case 'Escape':
        event.preventDefault();
        event.stopPropagation();
        this.close();
        return;
      case 'Tab': {
        event.preventDefault();
        const i = this.tabs.indexOf(this.tab());
        const next = (i + (event.shiftKey ? -1 : 1) + this.tabs.length) % this.tabs.length;
        this.setTab(this.tabs[next]);
        return;
      }
      case 'ArrowDown':
        if (n) {
          event.preventDefault();
          this.index.set((this.index() + 1) % n);
          this.reveal();
        }
        return;
      case 'ArrowUp':
        if (n) {
          event.preventDefault();
          this.index.set((this.index() - 1 + n) % n);
          this.reveal();
        }
        return;
      case 'PageDown':
      case 'PageUp':
        if (n) {
          event.preventDefault();
          const step = event.key === 'PageDown' ? 10 : -10;
          this.index.set(Math.min(n - 1, Math.max(0, this.index() + step)));
          this.reveal();
        }
        return;
      case 'Enter':
        event.preventDefault();
        this.open();
        return;
    }
  }

  private reveal(): void {
    queueMicrotask(() => {
      const el = this.listEl()?.nativeElement.querySelector('.goto-row.is-selected');
      (el as HTMLElement | null)?.scrollIntoView({ block: 'nearest' });
    });
  }
}
