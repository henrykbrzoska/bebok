/**
 * Changes tab (WP-M2 / F10-8 placeholder). WP-M6 replaces the body with the
 * real screen; keep this file the only one it edits for this tab.
 */

import { ChangeDetectionStrategy, Component, inject } from '@angular/core';

import { I18nService } from '../../../i18n/i18n.service';

@Component({
  selector: 'app-changes-tab',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <section class="m-empty" data-testid="changes-tab-empty">
      <h2>{{ t('mobile.changes.emptyTitle') }}</h2>
      <p>{{ t('mobile.changes.emptyHint') }}</p>
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

      .m-empty {
        flex: 1 1 auto;
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        gap: var(--space-8);
        padding: var(--space-24);
        text-align: center;
      }

      .m-empty h2 {
        margin: 0;
        font-size: var(--fs-16);
      }

      .m-empty p {
        margin: 0;
        color: var(--text-muted);
      }
    `,
  ],
})
export class ChangesTab {
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);
}
