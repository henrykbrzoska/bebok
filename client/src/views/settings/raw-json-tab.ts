/**
 * Raw JSON tab (WP-SETTINGS / F2-29) - the retired standalone Config page.
 *
 * Two columns: a clickable/editable JSON tree on the left (easy editor) and
 * the real raw JSON text on the right. The raw TEXT per layer (in
 * SettingsStore) stays the single source of truth:
 * - tree -> text: tree edits are always valid JSON; stringify into the store.
 * - text -> tree: when the raw text parses and differs from the tree state,
 *   push it; while the raw text is broken the tree keeps the last-good state.
 */

import { Component, computed, effect, inject, viewChild } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { I18nService } from '../../i18n/i18n.service';
import { SettingsStore } from './settings.store';
import { TreeJsonEditor } from './tree-json-editor';
import { JsonDiagnostic, highlightJson, validateConfig } from './json-highlight';

@Component({
  selector: 'app-settings-raw-json',
  imports: [FormsModule, TreeJsonEditor],
  templateUrl: './raw-json-tab.html',
  styleUrls: ['./settings-shared.css', './raw-json-tab.css'],
})
export class RawJsonTab {
  private readonly i18n = inject(I18nService);

  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);
  private readonly tree = viewChild(TreeJsonEditor);

  /** Text of the active layer. */
  readonly text = computed(() =>
    this.store.rawTab() === 'project' ? this.store.rawProjectText() : this.store.rawGlobalText(),
  );

  readonly diagnostics = computed<JsonDiagnostic[]>(() => validateConfig(this.text()));

  readonly errorCount = computed(
    () => this.diagnostics().filter((d) => d.severity === 'error').length,
  );

  /** Raw text is parseable — the tree shows live state. */
  readonly treeLive = computed(() => {
    try {
      JSON.parse(this.text());
      return true;
    } catch {
      return false;
    }
  });

  /** Parsed object for the tree (last-good value while raw text is broken). */
  private lastGood: unknown = {};
  readonly treeValue = computed<unknown>(() => {
    try {
      this.lastGood = JSON.parse(this.text());
    } catch {
      /* keep last-good while the user is mid-edit */
    }
    return this.lastGood;
  });

  readonly highlighted = computed(() => highlightJson(this.text(), this.diagnostics()));

  readonly layerPath = computed(() =>
    this.store.rawTab() === 'project' ? this.store.rawProjectPath() : this.store.rawGlobalPath(),
  );

  readonly layerExists = computed(() =>
    this.store.rawTab() === 'project' ? this.store.rawProjectExists() : this.store.rawGlobalExists(),
  );

  /** Debounce timer for tree -> text sync. */
  private treeTimer: ReturnType<typeof setTimeout> | undefined;

  constructor() {
    // Text -> tree: push freshly parsed raw text (layer switches included).
    // TreeJsonEditor.setValue() no-ops on identical values, so no echo loop.
    effect(() => {
      const value = this.treeValue();
      this.tree()?.setValue(value);
    });
  }

  setText(value: string): void {
    this.store.setRawText(value);
  }

  /** Tree -> text: stringify the edited object back into the store. */
  onTreeChanged(value: unknown): void {
    if (this.treeTimer !== undefined) clearTimeout(this.treeTimer);
    this.treeTimer = setTimeout(() => {
      this.lastGood = value;
      this.store.setRawText(`${JSON.stringify(value, null, 2)}\n`);
    }, 300);
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
