/**
 * Left sidebar of the app shell (WP-SHELL / F1-4..F1-7).
 *
 * Expanded it is 252px wide: header (logo, brand, ⌘K, collapse), project
 * switcher, session search, agent picker + "+ New", the grouped session list,
 * the bottom nav rail and the engine-status/language footer. Collapsed it
 * animates to a 60px icon rail.
 *
 * F9-12: sub-agent sessions (`isSubAgentSession`: parent + alias) render as
 * nested rows under their parent - indented, with a ↳ glyph, muted, tooltip
 * "Sub-agent of <parent>". A child whose parent is not in the list (deleted,
 * or filtered out by the search box) still renders with the glyph at the top
 * level so it is never mistaken for a normal session.
 *
 * F9-13: the bottom nav is an icon rail (inline SVG, `app-nav-icon`) in both
 * layouts - a horizontal row when expanded, stacked when collapsed - with a
 * delayed CSS tooltip / native title, `aria-label`, and a badge dot on
 * Settings while any tool is still uncategorized.
 */

import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router } from '@angular/router';

import { AgentInfo, SessionMeta, isSubAgentSession, parentSessionId } from '../../core/engine.dtos';
import { EngineClient } from '../../core/engine-client.service';
import { EventsStore } from '../../core/events.store';
import { OpenSessionsStore } from '../../core/open-sessions.store';
import { SessionActivityStore } from '../../core/session-activity.store';
import { ToolSafetyStore } from '../../core/tool-safety.store';
import { I18nService } from '../../i18n/i18n.service';
import { LANGUAGES, type Language } from '../../i18n';
import { BranchBadge } from '../new-session-dialog/branch-badge';
import { NewSessionDialog } from '../new-session-dialog/new-session-dialog';
import { NewSessionDialogStore } from '../new-session-dialog/new-session-dialog.store';
import { ProjectSessionsStore } from '../shell/project-sessions.store';
import { ShellStore, type Screen } from '../shell/shell.store';
import { StatusDot, type StatusTone } from '../status-dot/status-dot';
import { NavIcon, type NavIconName } from './nav-icon';

/** Indent per nesting level of a sub-agent row (px). */
const ROW_INDENT_PX = 14;
/** Base horizontal padding of a session row (matches `.session-row` CSS). */
const ROW_BASE_PADDING_PX = 8;
/** Deepest nesting drawn; deeper chains keep the glyph but stop indenting. */
const MAX_ROW_DEPTH = 4;

@Component({
  selector: 'app-sidebar',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule, StatusDot, BranchBadge, NewSessionDialog, NavIcon],
  templateUrl: './sidebar.html',
  styleUrl: './sidebar.css',
})
export class Sidebar {
  readonly shell = inject(ShellStore);
  readonly events = inject(EventsStore);
  readonly project = inject(ProjectSessionsStore);
  readonly tabs = inject(OpenSessionsStore);
  readonly activity = inject(SessionActivityStore);
  private readonly toolSafety = inject(ToolSafetyStore);
  private readonly newSessionDialog = inject(NewSessionDialogStore);
  private readonly engine = inject(EngineClient);
  private readonly i18n = inject(I18nService);
  private readonly router = inject(Router);

  /** Engine base URL (http://host:port, no token) shown on click. */
  readonly engineUrl = computed(() => this.engine.connection()?.baseUrl ?? '');
  readonly showEngineUrl = signal(false);
  toggleEngineUrl(): void {
    this.showEngineUrl.update((v) => !v);
  }

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
  /** Agent preset preselected in the "New session" dialog by "+ New". */
  readonly selectedAgent = signal('code');
  /** The dialog owns the in-flight state now; kept so the button binding is unchanged. */
  readonly creating = signal(false);

  /** Label for the agent `<select>`: `name · model` when a model is pinned. */
  agentLabel(agent: AgentInfo): string {
    return agent.model ? `${agent.name} · ${agent.model}` : agent.name;
  }

  /**
   * Open the "New session" dialog for the current directory (WP-GIT /
   * F6-16). The dialog offers agent, model and the git-worktree option, then
   * creates the session and opens its chat itself.
   */
  newSession(): void {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    this.newSessionDialog.openFor(dir, { agent: this.selectedAgent() });
  }

  /** Bottom nav rail entries (F9-13: one SVG icon each, see `nav-icon.ts`). */
  readonly navItems: { screen: Screen; path: string; labelKey: NavLabelKey; icon: NavIconName }[] =
    [
      { screen: 'explorer', path: '/explorer', labelKey: 'nav.explorer', icon: 'explorer' },
      { screen: 'terminal', path: '/terminal', labelKey: 'nav.terminal', icon: 'terminal' },
      { screen: 'debug', path: '/debug', labelKey: 'nav.debugLog', icon: 'debug' },
      { screen: 'stats', path: '/stats', labelKey: 'nav.stats', icon: 'stats' },
      { screen: 'settings', path: '/settings', labelKey: 'nav.settings', icon: 'settings' },
    ];

  /** F9-13: Settings badge - tools the safety table does not categorize yet. */
  readonly uncategorizedCount = this.toolSafety.uncategorizedCount;

  /** Engine status: color is never used alone - `statusLabel()` goes with it. */
  readonly statusTone = computed<StatusTone>(() => {
    switch (this.events.state()) {
      case 'live':
        return 'success';
      case 'connecting':
      case 'reconnecting':
        return 'warning';
      case 'error':
      case 'unauthorized':
        return 'danger';
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
      case 'error':
        return this.t('status.error');
      case 'unauthorized':
        return this.t('status.unauthorized');
      default:
        return this.t('status.idle');
    }
  });

