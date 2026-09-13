/**
 * Shared bits of the Settings-lite sections (WP-M5 / F10-19): the mobile
 * form styles and the global-scope save helper the sections use instead of
 * `SettingsStore`'s project-scoped writers.
 */

import { inject } from '@angular/core';

import { EngineClient } from '../../../core/engine-client.service';
import { I18nService } from '../../../i18n/i18n.service';
import type { MessageKey } from '../../../i18n';
import { SettingsStore } from '../../settings/settings.store';

export const LITE_STYLES = `
  :host {
    display: flex;
    flex-direction: column;
    gap: var(--space-12);
  }

  .card {
    display: flex;
    flex-direction: column;
    gap: var(--space-10);
    padding: var(--space-12);
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    background: var(--surface);
  }

  .card h3 {
    margin: 0;
    font-size: var(--fs-14, 14px);
  }

  .hint {
    margin: 0;
    font-size: var(--fs-12);
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    font-size: var(--fs-12);
    color: var(--text-muted);
  }

  .field input,
  .field select,
  .field textarea {
    min-height: 44px;
    padding: 0 var(--space-12);
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    background: var(--bg);
    color: var(--text);
    font: inherit;
  }

  .field textarea {
    padding: var(--space-8) var(--space-12);
    font-family: var(--font-mono);
    font-size: var(--fs-12);
  }

  .mono {
    font-family: var(--font-mono);
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--space-8);
    flex-wrap: wrap;
  }

  .toggle {
    display: flex;
    align-items: center;
    gap: var(--space-10);
    min-height: 44px;
    font-size: var(--fs-13, 13px);
  }

  .toggle input {
    width: 20px;
    height: 20px;
  }

  .btn,
  .btn-primary {
    min-height: 44px;
    padding: 0 var(--space-16);
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    background: var(--surface-2);
    color: var(--text);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
    -webkit-tap-highlight-color: transparent;
  }

  .btn-primary {
    border-color: var(--accent);
    background: var(--accent);
    color: var(--bg);
  }

  .btn:disabled,
  .btn-primary:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .list {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: var(--radius-control);
    background: var(--surface);
    overflow: hidden;
  }

  .list button {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-8);
    min-height: 48px;
    padding: 0 var(--space-16);
    border: 0;
    border-bottom: 1px solid var(--border);
    background: transparent;
    color: var(--text);
    font: inherit;
    text-align: left;
    cursor: pointer;
    -webkit-tap-highlight-color: transparent;
  }

  .list button:last-child {
    border-bottom: 0;
  }

  .list button:active {
    background: var(--surface-2);
  }

  .status {
    font-size: var(--fs-12);
    color: var(--text-muted);
  }

  .status.ok {
    color: var(--success, var(--accent));
  }

  .status.warn {
    color: var(--warn, var(--danger));
  }

  .error {
    margin: 0;
    color: var(--danger);
    font-size: var(--fs-12-5);
    overflow-wrap: anywhere;
  }

  .link {
    align-self: flex-start;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--accent);
    font: inherit;
    cursor: pointer;
  }
`;

/**
 * Save a config delta to the **global** layer through `PUT /config?scope=global`
 * and reload the store. Mirrors `SettingsStore`'s save helpers (busy flag,
 * banners) but for the phone's single-user, many-directories situation.
 */
export class GlobalSaver {
  private readonly engine = inject(EngineClient);
  private readonly i18n = inject(I18nService);
  private readonly store = inject(SettingsStore);

  async save(delta: unknown, savedKey: MessageKey): Promise<boolean> {
    const dir = this.store.directory();
    if (!dir || this.store.saving()) {
      return false;
    }
    this.store.saving.set(true);
    this.store.error.set(null);
    this.store.saved.set(null);
    try {
      await this.engine.putConfig(dir, delta, { scope: 'global' });
      this.store.saved.set(this.i18n.t(savedKey));
      await this.store.reload();
      return true;
    } catch (err) {
      this.store.error.set(this.store.describe(err));
      return false;
    } finally {
      this.store.saving.set(false);
    }
  }
}
