/**
 * Permissions tab (WP-SETTINGS / F2-27).
 *
 * The rule table shows the `tool(args)` patterns from `config.permission.rules`
 * in monospace with a colored allow/ask/deny verdict; the rules themselves come
 * from the permission engine's own config section (nothing is invented here,
 * the old raw-JSON textarea is only presented properly). Below it sits the
 * visually separate, danger-tinted "YOLO mode" card.
 */

import { Component, inject } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { I18nService } from '../../i18n/i18n.service';
import { PermissionRule, SettingsStore } from './settings.store';

@Component({
  selector: 'app-settings-permissions',
  imports: [FormsModule],
  templateUrl: './permissions-tab.html',
  styleUrls: ['./settings-shared.css', './permissions-tab.css'],
})
export class PermissionsTab {
  private readonly i18n = inject(I18nService);

  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly actions: Array<PermissionRule['action']> = ['allow', 'ask', 'deny'];

  actionLabel(action: PermissionRule['action']): string {
    switch (action) {
      case 'allow':
        return this.t('settings.ruleAllow');
      case 'deny':
        return this.t('settings.ruleDeny');
      default:
        return this.t('settings.ruleAsk');
    }
  }
}
