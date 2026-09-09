import { Component, OnDestroy, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import {
  NavigationEnd,
  Router,
  RouterLink,
  RouterLinkActive,
  RouterOutlet,
} from '@angular/router';

import { EventsStore } from '../core/events.store';
import { OpenSessionsStore } from '../core/open-sessions.store';
import { SessionActivityStore } from '../core/session-activity.store';
import { I18nService } from '../i18n/i18n.service';
import { LANGUAGES, type Language } from '../i18n';

@Component({
  selector: 'app-root',
  imports: [RouterOutlet, RouterLink, RouterLinkActive, FormsModule],
  templateUrl: './app.html',
  styleUrl: './app.css',
})
export class App implements OnDestroy {
  readonly events = inject(EventsStore);
  readonly activity = inject(SessionActivityStore);
  readonly tabs = inject(OpenSessionsStore);
  readonly i18n = inject(I18nService);
  private readonly router = inject(Router);

  readonly languages = LANGUAGES;
  readonly currentLang = this.i18n.lang;

  readonly t = this.i18n.t.bind(this.i18n);

  /** The chat currently open (synced from the route) - no session switching here. */
  readonly currentSessionId = signal<string | null>(null);

  /** Per-tab spinner comes straight from the SSE activity store. */
  tabRunning(id: string): boolean {
    return this.activity.isRunning(id);
  }

  /** Tab label: session title (or its id when the title is unknown). */
  tabLabel(id: string, title: string | null): string {
    return title?.trim() || id.slice(0, 8);
  }

  closeTab(event: Event, id: string): void {
    event.preventDefault();
    event.stopPropagation();
    this.tabs.close(id);
    // Closing the tab of the currently viewed chat navigates home.
    if (this.currentSessionId() === id) {
      void this.router.navigate(['/']);
    }
  }

  private readonly subscription = this.router.events.subscribe((ev) => {
    if (ev instanceof NavigationEnd) {
      const path = ev.urlAfterRedirects.split('?')[0];
      const match = path.match(/^\/chat\/([^/]+)/);
      this.currentSessionId.set(match ? match[1] : null);
    }
  });

  ngOnDestroy(): void {
    this.subscription.unsubscribe();
  }

  setLanguage(lang: Language): void {
    this.i18n.setLanguage(lang);
  }
}
