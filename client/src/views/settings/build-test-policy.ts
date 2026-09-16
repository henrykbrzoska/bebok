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
 *
 * The radio group stages a pending selection; an explicit Save button persists
 * the change to the chosen config layer (project by default).
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

  /** Where the next save goes ("This project only" by default). */
  readonly scope = signal<BuildTestScope>('project');

  /** Effective policy of the loaded (resolved) config. */
  readonly mode = computed<BuildTestMode>(() => {
    const raw = this.store.config()?.config.verify?.buildTest;
    return isBuildTestMode(raw) ? raw : DEFAULT_BUILD_TEST_MODE;
  });

  /**
   * Staged (unsaved) selection. When non-null the radio shows this value
   * instead of the persisted `mode()`. Reset to null after a successful save.
   */
  readonly pending = signal<BuildTestMode | null>(null);

  /** True when the user has staged a value different from the persisted one. */
  readonly hasChanges = computed(() => {
    const p = this.pending();
    return p !== null && p !== this.mode();
  });

  /** The project layer sets its own value (overriding the global one). */
  readonly projectOverride = computed<BuildTestMode | null>(() =>
    layerBuildTest(this.store.config()?.files?.project?.content),
  );

  /** The mode currently shown on the radio group (pending or persisted). */
  readonly effectiveMode = computed<BuildTestMode>(() => this.pending() ?? this.mode());

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
    this.scope.set(raw === 'global' ? 'global' : 'project');
  }

  /** Stage a mode selection (does not persist). */
  selectMode(raw: string): void {
    const mode = isBuildTestMode(raw) ? raw : DEFAULT_BUILD_TEST_MODE;
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
      // Merge the sibling key (`frontend`) into the delta: `PUT /config`
      // replaces the whole top-level `verify` section, so sending only
      // `{ verify: { buildTest } }` would wipe a previously saved `frontend`.
      const sibling = this.store.config()?.config.verify?.frontend;
      const verify: { buildTest: BuildTestMode; frontend?: FrontendVerify } = {
        buildTest: mode,
      };
      if (isFrontendVerify(sibling)) {
        verify.frontend = sibling;
      }
      const cfg = await this.engine.putConfig(dir, { verify }, { scope: this.scope() });
      this.store.applyConfig(cfg);
      this.pending.set(null);
      this.store.saved.set(this.t('settings.buildTestSaved'));
    } catch (err) {
      this.store.error.set(this.store.describe(err));
    } finally {
      this.store.saving.set(false);
    }
  }
}
