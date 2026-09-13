/**
 * Shell topbar (WP-SHELL / F1-8): 46px tall, `--bg`, bottom hairline.
 *
 * Left: breadcrumb - "Directory" always links back to Start, followed by the
 * current context (the session id in monospace on Chat, the screen name
 * elsewhere). Right: on Chat only, the right-drawer toggle which highlights
 * while the drawer is open. The density toggle that used to sit here was
 * removed in F7-2 (compact spacing is now the app's only layout).
 *
 * F9-12: when the open chat is a sub-agent session the crumb reads
 * "<parent title> ↳ <child label>" with the parent clickable (it navigates
 * to the parent's chat); the parent title comes from the project session
 * list, falling back to the short parent id while that list is loading.
 */

import { ChangeDetectionStrategy, Component, computed, inject } from '@angular/core';
import { RouterLink } from '@angular/router';

import { SessionMeta, isSubAgentSession, parentSessionId } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';
import { ChatSessionStore } from '../../views/chat/chat-session.store';
import { NavIcon } from '../sidebar/nav-icon';
import { ProjectSessionsStore } from '../shell/project-sessions.store';
import { ShellStore } from '../shell/shell.store';

/** Breadcrumb tail for a sub-agent chat (F9-12). */
export interface SubAgentCrumb {
  parentId: string;
  parentTitle: string;
  childLabel: string;
}

@Component({
  selector: 'app-topbar',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink, NavIcon],
  templateUrl: './topbar.html',
  styleUrl: './topbar.css',
})
export class Topbar {
  readonly shell = inject(ShellStore);
  private readonly i18n = inject(I18nService);
  private readonly chat = inject(ChatSessionStore);
  private readonly project = inject(ProjectSessionsStore);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly isChat = this.shell.isChat;
  readonly sessionId = this.shell.currentSessionId;

  /**
   * F9-12: "<parent> ↳ <child>" data when the open chat is a delegated
   * sub-agent session, null otherwise (the crumb then shows the session id).
   */
  readonly subAgentCrumb = computed<SubAgentCrumb | null>(() => {
    const meta = this.chat.meta();
    const id = this.sessionId();
    if (!meta || !id || meta.id !== id || !isSubAgentSession(meta)) {
      return null;
    }
    const parentId = parentSessionId(meta);
    if (!parentId) {
      return null;
    }
    const parent = this.project.sessions().find((s) => s.id === parentId) ?? null;
    return {
      parentId,
      parentTitle: parent ? titleOf(parent, this.t('start.untitled')) : parentId.slice(0, 8),
      childLabel: titleOf(meta, this.t('start.untitled')),
    };
  });

  /** Breadcrumb tail for non-chat screens (null on Start and Chat). */
  readonly contextLabel = computed<string | null>(() => {
    switch (this.shell.activeScreen()) {
      case 'explorer':
        return this.t('nav.explorer');
      case 'terminal':
        return this.t('nav.terminal');
      case 'debug':
        return this.t('nav.debugLog');
      case 'stats':
        return this.t('nav.stats');
      case 'settings':
        return this.t('nav.settings');
      case 'about':
        return this.t('topbar.about');
      default:
        return null;
    }
  });

  readonly drawerOpen = this.shell.rightDrawerOpen;

  toggleDrawer(): void {
    this.shell.toggleRightDrawer();
  }
}

/** Same precedence as the sidebar: alias, then prompt-derived title, then id. */
function titleOf(session: SessionMeta, untitled: string): string {
  return (
    session.alias?.trim() || session.title?.trim() || `${session.id.slice(0, 8)} — ${untitled}`
  );
}
