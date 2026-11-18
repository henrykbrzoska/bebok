/**
 * Explorer navigation bar - IntelliJ-style breadcrumbs of the open file
 * (`project › src › app › file.ts`).
 *
 * Clicking a directory segment drops down its contents; clicking the file
 * segment lists its siblings. In the popup: Up/Down move, Right/Enter on a
 * directory goes into it, Left goes up, Enter on a file opens it, typing
 * filters (speed search), Esc closes. Alt+Home in Explorer focuses the bar
 * (`openAt(-1)`, the file's own directory).
 */

import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  computed,
  inject,
  input,
  output,
  signal,
  viewChild,
} from '@angular/core';

import { EngineClient } from '../../core/engine-client.service';
import { FsEntry } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';
import { fuzzyMatch, highlightRuns } from './goto-match';

export interface NavSegment {
  /** Label shown in the bar. */
  name: string;
  /** Project-relative path (`''` = project root). */
  path: string;
  isDir: boolean;
}

/** Breadcrumb segments for `path` inside the project called `rootName`. */
export function navSegments(rootName: string, path: string | null): NavSegment[] {
  const out: NavSegment[] = [{ name: rootName, path: '', isDir: true }];
  if (!path) {
    return out;
  }
  const parts = path.split(/[\\/]/).filter((s) => s.length > 0);
  let acc = '';
  parts.forEach((part, i) => {
    acc = acc ? `${acc}/${part}` : part;
    out.push({ name: part, path: acc, isDir: i < parts.length - 1 });
  });
  return out;
}

function parentOf(path: string): string {
  const i = path.lastIndexOf('/');
  return i < 0 ? '' : path.slice(0, i);
}

@Component({
  selector: 'app-explorer-nav-bar',
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './nav-bar.html',
  styleUrl: './nav-bar.css',
  host: {
    '(document:mousedown)': 'onDocumentMousedown($event)',
  },
})
export class ExplorerNavBarComponent {
  private readonly engine = inject(EngineClient);
  private readonly i18n = inject(I18nService);
  private readonly host = inject<ElementRef<HTMLElement>>(ElementRef);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly directory = input.required<string>();
  /** Open file (project-relative) or `null`. */
  readonly path = input<string | null>(null);
  readonly dirty = input(false);

  /** A file was picked in a popup. */
  readonly opened = output<string>();
  /** "Reveal in tree" for a segment (double-click). */
  readonly revealed = output<string>();
  /** The bar's "Go to file" button. */
  readonly searchRequested = output<void>();

  readonly rootName = computed(() => {
    const d = this.directory().replace(/[\\/]+$/, '');
    return d.split(/[\\/]/).pop() || d;
  });

  readonly segments = computed(() => navSegments(this.rootName(), this.path()));

  /** Index of the segment whose popup is open, `null` when closed. */
  readonly openIndex = signal<number | null>(null);
  /** Directory listed by the popup (drilling moves it). */
  readonly popupDir = signal('');
  readonly entries = signal<FsEntry[] | null>(null);
  readonly popupIndex = signal(0);
  readonly filter = signal('');
  readonly popupLeft = signal(0);
  readonly error = signal<string | null>(null);

  private readonly popupEl = viewChild<ElementRef<HTMLElement>>('popup');
  private loadSeq = 0;

  readonly visibleEntries = computed(() => {
    const list = this.entries() ?? [];
    const f = this.filter();
    if (!f) {
      return list.map((e) => ({ entry: e, runs: [{ text: e.name, hit: false }] }));
    }
    const out: { entry: FsEntry; runs: { text: string; hit: boolean }[]; score: number }[] = [];
    for (const e of list) {
      const m = fuzzyMatch(f, e.name);
      if (m) {
        out.push({ entry: e, runs: highlightRuns(e.name, m.indices), score: m.score });
      }
    }
    return out.sort((a, b) => b.score - a.score);
  });

  readonly popupTitle = computed(() => this.popupDir() || this.rootName());

