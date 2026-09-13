/**
 * Right-drawer "Changes" panel (WP-CHANGES / F6-9).
 *
 * Lists the files the engine tracked for the open session
 * (`GET /session/{id}/changes`: real first-write snapshots, `+`/`-` line
 * counts against the git `HEAD` or snapshot baseline) - the replacement for
 * the old client-side "FILES CHANGED" heuristic. Clicking a row opens the
 * `<app-diff-overlay>` with the real unified diff; a revert from the overlay
 * re-lists. The list refreshes whenever the turn settles and, while a turn is
 * running, on each transcript update (debounced).
 *
 * F9-6: the engine list now includes the changes of descendant (sub-agent)
 * sessions; rows are grouped under a small header per agent (label + count,
 * main session first) and the overlay is told which session's tracker to
 * diff/revert against.
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  signal,
} from '@angular/core';

import { EngineClient } from '../../../core/engine-client.service';
import { ChangeEntry, EngineEvent } from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { I18nService } from '../../../i18n/i18n.service';
import { ChatSessionStore } from '../../../views/chat/chat-session.store';
import { DiffOverlay } from '../../diff-overlay/diff-overlay';

const REFRESH_DEBOUNCE_MS = 400;

/** F9-6: one agent's rows in the panel (`main` first, then children in spawn order). */
export interface ChangeGroup {
  /** `sessionID` of the rows (`''` for rows from an engine without the field). */
  sessionID: string;
  /** Agent label: the child's alias, or the main session's agent name. */
  label: string;
  isChild: boolean;
  rows: ChangeEntry[];
}

/**
 * Group the engine list by owning session, keeping the engine order (main
 * session first, then children in spawn order; paths sorted within). Rows
 * lacking the F9-6 fields all land in one unlabeled group.
 */
export function groupChanges(changes: readonly ChangeEntry[]): ChangeGroup[] {
  const groups: ChangeGroup[] = [];
  const byKey = new Map<string, ChangeGroup>();
  for (const row of changes) {
    const key = row.sessionID ?? '';
    let group = byKey.get(key);
    if (!group) {
      group = {
        sessionID: key,
        label: row.agent ?? '',
        isChild: row.isChild === true,
        rows: [],
      };
      byKey.set(key, group);
      groups.push(group);
    }
    group.rows.push(row);
  }
  // Main session first even if the engine sent children ahead of it.
  groups.sort((a, b) => Number(a.isChild) - Number(b.isChild));
  return groups;
}

