/**
 * Remote session list (WP-M6 / F10-24): the desktop's sessions, newest
 * first, optionally filtered by project.
 *
 * `GET /session` needs a `directory`, so the list is the union of
 * `GET /session?directory=` over `GET /projects` (both allow-listed for the
 * remote scope). It re-lists on `session.*` events and on every SSE
 * (re)connect, and falls back to the offline cache (F10-27) when the desktop
 * cannot be reached - then tagged as cached. Used by the Remote tab as the
 * main screen and by the Changes / Agents tabs as a session picker.
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
  untracked,
} from '@angular/core';

import { EngineClient } from '../../../core/engine-client.service';
import { EngineTargetStore } from '../../../core/engine-target.store';
import { EngineEvent, ProjectEntry, SessionMeta } from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { OfflineCache } from '../../../core/remote/offline-cache';
import { I18nService } from '../../../i18n/i18n.service';

const REFRESH_DEBOUNCE_MS = 300;
/** Projects listed in parallel (the desktop registry is small; keep the fan-out bounded). */
const MAX_PROJECTS = 24;

export interface SessionRow {
  session: SessionMeta;
  projectName: string;
}

/** `5m` / `3h` / `2d` / `Mar 3`. */
export function relativeTime(at: number, now: number = Date.now()): string {
  const diff = Math.max(0, now - at);
  const minutes = Math.floor(diff / 60_000);
  if (minutes < 1) {
    return 'now';
  }
  if (minutes < 60) {
    return `${minutes}m`;
  }
  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    return `${hours}h`;
  }
  const days = Math.floor(hours / 24);
  if (days < 14) {
    return `${days}d`;
  }
  return new Date(at).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

