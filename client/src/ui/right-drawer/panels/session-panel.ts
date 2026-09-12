/**
 * Right-drawer "Session" panel - PLACEHOLDER SLOT.
 *
 * WP-SHELL only builds the drawer shell. The real content (TOKENS grid, COST,
 * FILES CHANGED, SUB-AGENTS and the active mcp/skill chip list) is filled in
 * by WP-CHAT; keep the selector and file path stable so that package only has
 * to replace the template.
 */

import { ChangeDetectionStrategy, Component, inject } from '@angular/core';

import { I18nService } from '../../../i18n/i18n.service';

@Component({
  selector: 'app-session-panel',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `<div class="placeholder">{{ t('drawer.placeholder') }}</div>`,
  styles: [
    `
      .placeholder {
        padding: var(--space-16);
        font-size: var(--fs-12);
        color: var(--text-faint);
        line-height: 1.5;
      }
    `,
  ],
})
export class SessionPanel {
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);
}
