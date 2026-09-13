/**
 * More tab (WP-M2 / F10-8 placeholder; WP-M5 fills it in - Settings-lite,
 * devices/pairing, onboarding). For now: the three screens that already
 * exist as routes under `/m/more/**` plus the empty-state copy.
 */

import { ChangeDetectionStrategy, Component, inject } from '@angular/core';
import { RouterLink } from '@angular/router';

import { I18nService } from '../../../i18n/i18n.service';

@Component({
  selector: 'app-more-tab',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  template: `
    <section class="m-more" data-testid="more-tab">
      <h2>{{ t('mobile.more.emptyTitle') }}</h2>
      <p>{{ t('mobile.more.emptyHint') }}</p>
      <nav class="m-list">
        <a routerLink="/m/more/stats" data-testid="more-stats">{{ t('nav.stats') }}</a>
        <a routerLink="/m/more/settings" data-testid="more-settings">{{ t('nav.settings') }}</a>
        <a routerLink="/m/more/about" data-testid="more-about">{{ t('mobile.more.about') }}</a>
      </nav>
    </section>
  `,
  styles: [
    `
      :host {
        display: flex;
        flex-direction: column;
        flex: 1 1 auto;
        min-height: 0;
        overflow-y: auto;
      }

      .m-more {
        display: flex;
        flex-direction: column;
        gap: var(--space-8);
        padding: var(--space-16);
      }

      .m-more h2 {
        margin: 0;
        font-size: var(--fs-16);
      }

      .m-more p {
        margin: 0 0 var(--space-8);
        color: var(--text-muted);
      }

      .m-list {
        display: flex;
        flex-direction: column;
        border: 1px solid var(--border);
        border-radius: var(--radius-control);
        background: var(--surface);
        overflow: hidden;
      }

      .m-list a {
        display: flex;
        align-items: center;
        min-height: 48px;
        padding: 0 var(--space-16);
        color: var(--text);
        text-decoration: none;
        border-bottom: 1px solid var(--border);
      }

      .m-list a:last-child {
        border-bottom: 0;
      }

      .m-list a:active {
        background: var(--surface-2);
      }
    `,
  ],
})
export class MoreTab {
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);
}
