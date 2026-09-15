/**
 * "Build & test policy" card, shown in Settings -> Agents below the
 * "Frontend verification" card.
 *
 * Radio group for `verify.buildTest` (`auto` | `ask` | `off`) with help text
 * per mode, plus the same scope switch as the frontend card: the policy is a
 * user preference (global config) that a project may override
 * (`<project>/.bebok/config.json`). Saving is a `PUT /config?scope=...` delta
 * `{ verify: { buildTest } }`; the engine rebuilds the system-prompt section
 * (`agent::build_test_section`) on the next turn.
 *
 * A separate component (not inlined into `agents-tab.*`) so concurrent work on
 * the Agents tab merges without conflicts.
 */

import { Component, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { EngineClient } from '../../core/engine-client.service';
import { BUILD_TEST_MODES, BuildTestMode, isBuildTestMode } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';
import { SettingsStore } from './settings.store';

/** Engine default when no layer sets `verify.buildTest`. */
export const DEFAULT_BUILD_TEST_MODE: BuildTestMode = 'auto';

export type BuildTestScope = 'global' | 'project';

/**
 * Read `verify.buildTest` out of a raw config-layer JSON text (as served by
 * `GET /config` `files.<layer>.content`). `null` when the layer does not set
 * it or the text is not JSON.
 */
export function layerBuildTest(content: string | undefined): BuildTestMode | null {
  if (!content) {
    return null;
  }
  try {
    const parsed = JSON.parse(content) as { verify?: { buildTest?: unknown } };
    const raw = parsed?.verify?.buildTest;
    return isBuildTestMode(raw) ? raw : null;
  } catch {
    return null;
  }
}

@Component({
  selector: 'app-settings-build-test-policy',
  imports: [FormsModule],
  templateUrl: './build-test-policy.html',
  styleUrls: ['./settings-shared.css', './build-test-policy.css'],
})
export class BuildTestPolicyCard {
  private readonly i18n = inject(I18nService);
  private readonly engine = inject(EngineClient);

  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly modes = BUILD_TEST_MODES;

  /** Where the next save goes. */
  readonly scope = signal<BuildTestScope>('global');

  /** Effective policy of the loaded (resolved) config. */
  readonly mode = computed<BuildTestMode>(() => {
    const raw = this.store.config()?.config.verify?.buildTest;
    return isBuildTestMode(raw) ? raw : DEFAULT_BUILD_TEST_MODE;
  });

  /** The project layer sets its own value (overriding the global one). */
  readonly projectOverride = computed<BuildTestMode | null>(() =>
    layerBuildTest(this.store.config()?.files?.project?.content),
  );

  modeLabel(mode: BuildTestMode): string {
    switch (mode) {
      case 'ask':
        return this.t('settings.buildTestAsk');
      case 'off':
        return this.t('settings.buildTestOff');
      default:
        return this.t('settings.buildTestAuto');
    }
  }

  modeHelp(mode: BuildTestMode): string {
    switch (mode) {
      case 'ask':
        return this.t('settings.buildTestAskHelp');
      case 'off':
        return this.t('settings.buildTestOffHelp');
      default:
        return this.t('settings.buildTestAutoHelp');
    }
  }

  setScope(raw: string): void {
    this.scope.set(raw === 'project' ? 'project' : 'global');
  }

  /** Persist the chosen policy to the selected config layer. */
  async setMode(raw: string): Promise<void> {
    const mode = isBuildTestMode(raw) ? raw : DEFAULT_BUILD_TEST_MODE;
    const dir = this.store.directory();
    if (!dir || this.store.saving()) {
      return;
    }
    this.store.saving.set(true);
    this.store.error.set(null);
    this.store.saved.set(null);
    try {
      const cfg = await this.engine.putConfig(
        dir,
        { verify: { buildTest: mode } },
        { scope: this.scope() },
      );
      this.store.applyConfig(cfg);
      this.store.saved.set(this.t('settings.buildTestSaved'));
    } catch (err) {
      this.store.error.set(this.store.describe(err));
    } finally {
      this.store.saving.set(false);
    }
  }
}
