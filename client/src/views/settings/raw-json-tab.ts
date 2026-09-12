/**
 * Raw JSON tab (WP-SETTINGS / F2-29) - the retired standalone Config page.
 *
 * Project/Global layer switcher, a monospace editor with real syntax
 * highlighting (a highlighted layer rendered under a transparent textarea, both
 * sharing the exact same metrics) and inline validation against the config
 * schema: unknown keys and type mismatches are marked on the token itself and
 * listed with their line numbers, not raised as a toast. "Format" pretty-prints
 * the layer, "Save project"/"Save global" replaces the layer file.
 */

import { Component, computed, inject } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { I18nService } from '../../i18n/i18n.service';
import { SettingsStore } from './settings.store';
import { JsonDiagnostic, highlightJson, validateConfig } from './json-highlight';

@Component({
  selector: 'app-settings-raw-json',
  imports: [FormsModule],
  templateUrl: './raw-json-tab.html',
  styleUrls: ['./settings-shared.css', './raw-json-tab.css'],
})
export class RawJsonTab {
  private readonly i18n = inject(I18nService);

  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);

  /** Text of the active layer. */
  readonly text = computed(() =>
    this.store.rawTab() === 'project' ? this.store.rawProjectText() : this.store.rawGlobalText(),
  );

  readonly diagnostics = computed<JsonDiagnostic[]>(() => validateConfig(this.text()));

  readonly errorCount = computed(
    () => this.diagnostics().filter((d) => d.severity === 'error').length,
  );

  readonly highlighted = computed(() => highlightJson(this.text(), this.diagnostics()));

  readonly layerPath = computed(() =>
    this.store.rawTab() === 'project' ? this.store.rawProjectPath() : this.store.rawGlobalPath(),
  );

  readonly layerExists = computed(() =>
    this.store.rawTab() === 'project' ? this.store.rawProjectExists() : this.store.rawGlobalExists(),
  );

  setText(value: string): void {
    this.store.setRawText(value);
  }

  /** Keep the highlighted layer aligned while the textarea scrolls. */
  syncScroll(event: Event): void {
    const area = event.target as HTMLTextAreaElement;
    const pre = area.previousElementSibling as HTMLElement | null;
    if (pre) {
      pre.scrollTop = area.scrollTop;
      pre.scrollLeft = area.scrollLeft;
    }
  }

  /** Pretty-print the active layer (only when it parses). */
  format(): void {
    try {
      const parsed: unknown = JSON.parse(this.text());
      this.store.setRawText(`${JSON.stringify(parsed, null, 2)}\n`);
    } catch (err) {
      this.store.error.set(
        this.i18n.t('settings.invalidRawJson', { msg: this.store.describe(err) }),
      );
    }
  }

  /** Localized text of one diagnostic. */
  message(diagnostic: JsonDiagnostic): string {
    switch (diagnostic.key) {
      case 'parse':
        return this.t('settings.jsonParseError', diagnostic.params);
      case 'unknownKey':
        return this.t('settings.jsonUnknownKey', diagnostic.params);
      case 'typeMismatch':
        return this.t('settings.jsonTypeMismatch', diagnostic.params);
      default:
        return this.t('settings.rawMustBeObject');
    }
  }
}
