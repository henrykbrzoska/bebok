/**
 * Settings-lite (WP-M5 / F10-19): the phone's Settings as list → detail
 * navigation (plan §6). Only four sections - Providers, Agents, Appearance
 * and "This device" - Remote/pairing management lives in WP-M4/WP-M6.
 *
 * Routes: `/m/more/settings` (the list) and `/m/more/settings/:section` (one
 * section, full screen, with a back button that returns to the list - never
 * out of Settings). The sections reuse `SettingsStore` (provided here, like
 * the desktop `SettingsView` does) and `ProviderCatalog`; only the shell is
 * new. Saves go to the **global** config layer: a phone has one user and its
 * sessions live in several scratch/project directories, so a per-project
 * key would be lost the moment a quick session starts elsewhere.
 */

import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
  untracked,
} from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { map } from 'rxjs';

import { EngineClient } from '../../../core/engine-client.service';
import { EventsStore } from '../../../core/events.store';
import { I18nService } from '../../../i18n/i18n.service';
import type { MessageKey } from '../../../i18n';
import { ProviderCatalog } from '../../settings/provider-catalog';
import { SettingsStore } from '../../settings/settings.store';
import { AgentsLite } from './agents-lite';
import { AppearanceLite } from './appearance-lite';
import { DeviceLite } from './device-lite';
import { ProvidersLite } from './providers-lite';

export type LiteSection = 'providers' | 'agents' | 'appearance' | 'device';

export const LITE_SECTIONS: readonly LiteSection[] = ['providers', 'agents', 'appearance', 'device'];

export function isLiteSection(value: unknown): value is LiteSection {
  return typeof value === 'string' && (LITE_SECTIONS as readonly string[]).includes(value);
}

@Component({
  selector: 'app-settings-lite',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink, ProvidersLite, AgentsLite, AppearanceLite, DeviceLite],
  providers: [SettingsStore],
  template: `
    <section class="sl" data-testid="settings-lite">
      <header class="sl-head">
        @if (section(); as current) {
          <button
            type="button"
            class="sl-back"
            (click)="back()"
            [attr.aria-label]="t('mobile.settings.back')"
            data-testid="settings-lite-back"
          >‹</button>
          <h2>{{ t(sectionLabel(current)) }}</h2>
        } @else {
          <a class="sl-back" routerLink="/m/more" [attr.aria-label]="t('mobile.settings.back')">‹</a>
          <h2>{{ t('settings.title') }}</h2>
        }
      </header>

      @if (store.error(); as err) {
        <div class="sl-banner error" role="alert" data-testid="settings-lite-error">{{ err }}</div>
      }
      @if (store.saved(); as msg) {
        <div class="sl-banner saved" role="status" data-testid="settings-lite-saved">{{ msg }}</div>
      }

      <div class="sl-body">
        @switch (section()) {
          @case ('providers') {
            <app-settings-lite-providers />
          }
          @case ('agents') {
            <app-settings-lite-agents />
          }
          @case ('appearance') {
            <app-settings-lite-appearance />
          }
          @case ('device') {
            <app-settings-lite-device />
          }
          @default {
            <nav class="sl-list" data-testid="settings-lite-list">
              @for (entry of sections; track entry) {
                <a [routerLink]="['/m/more/settings', entry]" [attr.data-testid]="'settings-lite-' + entry">
                  <span>{{ t(sectionLabel(entry)) }}</span>
                  <span class="sl-chevron" aria-hidden="true">›</span>
                </a>
              }
            </nav>
            @if (store.directory(); as dir) {
              <p class="sl-dir mono" data-testid="settings-lite-directory">{{ dir }}</p>
            }
          }
        }
      </div>
    </section>
  `,
  styles: [
    `
      :host {
        display: flex;
        flex-direction: column;
        flex: 1 1 auto;
        min-height: 0;
      }

      .sl {
        display: flex;
        flex-direction: column;
        flex: 1 1 auto;
        min-height: 0;
      }

      .sl-head {
        flex: none;
        display: flex;
        align-items: center;
        gap: var(--space-4);
        min-height: 44px;
        padding: 0 var(--space-8);
        border-bottom: 1px solid var(--border);
      }

      .sl-head h2 {
        margin: 0;
        font-size: var(--fs-16);
        font-weight: 600;
      }

      .sl-back {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 40px;
        height: 40px;
        border: 0;
        border-radius: var(--radius-control);
        background: transparent;
        color: var(--accent);
        font-size: 24px;
        line-height: 1;
        text-decoration: none;
        cursor: pointer;
        -webkit-tap-highlight-color: transparent;
      }

      .sl-banner {
        flex: none;
        padding: var(--space-6) var(--space-16);
        font-size: var(--fs-12-5);
        border-bottom: 1px solid var(--border);
        overflow-wrap: anywhere;
      }

      .sl-banner.error {
        color: var(--danger);
      }

      .sl-banner.saved {
        color: var(--success, var(--accent));
      }

      .sl-body {
        flex: 1 1 auto;
        min-height: 0;
        overflow-y: auto;
        padding: var(--space-16);
      }

      .sl-list {
        display: flex;
        flex-direction: column;
        border: 1px solid var(--border);
        border-radius: var(--radius-control);
        background: var(--surface);
        overflow: hidden;
      }

      .sl-list a {
        display: flex;
        align-items: center;
        justify-content: space-between;
        min-height: 48px;
        padding: 0 var(--space-16);
        color: var(--text);
        text-decoration: none;
        border-bottom: 1px solid var(--border);
      }

      .sl-list a:last-child {
        border-bottom: 0;
      }

      .sl-list a:active {
        background: var(--surface-2);
      }

      .sl-chevron {
        color: var(--text-muted);
        font-size: 20px;
      }

      .sl-dir {
        margin: var(--space-12) 0 0;
        font-size: var(--fs-12);
        color: var(--text-muted);
        overflow-wrap: anywhere;
      }

      .mono {
        font-family: var(--font-mono);
      }
    `,
  ],
})
export class SettingsLiteView implements OnInit {
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  private readonly i18n = inject(I18nService);
  private readonly catalog = inject(ProviderCatalog);

  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);
  readonly sections = LITE_SECTIONS;

  private readonly sectionParam = toSignal(
    this.route.paramMap.pipe(map((p) => p.get('section'))),
    { initialValue: this.route.snapshot.paramMap.get('section') },
  );

  /** The open section, or null for the list. */
  readonly section = computed<LiteSection | null>(() => {
    const raw = this.sectionParam();
    return isLiteSection(raw) ? raw : null;
  });

  async ngOnInit(): Promise<void> {
    const directory = this.engine.readLastDirectory();
    if (!this.engine.connected()) {
      try {
        await this.engine.connect();
      } catch (err) {
        this.store.error.set(this.store.describe(err));
        return;
      }
    }
    this.events.start();
    void this.catalog.load();
    await this.store.load(directory);
  }

  sectionLabel(section: LiteSection): MessageKey {
    switch (section) {
      case 'providers':
        return 'settings.tab.providers';
      case 'agents':
        return 'settings.tab.agents';
      case 'appearance':
        return 'settings.tab.appearance';
      default:
        return 'mobile.settings.device';
    }
  }

  /** Back from a section returns to the list, never out of Settings. */
  back(): void {
    untracked(() => {
      this.store.error.set(null);
      this.store.saved.set(null);
    });
    void this.router.navigate(['/m/more/settings']);
  }
}
