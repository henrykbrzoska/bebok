/**
 * Chat home (WP-M5 / F10-18): the Chat tab when no session is open.
 *
 * - Recent local sessions, newest first, from `GET /session` against the
 *   active (embedded) engine - across the quick-session scratch directory,
 *   the last used directory and every registered project.
 * - An agent/preset picker (`GET /agent`) for the next session.
 * - "Quick session": a scratch session that skips project selection (plan
 *   goal A). Its directory is `<engine home>/.bebok/quick`, resolved once per
 *   target through `GET /fs/browse` (the engine's `Home` root - the app's
 *   private files dir on Android) and cached; the engine creates the
 *   directory on first use.
 *
 * Tapping a session navigates to the existing `chat/:sessionID` route.
 */

import {
  ChangeDetectionStrategy,
  Component,
  OnDestroy,
  OnInit,
  computed,
  effect,
  inject,
  output,
  signal,
  untracked,
} from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router } from '@angular/router';

import { ENGINE_API } from '../../../core/engine-api';
import { EngineTargetStore } from '../../../core/engine-target.store';
import { AgentInfo, SessionMeta } from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { ProjectsStore } from '../../../core/projects.store';
import { I18nService } from '../../../i18n/i18n.service';
import { resolveQuickDirectory } from './quick-directory';

const MAX_RECENT = 50;

