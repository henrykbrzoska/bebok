/**
 * Left sidebar of the app shell (WP-SHELL / F1-4..F1-7).
 *
 * Expanded it is 252px wide: header (logo, brand, ⌘K, collapse), project
 * switcher, session search, agent picker + "+ New", the grouped session list,
 * the bottom nav rail and the engine-status/language footer. Collapsed it
 * animates to a 60px icon rail.
 */

import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router } from '@angular/router';

import { AgentInfo, SessionMeta } from '../../core/engine.dtos';
import { EngineClient } from '../../core/engine-client.service';
import { EventsStore } from '../../core/events.store';
import { OpenSessionsStore } from '../../core/open-sessions.store';
import { SessionActivityStore } from '../../core/session-activity.store';
import { I18nService } from '../../i18n/i18n.service';
import { LANGUAGES, type Language } from '../../i18n';
import { ProjectSessionsStore } from '../shell/project-sessions.store';
import { ShellStore, type Screen } from '../shell/shell.store';
import { StatusDot, type StatusTone } from '../status-dot/status-dot';

@Component({
  selector: 'app-sidebar',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule, StatusDot],
  templateUrl: './sidebar.html',
  styleUrl: './sidebar.css',
})
export class Sidebar {
  readonly shell = inject(ShellStore);
  readonly events = inject(EventsStore);
  readonly project = inject(ProjectSessionsStore);
  readonly tabs = inject(OpenSessionsStore);
  readonly activity = inject(SessionActivityStore);
  private readonly engine = inject(EngineClient);
  private readonly i18n = inject(I18nService);
  private readonly router = inject(Router);

  readonly t = this.i18n.t.bind(this.i18n);
  readonly languages = LANGUAGES;
  readonly currentLang = this.i18n.lang;

  readonly expanded = this.shell.sidebarExpanded;
  readonly activeScreen = this.shell.activeScreen;

  /** Project directory shown in the switcher row (engine-persisted choice). */
  readonly directory = this.project.directory;
  readonly agents = this.project.agents;

  /** Session search box (filters the grouped list below it). */
  readonly search = signal('');
  /** Agent preset used by the "+ New" button. */
  readonly selectedAgent = signal('code');
  readonly creating = signal(false);

  /** Label for the agent `<select>`: `name · model` when a model is pinned. */
  agentLabel(agent: AgentInfo): string {
    return agent.model ? `${agent.name} · ${agent.model}` : agent.name;
  }

  /** Create a session in the current directory and open its chat. */
  async newSession(): Promise<void> {
    const dir = this.directory();
    if (!dir || this.creating()) {
      return;
    }
    this.creating.set(true);
    try {
      const created = await this.engine.createSession(dir, this.selectedAgent());
      await this.project.refresh();
      await this.router.navigate(['/chat', created.sessionID]);
    } catch {
      // The Start screen owns error reporting; the sidebar stays quiet.
      await this.router.navigate(['/']);
    } finally {
      this.creating.set(false);
    }
  }

  /** Bottom nav rail entries; `rail` is the 2-letter collapsed glyph. */
  readonly navItems: { screen: Screen; path: string; labelKey: NavLabelKey; railKey: RailKey }[] = [
    { screen: 'explorer', path: '/explorer', labelKey: 'nav.explorer', railKey: 'sidebar.railExplorer' },
    { screen: 'terminal', path: '/terminal', labelKey: 'nav.terminal', railKey: 'sidebar.railTerminal' },
    { screen: 'debug', path: '/debug', labelKey: 'nav.debugLog', railKey: 'sidebar.railDebug' },
    { screen: 'settings', path: '/settings', labelKey: 'nav.settings', railKey: 'sidebar.railSettings' },
  ];

  /** Engine status: color is never used alone - `statusLabel()` goes with it. */
  readonly statusTone = computed<StatusTone>(() => {
    switch (this.events.state()) {
      case 'live':
        return 'success';
      case 'connecting':
      case 'reconnecting':
        return 'warning';
      default:
        return 'idle';
    }
  });