  /**
   * Sessions grouped Pinned / Today / Older. "Pinned" are the sessions the
   * user keeps open (the persisted open-tabs list), the rest split by day.
   *
   * F9-12: within a group each top-level session is followed by its
   * sub-agent children (depth-first, spawn order), so a `task`/`fleet`
   * child sits right under its parent instead of interleaving by time.
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

    const tree = buildSessionTree(matches);
    const pinned = tree.roots.filter((s) => pinnedIds.has(s.id));
    const rest = tree.roots.filter((s) => !pinnedIds.has(s.id));
    const expand = (list: SessionMeta[]): SessionRow[] => list.flatMap((s) => tree.rows(s));

    return [
      { key: 'pinned' as const, labelKey: 'sidebar.pinned' as const, rows: expand(pinned) },
      {
        key: 'today' as const,
        labelKey: 'sidebar.today' as const,
        rows: expand(rest.filter((s) => s.updated_at >= startOfToday)),
      },
      {
        key: 'older' as const,
        labelKey: 'sidebar.older' as const,
        rows: expand(rest.filter((s) => s.updated_at < startOfToday)),
      },
    ].filter((group) => group.rows.length > 0);
  });

  readonly hasSessions = computed(() => this.project.sessions().length > 0);

  sessionTitle(session: SessionMeta): string {
    return (
      session.alias?.trim() ||
      session.title?.trim() ||
      `${session.id.slice(0, 8)} — ${this.t('start.untitled')}`
    );
  }

  /** F9-12: left padding grows with nesting depth (capped). */
  rowIndent(row: SessionRow): number {
    return ROW_BASE_PADDING_PX + Math.min(row.depth, MAX_ROW_DEPTH) * ROW_INDENT_PX;
  }

  /** F9-12: "Sub-agent of <parent>" for children, the plain title otherwise. */
  rowTooltip(row: SessionRow): string {
    if (!row.subAgent) {
      return this.sessionTitle(row.session);
    }
    const parent = row.parent
      ? this.sessionTitle(row.parent)
      : (parentSessionId(row.session)?.slice(0, 8) ?? '');
    return `${this.sessionTitle(row.session)} — ${this.t('sidebar.subAgentOf', { parent })}`;
  }

  /** `agent · Nk tok · time` - monospace meta line under the title. */
  sessionMeta(session: SessionMeta): string {
    const tokens = session.usage.input_tokens + session.usage.output_tokens;
    const formatted = tokens >= 1000 ? `${(tokens / 1000).toFixed(1)}k` : String(tokens);
    return `${session.agent} · ${formatted} tok · ${this.sessionTime(session.updated_at)}`;
  }

  /** Branch of a git-worktree session (engine-derived), or null. */
  worktreeBranch(session: SessionMeta): string | null {
    return session.worktree_branch?.trim() || null;
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

  /** Open the registered-project switcher without leaving the current screen. */
  switchProject(): void {
    this.shell.openProjectSwitcher();
  }

  setLanguage(lang: Language): void {
    this.i18n.setLanguage(lang);
  }
}

type NavLabelKey = 'nav.explorer' | 'nav.terminal' | 'nav.debugLog' | 'nav.stats' | 'nav.settings';

/** One rendered line of the session list (F9-12). */
export interface SessionRow {
  session: SessionMeta;
  /** 0 for a top-level session, 1+ for nested sub-agent children. */
  depth: number;
  /** True for a `task`/`fleet` child - drawn with the ↳ glyph even at depth 0
   *  (orphan: its parent is not in the visible list). */
  subAgent: boolean;
  /** The parent row's session when it is in the list (tooltip text). */
  parent: SessionMeta | null;
}

interface SessionGroup {
  key: 'pinned' | 'today' | 'older';
  labelKey: 'sidebar.pinned' | 'sidebar.today' | 'sidebar.older';
  rows: SessionRow[];
}

/**
 * F9-12: fold a flat, time-sorted session list into parent -> children.
 * `roots` are the sessions that render at the top level (normal sessions and
 * orphaned children); `rows(root)` expands one root into its depth-first
 * rows. Children are ordered by `created_at` (spawn order). Exported for the
 * spec.
 */
export function buildSessionTree(sessions: readonly SessionMeta[]): {
  roots: SessionMeta[];
  rows: (root: SessionMeta) => SessionRow[];
} {
  const byId = new Map(sessions.map((s) => [s.id, s]));
  const children = new Map<string, SessionMeta[]>();
  const roots: SessionMeta[] = [];
  for (const session of sessions) {
    const parentId = parentSessionId(session);
    if (isSubAgentSession(session) && parentId && byId.has(parentId)) {
      const list = children.get(parentId) ?? [];
      list.push(session);
      children.set(parentId, list);
    } else {
      roots.push(session);
    }
  }
  for (const list of children.values()) {
    list.sort((a, b) => a.created_at - b.created_at);
  }
  const rows = (root: SessionMeta): SessionRow[] => {
    const out: SessionRow[] = [];
    const seen = new Set<string>();
    const walk = (session: SessionMeta, depth: number, parent: SessionMeta | null): void => {
      if (seen.has(session.id)) {
        return; // defensive: a corrupt parent cycle must not hang the sidebar
      }
      seen.add(session.id);
      out.push({ session, depth, subAgent: isSubAgentSession(session), parent });
      for (const child of children.get(session.id) ?? []) {
        walk(child, depth + 1, session);
      }
    };
    walk(root, 0, null);
    return out;
  };
  return { roots, rows };
}