/** Last path segment of a directory, for the session rows. */
export function directoryName(path: string): string {
  const parts = path.replace(/[\\/]+$/, '').split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/** Short relative time ("3m", "2h", "5d"), else a locale date. */
export function relativeTime(ms: number, now = Date.now()): string {
  const diff = Math.max(0, now - ms);
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
  return new Date(ms).toLocaleDateString();
}

@Component({
  selector: 'app-chat-home',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  template: `
    <section class="home" data-testid="chat-home">
      @if (!connected()) {
        <div class="home-empty" data-testid="chat-home-disconnected">
          <h2>{{ t('mobile.home.noEngineTitle') }}</h2>
          <p>{{ sseConnecting() ? t('mobile.home.noEngineStarting') : t('mobile.home.noEngineHint') }}</p>
          <button type="button" class="btn-primary" (click)="setup.emit()" data-testid="chat-home-setup">
            {{ t('mobile.home.setup') }}
          </button>
        </div>
      } @else {
        <div class="home-quick">
          <label class="home-agent">
            <span>{{ t('start.agent') }}</span>
            <select
              [ngModel]="selectedAgent()"
              (ngModelChange)="selectedAgent.set($event)"
              [disabled]="creating()"
              data-testid="chat-home-agent"
            >
              @for (agent of agents(); track agent.name) {
                <option [value]="agent.name">{{ agent.name }}</option>
              } @empty {
                <option value="code">code</option>
              }
            </select>
          </label>
          <button
            type="button"
            class="btn-primary"
            (click)="quickSession()"
            [disabled]="creating() || !quickDirectory()"
            data-testid="chat-home-quick"
          >
            {{ creating() ? t('start.creating') : t('mobile.home.quickSession') }}
          </button>
          @if (agentDescription(); as description) {
            <p class="home-agent-desc">{{ description }}</p>
          }
        </div>

        @if (error(); as err) {
          <p class="home-error" role="alert" data-testid="chat-home-error">{{ err }}</p>
        }

        <h3 class="home-section">{{ t('mobile.home.recent') }}</h3>
        @if (loading() && sessions().length === 0) {
          <p class="home-muted">{{ t('start.loading') }}</p>
        } @else if (sessions().length === 0) {
          <div class="home-empty" data-testid="chat-home-empty">
            <p>{{ t('mobile.home.emptyHint') }}</p>
          </div>
        } @else {
          <ul class="home-list" data-testid="chat-home-sessions">
            @for (s of sessions(); track s.id) {
              <li>
                <button type="button" class="home-row" (click)="open(s)" [attr.data-testid]="'session-' + s.id">
                  <span class="home-row-main">
                    <span class="home-row-title">{{ s.title || s.alias || t('start.untitled') }}</span>
                    <span class="home-row-sub">
                      {{ s.agent }} · {{ dirLabel(s.directory) }}
                      @if (s.running) {
                        · <span class="home-running">{{ t('chat.running') }}</span>
                      }
                    </span>
                  </span>
                  <span class="home-row-when">{{ when(s.updated_at) }}</span>
                </button>
              </li>
            }
          </ul>
        }
      }
    </section>
  `,
  styles: [
    `
      :host {
        display: flex;
        flex-direction: column;
        flex: 1 1 auto;
        min-height: 0;
        overflow-y: auto;
      }

      .home {
        display: flex;
        flex-direction: column;
        gap: var(--space-12);
        padding: var(--space-16);
      }

      .home-quick {
        display: flex;
        flex-direction: column;
        gap: var(--space-8);
        padding: var(--space-12);
        border: 1px solid var(--border);
        border-radius: var(--radius-control);
        background: var(--surface);
      }

      .home-agent {
        display: flex;
        align-items: center;
        gap: var(--space-8);
        font-size: var(--fs-12-5);
        color: var(--text-muted);
      }

      .home-agent select {
        flex: 1 1 auto;
        min-height: 40px;
        padding: 0 var(--space-8);
        border: 1px solid var(--border);
        border-radius: var(--radius-control);
        background: var(--bg);
        color: var(--text);
        font: inherit;
      }

      .home-agent-desc {
        margin: 0;
        font-size: var(--fs-12);
        color: var(--text-muted);
      }

      .btn-primary {
        min-height: 44px;
        border: 0;
        border-radius: var(--radius-control);
        background: var(--accent);
        color: var(--bg);
        font: inherit;
        font-weight: 600;
        cursor: pointer;
      }

      .btn-primary:disabled {
        opacity: 0.5;
        cursor: default;
      }

      .home-section {
        margin: var(--space-8) 0 0;
        font-size: var(--fs-12);
        font-weight: 600;
        text-transform: uppercase;
        letter-spacing: 0.04em;
        color: var(--text-muted);
      }

      .home-muted {
        margin: 0;
        color: var(--text-muted);
      }

      .home-empty {
        display: flex;
        flex-direction: column;
        gap: var(--space-12);
        padding: var(--space-24) var(--space-12);
        text-align: center;
        color: var(--text-muted);
      }

      .home-empty h2 {
        margin: 0;
        font-size: var(--fs-16);
        color: var(--text);
      }

      .home-empty p {
        margin: 0;
      }

      .home-error {
        margin: 0;
        color: var(--danger);
        font-size: var(--fs-12-5);
        overflow-wrap: anywhere;
      }

      .home-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        border: 1px solid var(--border);
        border-radius: var(--radius-control);
        background: var(--surface);
        overflow: hidden;
      }

      .home-list li + li {
        border-top: 1px solid var(--border);
      }

      .home-row {
        width: 100%;
        display: flex;
        align-items: center;
        gap: var(--space-12);
        min-height: 56px;
        padding: var(--space-8) var(--space-12);
        border: 0;
        background: transparent;
        color: var(--text);
        font: inherit;
        text-align: left;
        cursor: pointer;
        -webkit-tap-highlight-color: transparent;
      }

      .home-row:active {
        background: var(--surface-2);
      }

      .home-row-main {
        flex: 1 1 auto;
        min-width: 0;
        display: flex;
        flex-direction: column;
        gap: 2px;
      }

      .home-row-title {
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .home-row-sub {
        font-size: var(--fs-12);
        color: var(--text-muted);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .home-running {
        color: var(--accent);
      }

      .home-row-when {
        flex: none;
        font-size: var(--fs-12);
        color: var(--text-muted);
      }
    `,
  ],
})
export class ChatHomeView implements OnInit, OnDestroy {
  private readonly engine = inject(ENGINE_API);
  private readonly targets = inject(EngineTargetStore);
  private readonly events = inject(EventsStore);
  private readonly projects = inject(ProjectsStore);
  private readonly router = inject(Router);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  /** "Set up" from the disconnected state - the tab reopens onboarding. */
  readonly setup = output<void>();

  readonly connected = this.engine.connected;
  readonly sseConnecting = computed(() => this.events.state() === 'connecting');

  readonly loading = signal(false);
  readonly creating = signal(false);
  readonly error = signal<string | null>(null);
  readonly sessions = signal<SessionMeta[]>([]);
  readonly agents = signal<AgentInfo[]>([]);
  readonly selectedAgent = signal('code');
  readonly quickDirectory = signal<string | null>(null);

  readonly agentDescription = computed(
    () => this.agents().find((a) => a.name === this.selectedAgent())?.description ?? null,
  );

  private refreshSeq = 0;
  private unsubscribe: (() => void) | null = null;

  constructor() {
    // Re-list after the engine (re)connects or the target switches
    // (`reconnectVersion` bumps on every new SSE stream).
    effect(() => {
      this.events.reconnectVersion();
      const connected = this.connected();
      untracked(() => {
        if (connected) {
          void this.refresh();
        }
      });
    });
  }

  ngOnInit(): void {
    this.unsubscribe = this.events.onEvent((event) => {
      if (
        event.type === 'session.created' ||
        event.type === 'session.deleted' ||
        (event.type === 'session.updated' && event.properties?.['running'] === false)
      ) {
        void this.refresh();
      }
    });
  }

  ngOnDestroy(): void {
    this.unsubscribe?.();
  }

  async refresh(): Promise<void> {
    const seq = ++this.refreshSeq;
    this.loading.set(true);
    try {
      const quick = await this.resolveQuickDirectory();
      if (seq !== this.refreshSeq) {
        return;
      }
      this.quickDirectory.set(quick);
      const directories = unique([
        quick,
        this.engine.readLastDirectory(),
        ...this.projects.projects().map((p) => p.path),
      ]);
      const [lists, agents] = await Promise.all([
        Promise.allSettled(directories.map((dir) => this.engine.listSessions(dir))),
        quick ? this.engine.listAgents(quick).catch(() => [] as AgentInfo[]) : Promise.resolve([]),
      ]);
      if (seq !== this.refreshSeq) {
        return;
      }
      const merged = new Map<string, SessionMeta>();
      for (const result of lists) {
        if (result.status === 'fulfilled') {
          for (const s of result.value) {
            merged.set(s.id, s);
          }
        }
      }
      this.sessions.set(
        [...merged.values()].sort((a, b) => b.updated_at - a.updated_at).slice(0, MAX_RECENT),
      );
      this.agents.set(agents);
      if (agents.length > 0 && !agents.some((a) => a.name === this.selectedAgent())) {
        this.selectedAgent.set(agents.some((a) => a.name === 'code') ? 'code' : agents[0].name);
      }
      this.error.set(
        lists.every((r) => r.status === 'rejected') && lists.length > 0
          ? describe((lists[0] as PromiseRejectedResult).reason)
          : null,
      );
    } catch (err) {
      if (seq === this.refreshSeq) {
        this.error.set(describe(err));
      }
    } finally {
      if (seq === this.refreshSeq) {
        this.loading.set(false);
      }
    }
  }

  /** Scratch session in the quick directory with the picked agent. */
  async quickSession(): Promise<void> {
    const directory = this.quickDirectory();
    if (!directory || this.creating()) {
      return;
    }
    this.creating.set(true);
    this.error.set(null);
    try {
      const created = await this.engine.createSession(directory, this.selectedAgent());
      this.engine.saveDirectory(directory);
      await this.router.navigate(['/m/chat', created.sessionID]);
    } catch (err) {
      this.error.set(describe(err));
    } finally {
      this.creating.set(false);
    }
  }

  open(session: SessionMeta): void {
    void this.router.navigate(['/m/chat', session.id]);
  }

  dirLabel(path: string): string {
    return path === this.quickDirectory() ? this.t('mobile.home.quickLabel') : directoryName(path);
  }

  when(ms: number): string {
    return relativeTime(ms);
  }

  private resolveQuickDirectory(): Promise<string | null> {
    return resolveQuickDirectory(this.engine, this.targets.activeId(), this.projects.projects());
  }
}

function unique(values: Array<string | null | undefined>): string[] {
  const out: string[] = [];
  for (const v of values) {
    if (v && !out.includes(v)) {
      out.push(v);
    }
  }
  return out;
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
