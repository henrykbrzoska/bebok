/**
 * Directory browser modal (F5-5): the engine-backed folder picker used in
 * browser and Capacitor mode, where no native dialog can produce a filesystem
 * path (see `core/directory-picker.service.ts` for why).
 *
 * Always mounted next to the command palette; it only paints while
 * `DirectoryPicker.request()` is set. Same overlay pattern as the palette:
 * dim backdrop, centered card, Escape/backdrop-click cancels.
 */

import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from '@angular/core';
import { FormsModule } from '@angular/forms';

import { DirectoryPicker } from '../../core/directory-picker.service';
import { EngineClient } from '../../core/engine-client.service';
import { FsBrowseEntry } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';

@Component({
  selector: 'app-directory-browser',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  templateUrl: './directory-browser.html',
  styleUrl: './directory-browser.css',
  host: {
    '(document:keydown)': 'onDocumentKeydown($event)',
  },
})
export class DirectoryBrowser {
  private readonly picker = inject(DirectoryPicker);
  private readonly engine = inject(EngineClient);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);
  readonly request = this.picker.request;
  readonly open = computed(() => this.request() !== null);

  /** The directory currently listed; null means "showing the host roots". */
  readonly currentPath = signal<string | null>(null);
  readonly entries = signal<FsBrowseEntry[]>([]);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);
  readonly filter = signal('');
  readonly showHidden = signal(false);
  /** Free-text path box, validated by attempting to list it. */
  readonly manualPath = signal('');
  readonly highlighted = signal(0);
  private loadVersion = 0;

  readonly visible = computed<FsBrowseEntry[]>(() => {
    const needle = this.filter().trim().toLowerCase();
    const rows = this.entries();
    return needle ? rows.filter((e) => e.name.toLowerCase().includes(needle)) : rows;
  });

  /** Breadcrumbs also drive the parent-directory action, so keep parsing in one tested helper. */
  readonly crumbs = computed(() => pathCrumbs(this.currentPath()));

  constructor() {
    effect(() => {
      if (this.open()) {
        this.filter.set('');
        this.error.set(null);
        void this.load(null);
      }
    });
  }

  onDocumentKeydown(event: KeyboardEvent): void {
    if (!this.open()) {
      return;
    }
    switch (event.key) {
      case 'Escape':
        event.preventDefault();
        this.cancel();
        return;
      case 'ArrowDown':
        event.preventDefault();
        this.move(1);
        return;
      case 'ArrowUp':
        event.preventDefault();
        this.move(-1);
        return;
      case 'Backspace':
        // Only when not typing into the filter/path boxes.
        if ((event.target as HTMLElement | null)?.tagName !== 'INPUT') {
          event.preventDefault();
          void this.up();
        }
        return;
      case 'Enter':
        if ((event.target as HTMLElement | null)?.tagName !== 'INPUT') {
          event.preventDefault();
          const row = this.visible()[this.highlighted()];
          if (row) {
            void this.enter(row);
          }
        }
        return;
      default:
    }
  }

  /** List a directory (null lists the host roots). */
  async load(path: string | null): Promise<void> {
    const version = ++this.loadVersion;
    this.loading.set(true);
    this.error.set(null);
    try {
      const response = await this.engine.browseDirectory(path, this.showHidden());
      if (version !== this.loadVersion) {
        return;
      }
      this.currentPath.set(response.path);
      this.entries.set(response.entries);
      this.manualPath.set(response.path ?? '');
      this.highlighted.set(0);
      this.filter.set('');
    } catch (err) {
      if (version === this.loadVersion) {
        this.error.set(err instanceof Error ? err.message : String(err));
      }
    } finally {
      if (version === this.loadVersion) {
        this.loading.set(false);
      }
    }
  }

  async enter(entry: FsBrowseEntry): Promise<void> {
    if (!entry.readable) {
      this.error.set(this.t('browser.unreadable'));
      return;
    }
    await this.load(entry.path);
  }

  /** Up one level: to the parent crumb, or back to the roots list. */
  async up(): Promise<void> {
    const crumbs = this.crumbs();
    if (crumbs.length <= 1) {
      await this.load(null);
      return;
    }
    await this.load(crumbs[crumbs.length - 2].path);
  }

  /** Navigate to a typed path; an invalid one shows an inline error instead. */
  async goManual(): Promise<void> {
    const value = this.manualPath().trim();
    if (!value) {
      await this.load(null);
      return;
    }
    const before = this.currentPath();
    await this.load(value);
    // Stay where we were and explain, rather than navigating nowhere.
    if (this.error()) {
      this.currentPath.set(before);
      this.error.set(this.t('browser.invalidPath'));
    }
  }

  async toggleHidden(): Promise<void> {
    this.showHidden.update((on) => !on);
    await this.load(this.currentPath());
  }

  /** Confirm the directory currently being browsed (not a child row). */
  select(): void {
    const path = this.currentPath();
    if (path) {
      this.picker.resolve(path);
    }
  }

  cancel(): void {
    this.picker.resolve(null);
  }

  private move(delta: number): void {
    const rows = this.visible();
    if (rows.length === 0) {
      return;
    }
    const next = Math.min(Math.max(this.highlighted() + delta, 0), rows.length - 1);
    this.highlighted.set(next);
  }
}

export interface PathCrumb {
  label: string;
  path: string;
}

/**
 * Turn an engine-normalised absolute path into navigable ancestors. Windows
 * UNC shares need special handling: `\\server\\share` is their root, while a
 * drive root and Unix `/` are each a single root segment.
 */
export function pathCrumbs(path: string | null): PathCrumb[] {
  if (!path) {
    return [];
  }

  if (path.startsWith('\\\\')) {
    const parts = path.slice(2).split('\\').filter(Boolean);
    if (parts.length < 2) {
      return [];
    }
    const share = `\\\\${parts[0]}\\${parts[1]}`;
    const crumbs: PathCrumb[] = [{ label: share, path: share }];
    let accumulated = share;
    for (const part of parts.slice(2)) {
      accumulated = `${accumulated}\\${part}`;
      crumbs.push({ label: part, path: accumulated });
    }
    return crumbs;
  }

  if (path.startsWith('/')) {
    const crumbs: PathCrumb[] = [{ label: '/', path: '/' }];
    let accumulated = '';
    for (const part of path.split('/').filter(Boolean)) {
      accumulated = `${accumulated}/${part}`;
      crumbs.push({ label: part, path: accumulated });
    }
    return crumbs;
  }

  const parts = path.split('\\').filter(Boolean);
  const crumbs: PathCrumb[] = [];
  let accumulated = '';
  parts.forEach((part, index) => {
    accumulated = index === 0 ? part : `${accumulated}\\${part}`;
    const target = /^[A-Za-z]:$/.test(accumulated) ? `${accumulated}\\` : accumulated;
    crumbs.push({ label: part, path: target });
  });
  return crumbs;
}
