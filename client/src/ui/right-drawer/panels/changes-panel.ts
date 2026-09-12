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
            @for (file of changes(); track file.path) {
              <li>
                <button
                  type="button"
                  class="file"
                  [class.deleted]="!file.exists"
                  (click)="open(file)"
                  [title]="file.path"
                  data-testid="change-row"
                >
                  <span class="file-path">{{ file.path }}</span>
                  <span class="plus">+{{ file.added }}</span>
                  <span class="minus">-{{ file.removed }}</span>
                </button>
              </li>
            }
          </ul>
        }
      </div>

      @if (openPath(); as path) {
        <app-diff-overlay
          [sessionId]="sessionId()!"
          [path]="path"
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

      .file-path {
        flex: 1 1 auto;
        min-width: 0;
        color: var(--code-text-strong);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
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

  open(file: ChangeEntry): void {
    this.openPath.set(file.path);
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