@Component({
  selector: 'app-remote-session-list',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="list" data-testid="remote-session-list" [attr.data-state]="state()">
      @if (projects().length > 1) {
        <div class="chips" role="tablist" [attr.aria-label]="t('mobile.remote.filterProjects')">
          <button
            type="button"
            class="chip"
            role="tab"
            [class.on]="filter() === null"
            [attr.aria-selected]="filter() === null"
            (click)="filter.set(null)"
            data-testid="remote-filter-all"
          >
            {{ t('mobile.remote.allProjects') }}
          </button>
          @for (project of projects(); track project.id) {
            <button
              type="button"
              class="chip"
              role="tab"
              [class.on]="filter() === project.path"
              [attr.aria-selected]="filter() === project.path"
              (click)="filter.set(project.path)"
              [attr.data-testid]="'remote-filter-' + project.id"
            >
              {{ project.name }}
            </button>
          }
        </div>
      }

      @if (fromCache()) {
        <p class="cached" role="status" data-testid="remote-list-cached">
          {{ t('mobile.remote.cachedList', { when: relative(cachedAt()) }) }}
        </p>
      }

      @if (error(); as err) {
        <p class="error" role="alert" data-testid="remote-list-error">{{ err }}</p>
      }

      @if (loading() && rows().length === 0) {
        <p class="empty" data-testid="remote-list-loading">{{ t('mobile.remote.loading') }}</p>
      } @else if (rows().length === 0) {
        <p class="empty" data-testid="remote-list-empty">
          {{ error() ? t('mobile.remote.noneOffline') : t('mobile.remote.none') }}
        </p>
      } @else {
        <ul class="sessions">
          @for (row of rows(); track row.session.id) {
            <li>
              <button
                type="button"
                class="session"
                [class.running]="row.session.running"
                [class.current]="row.session.id === current()"
                (click)="open.emit(row.session.id)"
                [attr.data-testid]="'remote-session-' + row.session.id"
                [attr.aria-current]="row.session.id === current() ? 'true' : null"
              >
                <span class="dot" aria-hidden="true"></span>
                <span class="main">
                  <span class="title">{{ titleOf(row.session) }}</span>
                  <span class="meta">
                    <span class="project">{{ row.projectName }}</span>
                    @if (row.session.agent) {
                      <span class="agent">· {{ row.session.agent }}</span>
                    }
                  </span>
                </span>
                <span class="when">{{ relative(row.session.updated_at) }}</span>
              </button>
            </li>
          }
        </ul>
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: block;
      }

      .list {
        display: flex;
        flex-direction: column;
      }

      .chips {
        display: flex;
        gap: var(--space-6);
        padding: var(--space-8) var(--space-12);
        overflow-x: auto;
        scrollbar-width: none;
        border-bottom: 1px solid var(--border);
      }

      .chips::-webkit-scrollbar {
        display: none;
      }

      .chip {
        flex: none;
        min-height: 32px;
        padding: 0 var(--space-12);
        border: 1px solid var(--border-strong);
        border-radius: 999px;
        background: transparent;
        color: var(--text-muted);
        font: inherit;
        font-size: var(--fs-12);
        white-space: nowrap;
        cursor: pointer;
      }

      .chip.on {
        border-color: var(--accent);
        color: var(--accent);
      }

      .cached,
      .error,
      .empty {
        margin: 0;
        padding: var(--space-10) var(--space-16);
        font-size: var(--fs-12);
        color: var(--text-muted);
      }

      .error {
        color: var(--danger);
        overflow-wrap: anywhere;
      }

      .empty {
        padding: var(--space-24) var(--space-16);
        text-align: center;
      }

      .cached {
        background: var(--surface);
        border-bottom: 1px solid var(--border);
        color: var(--warning);
      }

      .sessions {
        list-style: none;
        margin: 0;
        padding: 0;
      }

      .session {
        display: flex;
        align-items: center;
        gap: var(--space-10);
        width: 100%;
        min-height: 56px;
        padding: var(--space-8) var(--space-16);
        border: 0;
        border-bottom: 1px solid var(--border);
        background: transparent;
        color: var(--text);
        font: inherit;
        text-align: left;
        cursor: pointer;
        -webkit-tap-highlight-color: transparent;
      }

      .session:active {
        background: var(--surface-2);
      }

      .session.current {
        background: var(--surface);
      }

      .dot {
        flex: none;
        width: 8px;
        height: 8px;
        border-radius: 50%;
        background: var(--border-strong);
      }

      .session.running .dot {
        background: var(--success);
        box-shadow: 0 0 0 3px color-mix(in srgb, var(--success) 25%, transparent);
      }

      .main {
        flex: 1 1 auto;
        min-width: 0;
        display: flex;
        flex-direction: column;
        gap: 2px;
      }

      .title {
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-size: var(--fs-14);
      }

      .meta {
        display: flex;
        gap: var(--space-4);
        font-size: var(--fs-11-5);
        color: var(--text-faint);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .when {
        flex: none;
        font-size: var(--fs-11-5);
        color: var(--text-faint);
        font-variant-numeric: tabular-nums;
      }
    `,
  ],
})
export class RemoteSessionList {
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly targets = inject(EngineTargetStore);
  private readonly cache = inject(OfflineCache);
  private readonly i18n = inject(I18nService);
  private readonly destroyRef = inject(DestroyRef);

  readonly t = this.i18n.t.bind(this.i18n);

  /** Highlighted session (the one open in the picker's parent). */
  readonly current = input<string | null>(null);
  readonly open = output<string>();

  readonly projects = signal<ProjectEntry[]>([]);
  readonly sessions = signal<SessionMeta[]>([]);
  readonly filter = signal<string | null>(null);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);
  readonly fromCache = signal(false);
  readonly cachedAt = signal(0);

  readonly state = computed(() => {
    if (this.loading() && this.sessions().length === 0) {
      return 'loading';
    }
    if (this.fromCache()) {
      return 'cached';
    }
    return this.error() ? 'error' : this.sessions().length ? 'populated' : 'empty';
  });

  readonly rows = computed<SessionRow[]>(() => {
    const filter = this.filter();
    const names = new Map(this.projects().map((p) => [p.path, p.name]));
    return this.sessions()
      .filter((s) => filter === null || s.directory === filter)
      .filter((s) => !s.parent)
      .slice()
      .sort((a, b) => b.updated_at - a.updated_at)
      .map((session) => ({
        session,
        projectName: names.get(session.directory) ?? lastSegment(session.directory),
      }));
  });

  private debounce: ReturnType<typeof setTimeout> | null = null;
  private loadSeq = 0;
  private lastReconnect = -1;

  constructor() {
    effect(() => {
      const version = this.events.reconnectVersion();
      this.targets.activeId();
      untracked(() => {
        if (version !== this.lastReconnect) {
          this.lastReconnect = version;
        }
        void this.refresh();
      });
    });
    const unsubscribe = this.events.onEvent((ev) => this.handleEvent(ev));
    this.destroyRef.onDestroy(() => {
      unsubscribe();
      if (this.debounce) {
        clearTimeout(this.debounce);
      }
    });
  }

  titleOf(session: SessionMeta): string {
    return session.title || session.alias || session.id.slice(0, 8);
  }

  relative(at: number): string {
    return relativeTime(at);
  }

  async refresh(): Promise<void> {
    const targetId = this.targets.activeId();
    if (!this.engine.connected() || !targetId) {
      await this.loadCached(targetId);
      return;
    }
    const seq = ++this.loadSeq;
    this.loading.set(true);
    try {
      const projects = (await this.engine.listProjects()).slice(0, MAX_PROJECTS);
      const lists = await Promise.all(
        projects.map((p) => this.engine.listSessions(p.path).catch(() => [] as SessionMeta[])),
      );
      if (seq !== this.loadSeq) {
        return;
      }
      const merged = dedupe(lists.flat());
      this.projects.set(projects);
      this.sessions.set(merged);
      this.error.set(null);
      this.fromCache.set(false);
      void this.cache.putSessions(targetId, merged);
    } catch (err) {
      if (seq !== this.loadSeq) {
        return;
      }
      this.error.set(err instanceof Error ? err.message : String(err));
      if (this.sessions().length === 0) {
        await this.loadCached(targetId);
      }
    } finally {
      if (seq === this.loadSeq) {
        this.loading.set(false);
      }
    }
  }

  private async loadCached(targetId: string | null): Promise<void> {
    if (!targetId) {
      return;
    }
    const cached = await this.cache.getSessions(targetId);
    if (cached && this.sessions().length === 0) {
      this.sessions.set(cached.sessions);
      this.fromCache.set(true);
      this.cachedAt.set(cached.savedAt);
    }
  }

  private handleEvent(ev: EngineEvent): void {
    if (!ev.type.startsWith('session.')) {
      return;
    }
    if (this.debounce) {
      clearTimeout(this.debounce);
    }
    this.debounce = setTimeout(() => {
      this.debounce = null;
      void this.refresh();
    }, REFRESH_DEBOUNCE_MS);
  }
}

function dedupe(list: SessionMeta[]): SessionMeta[] {
  const seen = new Map<string, SessionMeta>();
  for (const s of list) {
    seen.set(s.id, s);
  }
  return [...seen.values()];
}

function lastSegment(path: string): string {
  const parts = path.replace(/[\\/]+$/, '').split(/[\\/]/);
  return parts[parts.length - 1] || path;
}
