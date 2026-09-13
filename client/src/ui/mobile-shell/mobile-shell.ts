/**
 * Mobile shell (WP-M2 / F10-8): the phone-sized replacement for `AppShell`.
 *
 * Layout: a 48px top bar (tab title, engine status dot + label, overflow),
 * the routed tab in between, the 56px bottom tab bar. `100dvh` + safe-area
 * insets, never `100vh` (the mobile URL bar would hide the tabs).
 *
 * The shell also owns the connection bootstrap the Start view does on the
 * desktop: `EngineClient.connect()` (registers the platform target) and
 * `EventsStore.start()`. The overflow button opens the engine sheet - the
 * list of known targets (this device / paired desktops), tapping one calls
 * `switchTarget()` (F10-7); WP-M5/M6 add pairing and the real onboarding on
 * top of it.
 */

import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { ActivatedRoute, NavigationEnd, Router, RouterOutlet } from '@angular/router';
import { toSignal } from '@angular/core/rxjs-interop';
import { filter, map, startWith } from 'rxjs';

import { EngineClient } from '../../core/engine-client.service';
import { EngineTarget, EngineTargetStore } from '../../core/engine-target.store';
import { EventsStore } from '../../core/events.store';
import { ProjectsStore } from '../../core/projects.store';
import { I18nService } from '../../i18n/i18n.service';
import type { MessageKey } from '../../i18n';
import { ReconnectBanner } from '../reconnect-banner/reconnect-banner';
import { StatusDot, StatusTone } from '../status-dot/status-dot';
import { BottomTabs, MOBILE_TAB_ORDER, MobileTabId } from './bottom-tabs';
import { Sheet } from './sheet';

@Component({
  selector: 'app-mobile-shell',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterOutlet, BottomTabs, Sheet, StatusDot, ReconnectBanner],
  templateUrl: './mobile-shell.html',
  styleUrl: './mobile-shell.css',
})
export class MobileShell {
  private readonly router = inject(Router);
  private readonly route = inject(ActivatedRoute);
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly projects = inject(ProjectsStore);
  private readonly i18n = inject(I18nService);
  readonly targets = inject(EngineTargetStore);

  readonly t = this.i18n.t.bind(this.i18n);

  /** Engine sheet (target list) visibility. */
  readonly engineSheetOpen = signal(false);
  /** Why the initial connect failed (shown under the status label). */
  readonly connectError = signal<string | null>(null);
  readonly switching = signal<string | null>(null);

  /** `data.tab` of the deepest activated route, refreshed on navigation. */
  readonly activeTab = toSignal(
    this.router.events.pipe(
      filter((ev) => ev instanceof NavigationEnd),
      startWith(null),
      map(() => this.readTab()),
    ),
    { initialValue: 'chat' as MobileTabId },
  );

  readonly title = computed(() => {
    const tab = this.activeTab();
    const entry = MOBILE_TAB_ORDER.find((t) => t.id === tab);
    return this.t(entry?.label ?? 'mobile.tab.chat');
  });

  readonly statusTone = computed<StatusTone>(() => {
    switch (this.events.state()) {
      case 'live':
        return 'success';
      case 'error':
      case 'unauthorized':
        return 'danger';
      case 'idle':
        return 'muted';
      default:
        return 'warning';
    }
  });

  readonly statusLabel = computed(() => {
    const key: MessageKey = (() => {
      switch (this.events.state()) {
        case 'live':
          return 'status.live';
        case 'connecting':
          return 'status.connecting';
        case 'reconnecting':
          return 'status.reconnecting';
        case 'error':
          return 'status.error';
        case 'unauthorized':
          return 'status.unauthorized';
        default:
          return 'status.idle';
      }
    })();
    return this.t(key);
  });

  constructor() {
    void this.boot();
  }

  openEngineSheet(): void {
    this.engineSheetOpen.set(true);
  }

  closeEngineSheet(): void {
    this.engineSheetOpen.set(false);
  }

  /** F10-7: re-point the client at another engine, no reload. */
  async switchTo(target: EngineTarget): Promise<void> {
    if (target.id === this.targets.activeId()) {
      this.closeEngineSheet();
      return;
    }
    this.switching.set(target.id);
    try {
      await this.engine.switchTarget(target.id);
      this.connectError.set(null);
      await this.projects.refresh();
    } catch (err) {
      this.connectError.set(err instanceof Error ? err.message : String(err));
    } finally {
      this.switching.set(null);
      this.closeEngineSheet();
    }
  }

  targetKindLabel(target: EngineTarget): string {
    switch (target.kind) {
      case 'embedded':
        return this.t('mobile.engine.kindEmbedded');
      case 'desktop':
        return this.t('mobile.engine.kindDesktop');
      case 'sidecar':
        return this.t('mobile.engine.kindSidecar');
      default:
        return this.t('mobile.engine.kindRemoteUrl');
    }
  }

  private async boot(): Promise<void> {
    try {
      await this.engine.connect();
      this.events.start();
      await this.projects.refresh();
    } catch (err) {
      this.connectError.set(err instanceof Error ? err.message : String(err));
      // Keep the SSE store retrying in the background (it owns the shared
      // connecting/error state the top bar shows).
      this.events.start();
    }
  }

  private readTab(): MobileTabId {
    let route = this.route.snapshot;
    while (route.firstChild) {
      route = route.firstChild;
    }
    const tab = route.data['tab'];
    return isTab(tab) ? tab : 'chat';
  }
}

const TAB_IDS: readonly string[] = MOBILE_TAB_ORDER.map((t) => t.id);

function isTab(value: unknown): value is MobileTabId {
  return typeof value === 'string' && TAB_IDS.includes(value);
}
