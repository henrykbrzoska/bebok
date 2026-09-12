/**
 * Shell state (WP-SHELL / F1-3).
 *
 * The redesign keeps the Angular Router: screens are still routes, they just
 * render *inside* `AppShell`. `activeScreen` is therefore derived from the
 * router - each route carries `data.screen` (see `app.routes.ts`) - rather
 * than being a hand-rolled parallel signal, so deep links keep working.
 *
 * Layout preferences (sidebar/drawer/density) live in `UiPrefsStore` so they
 * persist to localStorage; this store re-exports them so components have one
 * place to read shell state from.
 */

import { Injectable, computed, inject, signal } from '@angular/core';
import { ActivatedRoute, NavigationEnd, Router } from '@angular/router';

import { UiPrefsStore } from '../../core/ui-prefs.store';

export type Screen =
  | 'start'
  | 'connect'
  | 'chat'
  | 'explorer'
  | 'terminal'
  | 'debug'
  | 'settings';

interface RouteSnapshot {
  screen: Screen;
  sessionId: string | null;
}

const FALLBACK: RouteSnapshot = { screen: 'start', sessionId: null };

@Injectable({ providedIn: 'root' })
export class ShellStore {
  private readonly router = inject(Router);
  private readonly route = inject(ActivatedRoute);
  private readonly prefs = inject(UiPrefsStore);

  /** Raw route snapshot, refreshed on every successful navigation. */
  private readonly snapshot = signal<RouteSnapshot>(FALLBACK);

  /** Which screen the router is currently showing. */
  readonly activeScreen = computed<Screen>(() => this.snapshot().screen);
  /** Session id of the open chat, or null on every other screen. */
  readonly currentSessionId = computed<string | null>(() => this.snapshot().sessionId);
  readonly isChat = computed(() => this.activeScreen() === 'chat');

  /** Layout preferences (persisted). */
  readonly sidebarExpanded = this.prefs.sidebarExpanded;
  readonly rightDrawerOpen = this.prefs.rightDrawerOpen;
  readonly rightDrawerPanels = this.prefs.rightDrawerPanels;
  readonly rightDrawerWidth = this.prefs.rightDrawerWidth;
  readonly density = this.prefs.density;

  /** Command palette (ephemeral, never persisted). */
  readonly commandPaletteOpen = signal(false);

  constructor() {
    this.refresh();
    this.router.events.subscribe((event) => {
      if (event instanceof NavigationEnd) {
        this.refresh();
      }
    });
  }

  openCommandPalette(): void {
    this.commandPaletteOpen.set(true);
  }

  closeCommandPalette(): void {
    this.commandPaletteOpen.set(false);
  }

  toggleCommandPalette(): void {
    this.commandPaletteOpen.update((open) => !open);
  }

  toggleSidebar(): void {
    this.prefs.toggleSidebarExpanded();
  }

  toggleRightDrawer(): void {
    this.prefs.toggleRightDrawer();
  }

  toggleRightDrawerPanel(panel: 'session' | 'explorer' | 'terminal'): void {
    this.prefs.toggleRightDrawerPanel(panel);
  }

  setRightDrawerWidth(px: number): void {
    this.prefs.setRightDrawerWidth(px);
  }

  toggleDensity(): void {
    this.prefs.toggleDensity();
  }

  /** Walk to the deepest activated route and read its `data.screen`. */
  private refresh(): void {
    let route = this.route.snapshot;
    while (route.firstChild) {
      route = route.firstChild;
    }
    const screen = route.data['screen'];
    this.snapshot.set({
      screen: isScreen(screen) ? screen : FALLBACK.screen,
      sessionId: route.paramMap.get('sessionID'),
    });
  }
}

const SCREENS: readonly string[] = [
  'start',
  'connect',
  'chat',
  'explorer',
  'terminal',
  'debug',
  'settings',
];

function isScreen(value: unknown): value is Screen {
  return typeof value === 'string' && SCREENS.includes(value);
}
