import {
  AfterViewInit,
  Component,
  ElementRef,
  OnDestroy,
  OnInit,
  computed,
  effect,
  inject,
  signal,
  viewChild,
} from '@angular/core';
import { FormsModule } from '@angular/forms';
import {
  NavigationEnd,
  Router,
  RouterLink,
  RouterLinkActive,
  RouterOutlet,
} from '@angular/router';

import { CustomCssService } from '../core/custom-css.service';
import { EventsStore } from '../core/events.store';
import { OpenSessionsStore } from '../core/open-sessions.store';
import { SessionActivityStore } from '../core/session-activity.store';
import { UiPrefsStore } from '../core/ui-prefs.store';
import { EngineClient } from '../core/engine-client.service';
import { I18nService } from '../i18n/i18n.service';
import { LANGUAGES, type Language } from '../i18n';

@Component({
  selector: 'app-root',
  imports: [RouterOutlet, RouterLink, RouterLinkActive, FormsModule],
  templateUrl: './app.html',
  styleUrl: './app.css',
})
export class App implements OnInit, AfterViewInit, OnDestroy {
  readonly events = inject(EventsStore);
  readonly activity = inject(SessionActivityStore);
  readonly tabs = inject(OpenSessionsStore);
  readonly i18n = inject(I18nService);
  readonly uiPrefs = inject(UiPrefsStore);
  private readonly router = inject(Router);
  private readonly engine = inject(EngineClient);
  private readonly customCss = inject(CustomCssService);

  readonly languages = LANGUAGES;
  readonly currentLang = this.i18n.lang;

  readonly t = this.i18n.t.bind(this.i18n);

  /** The chat currently open (synced from the route) - no session switching here. */
  readonly currentSessionId = signal<string | null>(null);

  /** Variant 2: independent tabs-strip scroll state. */
  readonly tabsStrip = viewChild<ElementRef<HTMLElement>>('tabsStrip');
  readonly canScrollLeft = signal(false);
  readonly canScrollRight = signal(false);
  private resizeObserver: ResizeObserver | null = null;

  /** Custom CSS is directory-scoped: re-sync on navigation. */
  private lastCssDirectory: string | null = null;
  private readonly unsubscribeEvents: () => void;

  constructor() {
    this.unsubscribeEvents = this.events.onEvent((ev) => {
      if (ev.type === 'config.changed') {
        void this.customCss.resync();
      }
    });
    // Keep arrows + active-tab visibility in sync with tabs/route.
    effect(() => {
      void this.tabs.sessions().length;
      void this.currentSessionId();
      queueMicrotask(() => {
        this.updateScrollArrows();
        this.scrollActiveIntoView(false);
      });
    });
  }

  ngOnInit(): void {
    this.syncRoute(this.router.url);
    void this.syncCustomCss();
  }

  ngAfterViewInit(): void {
    this.updateScrollArrows();
    this.scrollActiveIntoView(false);
    window.addEventListener('resize', this.onWindowResize);
    const el = this.tabsStrip()?.nativeElement ?? null;
    if (el && typeof ResizeObserver !== 'undefined') {
      this.resizeObserver = new ResizeObserver(() => this.updateScrollArrows());
      this.resizeObserver.observe(el);
    }
  }

  /** Per-tab spinner comes straight from the SSE activity store. */
  tabRunning(id: string): boolean {
    return this.activity.isRunning(id);
  }

  /** Tab label: prefer alias, then session title (or its id when both are unknown). */
  tabLabel(id: string, title: string | null, alias?: string | null): string {
    return alias?.trim() || title?.trim() || id.slice(0, 8);
  }

  closeTab(event: Event, id: string): void {
    event.preventDefault();
    event.stopPropagation();
    this.closeTabById(id);
  }

  closeOnAuxClick(event: MouseEvent, id: string): void {
    if (event.button === 1) {
      event.preventDefault();
      this.closeTabById(id);
    }
  }

  private closeTabById(id: string): void {
    this.tabs.close(id);
    // Closing the tab of the currently viewed chat navigates home.
    if (this.currentSessionId() === id) {
      void this.router.navigate(['/']);
    }
    queueMicrotask(() => this.updateScrollArrows());
  }

  toggleTopbar(): void {
    this.uiPrefs.toggleTopbar();
  }

  scrollTabs(dir: 1 | -1): void {
    this.tabsStrip()?.nativeElement.scrollBy({ left: dir * 240, behavior: 'smooth' });
  }

  onTabsScroll(): void {
    this.updateScrollArrows();
  }

  private readonly onWindowResize = (): void => {
    this.updateScrollArrows();
  };

  private updateScrollArrows(): void {
    const el = this.tabsStrip()?.nativeElement;
    if (!el) {
      this.canScrollLeft.set(false);
      this.canScrollRight.set(false);
      return;
    }
    const tolerance = 2;
    this.canScrollLeft.set(el.scrollLeft > tolerance);
    this.canScrollRight.set(el.scrollLeft + el.clientWidth < el.scrollWidth - tolerance);
  }

  private scrollActiveIntoView(smooth = true): void {
    const strip = this.tabsStrip()?.nativeElement;
    const active = strip?.querySelector<HTMLElement>('.session-link.active');
    active?.scrollIntoView({
      block: 'nearest',
      inline: 'nearest',
      behavior: smooth ? 'smooth' : 'auto',
    });
  }

  private readonly subscription = this.router.events.subscribe((ev) => {
    if (ev instanceof NavigationEnd) {
      this.syncRoute(ev.urlAfterRedirects);
      void this.syncCustomCss();
    }
  });

  private syncRoute(url: string): void {
    const path = url.split('?')[0];
    const match = path.match(/^\/chat\/([^/]+)/);
    this.currentSessionId.set(match ? match[1] : null);
  }

  private async syncCustomCss(): Promise<void> {
    try {
      const url = new URL(this.router.url, 'http://localhost');
      const dir =
        url.searchParams.get('directory') ?? this.engine.readLastDirectory();
      if (!dir || dir === this.lastCssDirectory) {
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
    window.removeEventListener('resize', this.onWindowResize);
    this.resizeObserver?.disconnect();
    this.resizeObserver = null;
  }

  setLanguage(lang: Language): void {
    this.i18n.setLanguage(lang);
  }
}
