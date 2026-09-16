/**
 * "Frontend verification" card (WP-AUTOVERIFY / F8-1), shown in
 * Settings -> Agents.
 *
 * Radio group for `verify.frontend` (`auto` | `ask` | `off`) with help text
 * per mode, plus a scope switch: the policy is a user preference (global
 * config) that a project may override (`<project>/.bebok/config.json`).
 * Saving is a `PUT /config?scope=...` delta `{ verify: { frontend } }`; the
 * engine rebuilds the system prompt section and flips the `browser_*`
 * permission default on the next turn.
 *
 * A separate component (not inlined into `agents-tab.*`) so concurrent work
 * on the Agents tab merges without conflicts.
 *
 * F8-1: the radio group stages a pending selection; an explicit Save button
 * persists the change to the chosen config layer (project by default).
 */

import { Component, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { EngineClient } from '../../core/engine-client.service';
import {
  BUILD_TEST_MODES,
  BuildTestMode,
  FRONTEND_VERIFY_MODES,
  FrontendVerify,
  isBuildTestMode,
  isFrontendVerify,
} from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';
import { SettingsStore } from './settings.store';

/** Engine default when no layer sets `verify.frontend`. */
export const DEFAULT_FRONTEND_VERIFY: FrontendVerify = 'auto';

export type VerifyScope = 'global' | 'project';

/**
 * Read `verify.frontend` out of a raw config-layer JSON text (as served by
 * `GET /config` `files.<layer>.content`). `null` when the layer does not set
 * it or the text is not JSON.
 */
export function layerFrontendVerify(content: string | undefined): FrontendVerify | null {
  if (!content) {
    return null;
  }
  try {
    const parsed = JSON.parse(content) as { verify?: { frontend?: unknown } };
    const raw = parsed?.verify?.frontend;
    return isFrontendVerify(raw) ? raw : null;
  } catch {
    return null;
  }
}

@Component({
  selector: 'app-settings-frontend-verify',
  imports: [FormsModule],
  templateUrl: './frontend-verify.html',
  styleUrls: ['./settings-shared.css', './frontend-verify.css'],
})
export class FrontendVerifyCard {
  private readonly i18n = inject(I18nService);
  private readonly engine = inject(EngineClient);

  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly modes = FRONTEND_VERIFY_MODES;

  /** Where the next save goes ("This project only" by default). */
  readonly scope = signal<VerifyScope>('project');

  /** Effective policy of the loaded (resolved) config. */
  readonly mode = computed<FrontendVerify>(() => {
    const raw = this.store.config()?.config.verify?.frontend;
    return isFrontendVerify(raw) ? raw : DEFAULT_FRONTEND_VERIFY;
  });

  /**
   * Staged (unsaved) selection. When non-null the radio shows this value
   * instead of the persisted `mode()`. Reset to null after a successful save.
   */
  readonly pending = signal<FrontendVerify | null>(null);

  /** True when the user has staged a value different from the persisted one. */
  readonly hasChanges = computed(() => {
    const p = this.pending();
    return p !== null && p !== this.mode();
  });

  /** The project layer sets its own value (overriding the global one). */
  readonly projectOverride = computed<FrontendVerify | null>(() =>
    layerFrontendVerify(this.store.config()?.files?.project?.content),
  );

  /** The mode currently shown on the radio group (pending or persisted). */
  readonly effectiveMode = computed<FrontendVerify>(() => this.pending() ?? this.mode());

  modeLabel(mode: FrontendVerify): string {
    switch (mode) {
      case 'ask':
        return this.t('settings.frontendVerifyAsk');
      case 'off':
        return this.t('settings.frontendVerifyOff');
      default:
        return this.t('settings.frontendVerifyAuto');
    }
  }

  modeHelp(mode: FrontendVerify): string {
    switch (mode) {
      case 'ask':
        return this.t('settings.frontendVerifyAskHelp');
      case 'off':
        return this.t('settings.frontendVerifyOffHelp');
      default:
        return this.t('settings.frontendVerifyAutoHelp');
    }
  }

  setScope(raw: string): void {
    this.scope.set(raw === 'global' ? 'global' : 'project');
  }

  /** Stage a mode selection (does not persist). */
  selectMode(raw: string): void {
    const mode = isFrontendVerify(raw) ? raw : DEFAULT_FRONTEND_VERIFY;
    this.pending.set(mode);
  }

  /** Persist the staged policy to the selected config layer. */
  async save(): Promise<void> {
    const mode = this.pending() ?? this.mode();
    const dir = this.store.directory();
    if (!dir || this.store.saving()) {
      return;
    }
    this.store.saving.set(true);
    this.store.error.set(null);
    this.store.saved.set(null);
    try {
      // Merge the sibling key (`buildTest`) into the delta: `PUT /config`
      // replaces the whole top-level `verify` section, so sending only
      // `{ verify: { frontend } }` would wipe a previously saved `buildTest`.
      const sibling = this.store.config()?.config.verify?.buildTest;
      const verify: { frontend: FrontendVerify; buildTest?: BuildTestMode } = {
        frontend: mode,
      };
      if (isBuildTestMode(sibling)) {
        verify.buildTest = sibling;
      }
      const cfg = await this.engine.putConfig(dir, { verify }, { scope: this.scope() });
      this.store.applyConfig(cfg);
      this.pending.set(null);
      this.store.saved.set(this.t('settings.frontendVerifySaved'));
    } catch (err) {
      this.store.error.set(this.store.describe(err));
    } finally {
      this.store.saving.set(false);
    }
  }
}
