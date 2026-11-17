/**
 * "Find in Files" dialog (Explorer, Ctrl/Cmd+Shift+F) - IntelliJ-style.
 *
 * Query field with Match case / Words / Regex toggles and a file mask, two
 * tabs - Text (gitignore-aware scan, file order ranked by the bebok-index
 * plugin when installed) and Symbols (tree-sitter definitions whose name
 * contains the query) - both from one `GET /fs/search` call. The result list
 * is keyboard driven (Up/Down, Enter opens, Esc closes) and the lower half
 * previews the selected hit in its file with surrounding lines.
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
import { FormsModule } from '@angular/forms';

import { EngineClient } from '../../core/engine-client.service';
import { FsSearchResponse } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';

/** Where to open a picked result. */
export interface FindInFilesTarget {
  path: string;
  line: number;
  column: number;
}

/** One row of the result list (text and symbol hits share it). */
export interface FindRow {
  path: string;
  line: number;
  column: number;
  /** Line text / declaration snippet. */
  text: string;
  /** Match span inside `text` (characters) - `null` when unknown. */
  matchStart: number | null;
  matchLength: number;
  /** Symbol name + kind (Symbols tab only). */
  symbol?: string;
  kind?: string;
}

/** Split `text` into before / match / after for highlighting. */
export function splitMatch(
  text: string,
  start: number | null,
  length: number,
): [string, string, string] {
  if (start === null || start < 0 || length <= 0) {
    return [text, '', ''];
  }
  const chars = Array.from(text);
  return [
    chars.slice(0, start).join(''),
    chars.slice(start, start + length).join(''),
    chars.slice(start + length).join(''),
  ];
}

/** Leading whitespace trimmed off a hit line, with the match start shifted. */
export function trimHitLine(text: string, column: number): { text: string; start: number } {
  const lead = text.length - text.trimStart().length;
  return { text: text.trim(), start: Math.max(0, column - 1 - lead) };
}

const DEBOUNCE_MS = 250;
const PREVIEW_CONTEXT = 8;

@Component({
  selector: 'app-find-in-files',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  templateUrl: './find-in-files.html',
  styleUrl: './find-in-files.css',
  host: {
    '(document:keydown)': 'onKeydown($event)',
  },
})
export class FindInFilesComponent {
  private readonly engine = inject(EngineClient);
  private readonly i18n = inject(I18nService);
  private readonly destroyRef = inject(DestroyRef);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly directory = input.required<string>();
  /** Prefill (the editor selection). */
  readonly initialQuery = input('');

  readonly opened = output<FindInFilesTarget>();
  readonly closed = output<void>();

  readonly query = signal('');
  readonly caseSensitive = signal(false);
  readonly wholeWord = signal(false);
  readonly regex = signal(false);
  readonly mask = signal('');
  readonly tab = signal<'text' | 'symbols'>('text');

  readonly loading = signal(false);
  readonly error = signal<string | null>(null);
  readonly response = signal<FsSearchResponse | null>(null);
  readonly index = signal(0);

  /** Lines of the previewed file (`null` while loading / none). */
  readonly previewLines = signal<string[] | null>(null);
  private readonly fileCache = new Map<string, string[]>();

  private readonly queryInput = viewChild<ElementRef<HTMLInputElement>>('queryInput');
  private readonly listEl = viewChild<ElementRef<HTMLElement>>('list');

  private timer: number | undefined;
  private seq = 0;

  readonly rows = computed<FindRow[]>(() => {
    const res = this.response();
    if (!res) {
      return [];
    }
    if (this.tab() === 'symbols') {
      return res.symbols.map((s) => {
        const hit = trimHitLine(s.snippet, s.column);
        return {
          path: s.path,
          line: s.line,
          column: s.column,
          text: hit.text,
          matchStart: hit.start,
          matchLength: Array.from(s.name).length,
          symbol: s.name,
          kind: s.kind,
        };
      });
    }
    return res.text.results.map((h) => {
      const hit = trimHitLine(h.text, h.column);
      return {
        path: h.path,
        line: h.line,
        column: h.column,
        text: hit.text,
        matchStart: hit.start,
        matchLength: h.length,
      };
    });
  });

  readonly selected = computed<FindRow | null>(() => this.rows()[this.index()] ?? null);

  readonly summary = computed(() => {
    const res = this.response();
    if (!res) {
      return '';
    }
    if (this.tab() === 'symbols') {
      return this.t('findInFiles.symbolCount', { count: res.symbols.length });
    }
    const base = this.t('findInFiles.textCount', {
      count: res.text.results.length,
      files: res.text.files_matched,
    });
    return res.text.truncated ? `${base} ${this.t('findInFiles.truncated')}` : base;
  });