@Component({
  selector: 'app-changes-panel',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [DiffOverlay],
  template: `
    @if (!sessionId()) {
      <div class="empty">{{ t('drawer.noSession') }}</div>
    } @else {
      <div class="panel-body">
        <div class="head">
          <span class="count">{{ t('changes.count', { n: changes().length }) }}</span>
          <button
            type="button"
            class="refresh"
            (click)="refresh()"
            [disabled]="loading()"
            [title]="t('changes.refresh')"
            [attr.aria-label]="t('changes.refresh')"
          >
            ↻
          </button>
        </div>

        @if (error()) {
          <div class="error">{{ error() }}</div>
        } @else if (changes().length === 0) {
          <div class="none">{{ loading() ? t('changes.loading') : t('changes.none') }}</div>
        } @else {
          <ul class="files">
            @for (group of groups(); track group.sessionID) {
              @if (showGroupHeaders()) {
                <li
                  class="group-head"
                  [class.child]="group.isChild"
                  data-testid="change-group"
                  [attr.data-session]="group.sessionID"
                >
                  <span class="group-label">{{ groupLabel(group) }}</span>
                  <span class="group-count">{{ group.rows.length }}</span>
                </li>
              }
              @for (file of group.rows; track file.path) {
                <li>
                  <button
                    type="button"
                    class="file"
                    [class.deleted]="!file.exists"
                    (click)="open(file)"
                    [title]="rowTitle(file)"
                    data-testid="change-row"
                    [attr.data-session]="file.sessionID ?? null"
                  >
                    <span class="file-path"
                      ><span class="file-dir">{{ dirOf(file.path) }}</span
                      ><span class="file-name">{{ nameOf(file.path) }}</span></span
                    >
                    <span class="plus">+{{ file.added }}</span>
                    <span class="minus">-{{ file.removed }}</span>
                  </button>
                </li>
              }
            }
          </ul>
        }
      </div>

      @if (openPath(); as path) {
        <app-diff-overlay
          [sessionId]="sessionId()!"
          [path]="path"
          [session]="openSession()"
          [directory]="directory()"
          (closed)="openPath.set(null)"
          (reverted)="refresh()"
        />
      }
    }
  `,
  styles: [
    `
      .empty,
      .none,
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
        justify-content: space-between;
        gap: var(--space-6);
        padding: var(--space-6) var(--space-10);
        border-bottom: 1px solid var(--border);
      }

      .count {
        font-size: var(--fs-label);
        text-transform: uppercase;
        letter-spacing: var(--label-tracking);
        color: var(--text-faint);
      }

      .refresh {
        border: none;
        background: transparent;
        color: var(--text-muted);
        font-size: var(--fs-13);
        padding: 0 2px;
        cursor: pointer;
      }

      .refresh:hover:not(:disabled) {
        color: var(--accent);
      }

      .refresh:disabled {
        opacity: 0.5;
        cursor: default;
      }

      .files {
        list-style: none;
        margin: 0;
        padding: var(--space-6) 0 0;
        display: flex;
        flex-direction: column;
        gap: 1px;
      }

      /* F9-6: per-agent group header ("main 3", "api-orders 2"). */
      .group-head {
        display: flex;
        align-items: baseline;
        gap: var(--space-6);
        padding: var(--space-6) var(--space-10) 2px;
        font-size: var(--fs-label);
        text-transform: uppercase;
        letter-spacing: var(--label-tracking);
        color: var(--text-faint);
      }

      .group-head.child .group-label {
        color: var(--accent);
        text-transform: none;
        letter-spacing: 0;
        font-family: var(--font-mono);
      }

      .group-label {
        flex: 1 1 auto;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .group-count {
        flex: none;
        font-family: var(--font-mono);
      }

      .file {
        display: flex;
        align-items: baseline;
        gap: var(--space-6);
        width: 100%;
        padding: 3px var(--space-10);
        border: 1px solid transparent;
        border-radius: 0;
        background: transparent;
        text-align: left;
        font-family: var(--font-mono);
        font-size: var(--fs-11);
        color: var(--text-muted);
        min-width: 0;
        cursor: pointer;
      }

      .file:hover {
        background: var(--surface-2);
      }

      .file.deleted .file-path {
        text-decoration: line-through;
        color: var(--text-faint);
      }

      /* E2E R10: the directory part truncates, the basename never does, so
         two files in the same deep folder stay distinguishable. */
      .file-path {
        flex: 1 1 auto;
        min-width: 0;
        display: flex;
        color: var(--code-text-strong);
        white-space: nowrap;
      }

      .file-dir {
        flex: 0 1 auto;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        color: var(--text-muted);
      }

      .file-name {
        flex: 0 0 auto;
      }

      .plus {
        color: var(--diff-add-text);
        flex: none;
      }

      .minus {
        color: var(--diff-remove-text);
        flex: none;
      }
    `,
  ],
})
export class ChangesPanel {
  private readonly i18n = inject(I18nService);
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly destroyRef = inject(DestroyRef);
  private readonly session = inject(ChatSessionStore);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly sessionId = computed(() => this.session.meta()?.id ?? null);
  readonly directory = computed(() => this.session.directory());
  readonly changes = signal<ChangeEntry[]>([]);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);
  /** Path currently shown in the diff overlay (null = closed). */
  readonly openPath = signal<string | null>(null);
  /** F9-6: session that owns the change shown in the overlay (undefined = let the engine search). */
  readonly openSession = signal<string | undefined>(undefined);

  /** F9-6: rows grouped by owning session/agent, main first. */
  readonly groups = computed(() => groupChanges(this.changes()));
  /** Headers only earn their space once a child session contributed a change. */
  readonly showGroupHeaders = computed(() => this.groups().some((g) => g.isChild));

  private loadedFor: string | null = null;
  private debounce: ReturnType<typeof setTimeout> | null = null;

  constructor() {
    effect(() => {
      const id = this.sessionId();
      // Re-read when the session changes and whenever a turn starts/settles.
      this.session.running();
      if (!id) {
        this.loadedFor = null;
        this.changes.set([]);
        this.openPath.set(null);
        return;
      }
      if (this.loadedFor !== id) {
        this.loadedFor = id;
        this.changes.set([]);
        this.openPath.set(null);
      }
      void this.load(id);
    });
    const unsubscribe = this.events.onEvent((ev) => this.handleEvent(ev));
    this.destroyRef.onDestroy(() => {
      unsubscribe();
      if (this.debounce) {
        clearTimeout(this.debounce);
      }
    });
  }

  refresh(): void {
    const id = this.sessionId();
    if (id) {
      void this.load(id);
    }
  }

  /** Directory part of a path including the trailing `/` ('' for a root file). */
  dirOf(path: string): string {
    const i = path.lastIndexOf('/');
    return i < 0 ? '' : path.slice(0, i + 1);
  }

  /** Basename of a path. */
  nameOf(path: string): string {
    return path.slice(path.lastIndexOf('/') + 1);
  }

  open(file: ChangeEntry): void {
    this.openSession.set(file.sessionID || undefined);
    this.openPath.set(file.path);
  }

  /** "main" for the main session, the alias for a child. */
  groupLabel(group: ChangeGroup): string {
    if (group.label) {
      return group.label;
    }
    return group.isChild ? group.sessionID.slice(0, 8) : this.t('changes.agentMain');
  }

  /** Row tooltip: the path, plus the agent that changed it when known. */
  rowTitle(file: ChangeEntry): string {
    return file.agent
      ? `${file.path} · ${this.t('changes.byAgent', { agent: file.agent })}`
      : file.path;
  }

  /** Tool writes land mid-turn: re-list on transcript updates, debounced. */
  private handleEvent(ev: EngineEvent): void {
    const id = this.sessionId();
    if (!id || ev.sessionID !== id || !this.session.running()) {
      return;
    }
    if (ev.type !== 'message.part.updated' && ev.type !== 'message.updated') {
      return;
    }
    if (this.debounce) {
      clearTimeout(this.debounce);
    }
    this.debounce = setTimeout(() => {
      this.debounce = null;
      void this.load(id);
    }, REFRESH_DEBOUNCE_MS);
  }

  private async load(id: string): Promise<void> {
    if (!this.engine.connected()) {
      return;
    }
    this.loading.set(true);
    try {
      const list = await this.engine.sessionChanges(id);
      if (this.sessionId() !== id) {
        return;
      }
      this.changes.set(list);
      this.error.set(null);
    } catch (err) {
      if (this.sessionId() === id) {
        this.error.set(describe(err));
      }
    } finally {
      this.loading.set(false);
    }
  }
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
