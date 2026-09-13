/**
 * Root component. Since the redesign (WP-SHELL / F1-11) it only mounts the
 * persistent `AppShell` - sidebar, topbar, right drawer, command palette and
 * the `<router-outlet>` all live inside the shell, so the old top-nav and the
 * session tabs strip are gone. WP-M2 adds the phone-sized `MobileShell`,
 * picked by `FormFactor` (Capacitor, narrow viewport or `?ff=mobile`).
 *
 * What stays here is the cross-cutting wiring the shell does not own:
 * directory-scoped custom CSS, re-synced on navigation and on
 * `config.changed`.
 */

import { Component, OnDestroy, OnInit, inject, signal } from '@angular/core';
import { ActivatedRouteSnapshot, NavigationEnd, Router, RouterOutlet } from '@angular/router';

import { CustomCssService } from '../core/custom-css.service';
import { EngineClient } from '../core/engine-client.service';
import { EngineTargetStore } from '../core/engine-target.store';
import { EventsStore } from '../core/events.store';
import { FormFactor } from '../core/form-factor';
import { ToolSafetyStore } from '../core/tool-safety.store';
import { MobileShell } from '../ui/mobile-shell/mobile-shell';
import { AppShell } from '../ui/shell/app-shell';

@Component({
  selector: 'app-root',
  imports: [AppShell, MobileShell, RouterOutlet],
  templateUrl: './app.html',
  styleUrl: './app.css',
})
export class App implements OnInit, OnDestroy {
  private readonly events = inject(EventsStore);
  private readonly router = inject(Router);
  private readonly engine = inject(EngineClient);
  private readonly customCss = inject(CustomCssService);
  private readonly toolSafety = inject(ToolSafetyStore);
  private readonly targets = inject(EngineTargetStore);

  /**
   * WP-M2 (F10-8): phone form factor -> `MobileShell` instead of `AppShell`.
   * The mobile shell is referenced only inside a `@defer` block, so it (and
   * its tabs) ships as a lazy chunk the desktop never downloads.
   */
  readonly isMobile = inject(FormFactor).isMobile;

  /**
   * WP-BROWSER2 (F7-6): routes flagged `data.bare` (the browser viewer
   * window) render a plain `<router-outlet>` instead of the shell. Seeded
   * from the initial URL so the shell never flashes in a viewer window.
   */
  readonly bare = signal(isBarePath(typeof window !== 'undefined' ? window.location.pathname : ''));

  /** Custom CSS is directory-scoped: re-sync on navigation. */
  private lastCssDirectory: string | null = null;
  private readonly unsubscribeEvents: () => void;

  private readonly subscription = this.router.events.subscribe((ev) => {
    if (ev instanceof NavigationEnd) {
      this.bare.set(isBareRoute(this.router.routerState.snapshot.root));
      void this.syncCustomCss();
    }
  });

  constructor() {
    this.unsubscribeEvents = this.events.onEvent((ev) => {
      // F10-31: both re-syncs hit `GET /config` / `GET /tools/safety`, which
      // a paired desktop's remote scope 403s - nothing to re-sync there.
      if (ev.type === 'config.changed' && !this.targets.remoteScope()) {
        void this.customCss.resync();
        // F7-7: a saved tool_safety override (or an MCP toggle) changes the
        // category list the transcript colours historical calls with.
        void this.toolSafety.resync();
      }
    });
  }

  ngOnInit(): void {
    void this.syncCustomCss();
  }

  private async syncCustomCss(): Promise<void> {
    try {
      const url = new URL(this.router.url, 'http://localhost');
      const dir = url.searchParams.get('directory') ?? this.engine.readLastDirectory();
      if (!dir || dir === this.lastCssDirectory || this.targets.remoteScope()) {
        return;
      }
      this.lastCssDirectory = dir;
      await this.customCss.sync(dir);
    } catch {
      /* theming is non-critical */
    }
  }

  ngOnDestroy(): void {
    this.unsubscribeEvents();
    this.subscription.unsubscribe();
  }
}

/** Deepest activated route carries `data.bare === true`. */
export function isBareRoute(root: ActivatedRouteSnapshot): boolean {
  let route = root;
  while (route.firstChild) {
    route = route.firstChild;
  }
  return route.data['bare'] === true;
}

/** Pre-router guess from the URL path (`/browser-view`), for the first paint. */
export function isBarePath(pathname: string): boolean {
  return /^\/browser-view(\/|$)/.test(pathname);
}
