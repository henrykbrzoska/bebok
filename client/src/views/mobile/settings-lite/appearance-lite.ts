/**
 * Appearance section of Settings-lite (WP-M5 / F10-19): UI language
 * (`I18nService`), the tool-call expansion preference (`UiPrefsStore`) and
 * custom CSS (`config.ui.customCss`, saved to the global layer). Runtime
 * executable paths stay desktop-only.
 */

import { ChangeDetectionStrategy, Component, inject } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { CustomCssService } from '../../../core/custom-css.service';
import { UiPrefsStore } from '../../../core/ui-prefs.store';
import type { Language } from '../../../i18n';
import { I18nService } from '../../../i18n/i18n.service';
import { SettingsStore } from '../../settings/settings.store';
import { GlobalSaver, LITE_STYLES } from './lite-shared';

@Component({
  selector: 'app-settings-lite-appearance',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  template: `
    <div class="card" data-testid="appearance-lite-language">
      <label class="field">
        <span>{{ t('sidebar.language') }}</span>
        <select [ngModel]="i18n.lang()" (ngModelChange)="setLanguage($event)" data-testid="appearance-lite-lang">
          @for (option of i18n.languages(); track option.code) {
            <option [value]="option.code">{{ option.label }}</option>
          }
        </select>
      </label>
    </div>

    <div class="card">
      <h3>{{ t('settings.toolCallsTitle') }}</h3>
      <label class="toggle">
        <input
          type="checkbox"
          [checked]="prefs.expandToolCallsByDefault()"
          (change)="prefs.setExpandToolCallsByDefault($any($event.target).checked)"
          data-testid="appearance-lite-expand"
        />
        <span>{{ t('settings.expandToolCalls') }}</span>
      </label>
    </div>

    <div class="card">
      <h3>{{ t('settings.appearanceTitle') }}</h3>
      <label class="field">
        <span>{{ t('settings.customCss') }}</span>
        <textarea
          rows="6"
          [ngModel]="store.customCssText()"
          (ngModelChange)="store.customCssText.set($event)"
          spellcheck="false"
          placeholder="/* .m-topbar { background: #1a1d27; } */"
          data-testid="appearance-lite-css"
        ></textarea>
      </label>
      <button
        type="button"
        class="btn-primary"
        (click)="save()"
        [disabled]="store.saving()"
        data-testid="appearance-lite-save"
      >
        {{ t('settings.saveAppearance') }}
      </button>
    </div>
  `,
  styles: [LITE_STYLES],
})
export class AppearanceLite {
  private readonly customCss = inject(CustomCssService);
  private readonly saver = new GlobalSaver();

  readonly i18n = inject(I18nService);
  readonly prefs = inject(UiPrefsStore);
  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);

  setLanguage(code: string): void {
    this.i18n.setLanguage(code as Language);
  }

  async save(): Promise<void> {
    const files = this.store
      .customCssFilesText()
      .split('\n')
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
    const ok = await this.saver.save(
      { ui: { customCss: this.store.customCssText(), customCssFiles: files } },
      'settings.savedAppearance',
    );
    const dir = this.store.directory();
    if (ok && dir) {
      await this.customCss.sync(dir);
    }
  }
}
