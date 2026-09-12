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

import { EngineClient } from '../../core/engine-client.service';
import { EventsStore } from '../../core/events.store';
import { I18nService } from '../../i18n/i18n.service';
import { LANGUAGES, type Language } from '../../i18n';
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
  private readonly engine = inject(EngineClient);
  private readonly i18n = inject(I18nService);
  private readonly router = inject(Router);

  readonly t = this.i18n.t.bind(this.i18n);
  readonly languages = LANGUAGES;
  readonly currentLang = this.i18n.lang;

  readonly expanded = this.shell.sidebarExpanded;
  readonly activeScreen = this.shell.activeScreen;

  /** Project directory shown in the switcher row (engine-persisted choice). */
  readonly directory = signal<string | null>(this.engine.readLastDirectory());

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