  /** Preview window around the selected hit: `{ n, text, hit }` rows. */
  readonly previewRows = computed(() => {
    const lines = this.previewLines();
    const sel = this.selected();
    if (!lines || !sel) {
      return [];
    }
    const from = Math.max(1, sel.line - PREVIEW_CONTEXT);
    const to = Math.min(lines.length, sel.line + PREVIEW_CONTEXT);
    const out: { n: number; text: string; hit: boolean }[] = [];
    for (let n = from; n <= to; n++) {
      out.push({ n, text: lines[n - 1] ?? '', hit: n === sel.line });
    }
    return out;
  });

  constructor() {
    effect(() => {
      const q = this.initialQuery();
      untracked(() => {
        if (q && !this.query()) {
          this.query.set(q);
          this.schedule();
        }
      });
    });
    // Re-run when any option changes (query edits debounce via `onQuery`).
    effect(() => {
      this.caseSensitive();
      this.wholeWord();
      this.regex();
      untracked(() => this.schedule(0));
    });
    effect(() => {
      const sel = this.selected();
      untracked(() => void this.loadPreview(sel));
    });
    queueMicrotask(() => {
      const el = this.queryInput()?.nativeElement;
      el?.focus();
      el?.select();
    });
    this.destroyRef.onDestroy(() => window.clearTimeout(this.timer));
  }

  onQuery(value: string): void {
    this.query.set(value);
    this.schedule();
  }

  onMask(value: string): void {
    this.mask.set(value);
    this.schedule();
  }

  setTab(tab: 'text' | 'symbols'): void {
    this.tab.set(tab);
    this.index.set(0);
  }

  private schedule(delay = DEBOUNCE_MS): void {
    window.clearTimeout(this.timer);
    this.timer = window.setTimeout(() => void this.run(), delay);
  }

  async run(): Promise<void> {
    const q = this.query();
    const seq = ++this.seq;
    if (!q.trim()) {
      this.response.set(null);
      this.error.set(null);
      this.loading.set(false);
      return;
    }
    this.loading.set(true);
    this.error.set(null);
    try {
      const res = await this.engine.fsSearch(this.directory(), q, {
        caseSensitive: this.caseSensitive(),
        wholeWord: this.wholeWord(),
        regex: this.regex(),
        mask: this.mask().trim(),
      });
      if (seq !== this.seq) {
        return;
      }
      this.response.set(res);
      this.index.set(0);
      if (this.regex() && this.tab() === 'symbols') {
        this.tab.set('text');
      }
      // Nothing textual but symbols found: show them.
      if (this.tab() === 'text' && res.text.results.length === 0 && res.symbols.length > 0) {
        this.tab.set('symbols');
      }
    } catch (err) {
      if (seq === this.seq) {
        this.response.set(null);
        this.error.set(err instanceof Error ? err.message : String(err));
      }
    } finally {
      if (seq === this.seq) {
        this.loading.set(false);
      }
    }
  }

  private async loadPreview(sel: FindRow | null): Promise<void> {
    if (!sel) {
      this.previewLines.set(null);
      return;
    }
    const cached = this.fileCache.get(sel.path);
    if (cached) {
      this.previewLines.set(cached);
      return;
    }
    this.previewLines.set(null);
    try {
      const file = await this.engine.fsFile(this.directory(), sel.path);
      const lines = file.content.split(/\r?\n/);
      this.fileCache.set(sel.path, lines);
      if (this.selected()?.path === sel.path) {
        this.previewLines.set(lines);
      }
    } catch {
      this.previewLines.set([]);
    }
  }

  select(i: number): void {
    this.index.set(i);
  }

  open(row: FindRow | null = this.selected()): void {
    if (row) {
      this.opened.emit({ path: row.path, line: row.line, column: row.column });
    }
  }

  close(): void {
    this.closed.emit();
  }

  split(row: FindRow): [string, string, string] {
    return splitMatch(row.text, row.matchStart, row.matchLength);
  }

  dirOf(path: string): string {
    const i = path.lastIndexOf('/');
    return i < 0 ? '' : path.slice(0, i);
  }

  nameOf(path: string): string {
    const i = path.lastIndexOf('/');
    return i < 0 ? path : path.slice(i + 1);
  }

  onKeydown(event: KeyboardEvent): void {
    const n = this.rows().length;
    switch (event.key) {
      case 'Escape':
        event.preventDefault();
        event.stopPropagation();
        this.close();
        return;
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
    const mod = event.ctrlKey || event.metaKey;
    if (event.altKey && !mod) {
      const k = event.key.toLowerCase();
      if (k === 'c') {
        event.preventDefault();
        this.caseSensitive.update((v) => !v);
      } else if (k === 'w') {
        event.preventDefault();
        this.wholeWord.update((v) => !v);
      } else if (k === 'x') {
        event.preventDefault();
        this.regex.update((v) => !v);
      }
    }
  }

  private reveal(): void {
    queueMicrotask(() => {
      const el = this.listEl()?.nativeElement.querySelector('.fif-row.is-selected');
      (el as HTMLElement | null)?.scrollIntoView({ block: 'nearest' });
    });
  }
}