  /** Toggle the popup under segment `i` (`-1` = the open file's directory). */
  openAt(i: number, anchor?: HTMLElement): void {
    const segs = this.segments();
    const idx = i < 0 ? Math.max(0, segs.length - (segs[segs.length - 1].isDir ? 1 : 2)) : i;
    if (this.openIndex() === idx && i >= 0) {
      this.close();
      return;
    }
    const seg = segs[idx];
    if (!seg) {
      return;
    }
    const el =
      anchor ??
      (this.host.nativeElement.querySelectorAll('.nav-seg')[idx] as HTMLElement | undefined);
    const barLeft = this.host.nativeElement.getBoundingClientRect().left;
    this.popupLeft.set(el ? el.getBoundingClientRect().left - barLeft : 0);
    this.openIndex.set(idx);
    // A file segment lists its siblings (the file pre-selected).
    const dir = seg.isDir ? seg.path : parentOf(seg.path);
    const select = seg.isDir ? (segs[idx + 1]?.path ?? null) : seg.path;
    void this.load(dir, select);
  }

  close(): void {
    this.openIndex.set(null);
    this.entries.set(null);
    this.filter.set('');
    this.error.set(null);
  }

  private async load(dir: string, select: string | null): Promise<void> {
    const seq = ++this.loadSeq;
    this.popupDir.set(dir);
    this.filter.set('');
    this.entries.set(null);
    this.error.set(null);
    try {
      const res = await this.engine.fsTree(this.directory(), dir || undefined);
      if (seq !== this.loadSeq) {
        return;
      }
      this.entries.set(res.entries);
      const at = select ? res.entries.findIndex((e) => e.path === select) : -1;
      this.popupIndex.set(Math.max(0, at));
      queueMicrotask(() => {
        this.popupEl()?.nativeElement.focus();
        this.reveal();
      });
    } catch (err) {
      if (seq === this.loadSeq) {
        this.entries.set([]);
        this.error.set(err instanceof Error ? err.message : String(err));
      }
    }
  }

  pick(entry: FsEntry): void {
    if (entry.is_dir) {
      void this.load(entry.path, null);
      return;
    }
    this.close();
    this.opened.emit(entry.path);
  }

  up(): void {
    const dir = this.popupDir();
    if (dir) {
      void this.load(parentOf(dir), dir);
    }
  }

  onPopupKeydown(event: KeyboardEvent): void {
    const list = this.visibleEntries();
    const n = list.length;
    const current = list[this.popupIndex()]?.entry;
    const move = (to: number) => {
      event.preventDefault();
      if (n) {
        this.popupIndex.set(Math.min(n - 1, Math.max(0, to)));
        this.reveal();
      }
    };
    switch (event.key) {
      case 'Escape':
        event.preventDefault();
        event.stopPropagation();
        if (this.filter()) {
          this.filter.set('');
          this.popupIndex.set(0);
        } else {
          this.close();
        }
        return;
      case 'ArrowDown':
        return move(n ? (this.popupIndex() + 1) % n : 0);
      case 'ArrowUp':
        return move(n ? (this.popupIndex() - 1 + n) % n : 0);
      case 'PageDown':
        return move(this.popupIndex() + 10);
      case 'PageUp':
        return move(this.popupIndex() - 10);
      case 'Home':
        return move(0);
      case 'End':
        return move(n - 1);
      case 'Enter':
        event.preventDefault();
        if (current) {
          this.pick(current);
        }
        return;
      case 'ArrowRight':
        event.preventDefault();
        if (current?.is_dir) {
          this.pick(current);
        }
        return;
      case 'ArrowLeft':
        event.preventDefault();
        this.up();
        return;
      case 'Backspace':
        event.preventDefault();
        if (this.filter()) {
          this.filter.update((f) => f.slice(0, -1));
          this.popupIndex.set(0);
        } else {
          this.up();
        }
        return;
    }
    if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
      event.preventDefault();
      this.filter.update((f) => f + event.key);
      this.popupIndex.set(0);
    }
  }

  onDocumentMousedown(event: MouseEvent): void {
    if (this.openIndex() === null) {
      return;
    }
    if (!this.host.nativeElement.contains(event.target as Node)) {
      this.close();
    }
  }

  private reveal(): void {
    queueMicrotask(() => {
      const el = this.popupEl()?.nativeElement.querySelector('.nav-item.is-selected');
      (el as HTMLElement | null)?.scrollIntoView({ block: 'nearest' });
    });
  }
}
