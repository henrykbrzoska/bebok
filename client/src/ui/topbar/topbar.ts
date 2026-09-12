/**
 * Shell topbar (WP-SHELL / F1-8): 46px tall, `--bg`, bottom hairline.
 *
 * Left: breadcrumb - "Directory" always links back to Start, followed by the
 * current context (the session id in monospace on Chat, the screen name
 * elsewhere). Right: the density toggle and, on Chat only, the right-drawer
 * toggle which highlights while the drawer is open.
 */

import { ChangeDetectionStrategy, Component, computed, inject } from '@angular/core';
import { RouterLink } from '@angular/router';

import { I18nService } from '../../i18n/i18n.service';
import { ShellStore } from '../shell/shell.store';

@Component({
  selector: 'app-topbar',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  templateUrl: './topbar.html',
  styleUrl: './topbar.css',
})
export class Topbar {
  readonly shell = inject(ShellStore);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly isChat = this.shell.isChat;
  readonly sessionId = this.shell.currentSessionId;

  /** Breadcrumb tail for non-chat screens (null on Start and Chat). */
  readonly contextLabel = computed<string | null>(() => {
    switch (this.shell.activeScreen()) {
      case 'explorer':
        return this.t('nav.explorer');
      case 'terminal':
        return this.t('nav.terminal');
      case 'debug':
        return this.t('nav.debugLog');
      case 'settings':
        return this.t('nav.settings');
      default:
        return null;
    }
  });

  readonly compact = computed(() => this.shell.density() === 'compact');
  readonly drawerOpen = this.shell.rightDrawerOpen;

  toggleDensity(): void {
    this.shell.toggleDensity();
  }

  toggleDrawer(): void {
    this.shell.toggleRightDrawer();
  }
}
