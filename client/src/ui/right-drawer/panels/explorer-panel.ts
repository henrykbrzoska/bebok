/**
 * Right-drawer "Explorer" panel (F2-13).
 *
 * A compact version of the full-screen Explorer tree: the same lazy,
 * gitignore-aware `GET /fs/tree` walk, rendered at drawer width. The selected
 * file is kept in `ExplorerSelectionStore` so the full-screen Explorer and this
 * mini tree stay on the same file instead of forking the state.
 */

import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from '@angular/core';
import { RouterLink } from '@angular/router';

import { EngineClient } from '../../../core/engine-client.service';
import { FsEntry } from '../../../core/engine.dtos';
import { I18nService } from '../../../i18n/i18n.service';
import { ChatSessionStore } from '../../../views/chat/chat-session.store';
import { ExplorerSelectionStore } from './explorer-selection.store';

interface FsNode {
  name: string;
  path: string;
  is_dir: boolean;
  depth: number;
  expanded: boolean;
}

interface DirState {
  entries: FsEntry[];
  loaded: boolean;
  expanded: boolean;
}

@Component({
  selector: 'app-explorer-panel',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  template: `
    @if (!directory()) {
      <div class="empty">{{ t('drawer.noDirectory') }}</div>
    } @else {
      <div class="panel-body">
        <div class="head">
          <span class="dir" [title]="directory()!">{{ directory() }}</span>
          <a
            class="open"
            [routerLink]="['/explorer']"
            [queryParams]="{ directory: directory() }"
            [title]="t('drawer.openExplorer')"
          >↗</a>
        </div>

        @if (error()) {
          <div class="error">{{ error() }}</div>
        } @else if (loading() && rows().length === 0) {
          <div class="empty">{{ t('drawer.loadingTree') }}</div>
        } @else if (rows().length === 0) {
          <div class="empty">{{ t('drawer.emptyTree') }}</div>
        } @else {
          <ul class="tree">
            @for (node of rows(); track node.path) {
              <li>
                <button
                  type="button"
                  class="row"
                  [class.selected]="!node.is_dir && selected() === node.path"
                  [style.padding-left.px]="8 + node.depth * 12"
                  (click)="activate(node)"
                  [title]="node.path"
                  [attr.aria-expanded]="node.is_dir ? node.expanded : null"
                >
                  <span class="glyph" aria-hidden="true">{{
                    node.is_dir ? (node.expanded ? '▾' : '▸') : '·'
                  }}</span>
                  <span class="name" [class.dir]="node.is_dir">{{ node.name }}</span>
                </button>
              </li>
            }
          </ul>
        }
      </div>
    }
  `,
  styles: [
    `
      .empty,
      .error {
        padding: var(--space-12) var(--space-14);
        font-size: var(--fs-11-5);
        color: var(--text-faint);
      }

      .error {
        color: var(--danger);
        overflow-wrap: anywhere;
      }

      .panel-body {
        display: flex;
        flex-direction: column;
        padding-bottom: var(--space-8);
      }

      .head {
        display: flex;
        align-items: center;
        gap: var(--space-6);
        padding: var(--space-6) var(--space-10);
        border-bottom: 1px solid var(--border);
      }

      .dir {
        flex: 1 1 auto;
        min-width: 0;
        font-family: var(--font-mono);
        font-size: 10.5px;
        color: var(--text-faint);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        direction: rtl;
        text-align: left;
      }

      .open {
        flex: none;
        color: var(--text-muted);
        text-decoration: none;
        font-size: var(--fs-12);
        padding: 0 2px;
      }

      .open:hover {
        color: var(--accent);
      }

      .tree {
        list-style: none;
        margin: 0;
        padding: var(--space-6) 0 0;
        max-height: 320px;
        overflow-y: auto;
      }

      .row {
        display: flex;
        align-items: center;
        gap: 5px;
        width: 100%;
        padding: 2px var(--space-8);
        border: 1px solid transparent;
        border-radius: 0;
        background: transparent;
        text-align: left;
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: var(--fs-11-5);
        min-width: 0;
      }

      .row:hover {
        background: var(--surface-2);
      }

      .row.selected {
        background: var(--surface-3);
        border-color: var(--border-strong);
        color: var(--text);
      }

      .glyph {
        flex: none;
        width: 10px;
        font-size: 9px;
        color: var(--text-faint);
      }

      .name {
        flex: 1 1 auto;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .name.dir {
        color: var(--text);
      }
    `,
  ],
})
export class ExplorerPanel {
  private readonly i18n = inject(I18nService);
  private readonly engine = inject(EngineClient);
  private readonly session = inject(ChatSessionStore);
  private readonly selection = inject(ExplorerSelectionStore);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly directory = computed(
    () => this.session.directory() ?? this.engine.readLastDirectory(),
  );
  readonly selected = this.selection.selectedPath;
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  /** Directory state keyed by path ('' = root). */
  private readonly dirs = signal<Record<string, DirState>>({});

  readonly rows = computed<FsNode[]>(() => {
    const dirs = this.dirs();
    const out: FsNode[] = [];
    flatten(dirs, '', 0, out);
    return out;
  });

  private loadedDirectory: string | null = null;

  constructor() {
    effect(() => {
      const dir = this.directory();
      if (dir && dir !== this.loadedDirectory) {
        this.loadedDirectory = dir;
        this.dirs.set({});
        void this.loadRoot(dir);
      }
    });
  }

  /** Folders expand/collapse (lazy-loading children); files get selected. */
  async activate(node: FsNode): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    if (!node.is_dir) {
      this.selection.select(dir, node.path);
      return;
    }
    const state = this.dirs()[node.path];
    if (state?.loaded) {
      this.dirs.update((d) => ({ ...d, [node.path]: { ...state, expanded: !state.expanded } }));
      return;
    }
    try {
      const res = await this.engine.fsTree(dir, node.path);
      this.dirs.update((d) => ({
        ...d,
        [node.path]: { entries: res.entries, loaded: true, expanded: true },
      }));
    } catch (err) {
      this.error.set(describe(err));
    }
  }

  private async loadRoot(dir: string): Promise<void> {
    this.loading.set(true);
    this.error.set(null);
    try {
      const res = await this.engine.fsTree(dir);
      if (this.directory() !== dir) {
        return;
      }
      this.dirs.set({ '': { entries: res.entries, loaded: true, expanded: true } });
    } catch (err) {
      this.error.set(describe(err));
    } finally {
      this.loading.set(false);
    }
  }
}

function flatten(
  dirs: Record<string, DirState>,
  path: string,
  depth: number,
  out: FsNode[],
): void {
  const state = dirs[path];
  if (!state || !state.expanded) {
    return;
  }
  for (const child of state.entries) {
    const expanded = dirs[child.path]?.expanded === true;
    out.push({
      name: child.name,
      path: child.path,
      is_dir: child.is_dir,
      depth,
      expanded,
    });
    if (child.is_dir && expanded) {
      flatten(dirs, child.path, depth + 1, out);
    }
  }
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