  readonly statusLabel = computed(() => {
    switch (this.events.state()) {
      case 'live':
        return this.t('status.live');
      case 'connecting':
        return this.t('status.connecting');
      case 'reconnecting':
        return this.t('status.reconnecting');
      default:
        return this.t('status.idle');
    }
  });

  /**
   * Sessions grouped Pinned / Today / Older. "Pinned" are the sessions the
   * user keeps open (the persisted open-tabs list), the rest split by day.
   */
  readonly groups = computed<SessionGroup[]>(() => {
    const query = this.search().trim().toLowerCase();
    const pinnedIds = new Set(this.tabs.sessions().map((s) => s.id));
    const startOfToday = new Date().setHours(0, 0, 0, 0);

    const matches = this.project
      .sessions()
      .filter((session) => {
        if (!query) {
          return true;
        }
        const haystack = `${session.title ?? ''} ${session.alias ?? ''} ${session.agent} ${session.id}`;
        return haystack.toLowerCase().includes(query);
      })
      .slice()
      .sort((a, b) => b.updated_at - a.updated_at);

    const pinned = matches.filter((s) => pinnedIds.has(s.id));
    const rest = matches.filter((s) => !pinnedIds.has(s.id));

    return [
      { key: 'pinned' as const, labelKey: 'sidebar.pinned' as const, sessions: pinned },
      {
        key: 'today' as const,
        labelKey: 'sidebar.today' as const,
        sessions: rest.filter((s) => s.updated_at >= startOfToday),
      },
      {
        key: 'older' as const,
        labelKey: 'sidebar.older' as const,
        sessions: rest.filter((s) => s.updated_at < startOfToday),
      },
    ].filter((group) => group.sessions.length > 0);
  });

  readonly hasSessions = computed(() => this.project.sessions().length > 0);

  sessionTitle(session: SessionMeta): string {
    return session.alias?.trim() || session.title?.trim() || `${session.id.slice(0, 8)} — ${this.t('start.untitled')}`;
  }

  /** `agent · Nk tok · time` - monospace meta line under the title. */
  sessionMeta(session: SessionMeta): string {
    const tokens = session.usage.input_tokens + session.usage.output_tokens;
    const formatted = tokens >= 1000 ? `${(tokens / 1000).toFixed(1)}k` : String(tokens);
    return `${session.agent} · ${formatted} tok · ${this.sessionTime(session.updated_at)}`;
  }

  sessionTone(session: SessionMeta): StatusTone {
    return this.activity.isRunning(session.id) ? 'success' : 'idle';
  }

  sessionStatusLabel(session: SessionMeta): string {
    return this.activity.isRunning(session.id) ? this.t('nav.working') : this.t('status.idle');
  }

  isCurrentSession(session: SessionMeta): boolean {
    return this.shell.currentSessionId() === session.id;
  }

  openSession(session: SessionMeta): void {
    void this.router.navigate(['/chat', session.id]);
  }

  private sessionTime(ms: number): string {
    const date = new Date(ms);
    if (ms >= new Date().setHours(0, 0, 0, 0)) {
      return date.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
    }
    return date.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  }

  isActive(screen: Screen): boolean {
    return this.activeScreen() === screen;
  }

  go(path: string): void {
    void this.router.navigateByUrl(path);
  }

  /** The project switcher navigates to Start, which owns directory picking. */
  switchProject(): void {
    void this.router.navigate(['/']);
  }

  setLanguage(lang: Language): void {
    this.i18n.setLanguage(lang);
  }
}

type NavLabelKey = 'nav.explorer' | 'nav.terminal' | 'nav.debugLog' | 'nav.settings';
type RailKey =
  | 'sidebar.railExplorer'
  | 'sidebar.railTerminal'
  | 'sidebar.railDebug'
  | 'sidebar.railSettings';

interface SessionGroup {
  key: 'pinned' | 'today' | 'older';
  labelKey: 'sidebar.pinned' | 'sidebar.today' | 'sidebar.older';
  sessions: SessionMeta[];
}
