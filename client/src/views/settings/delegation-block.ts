/**
 * WP-DELEGATION (F8-2): the "Delegation" block of Settings -> Agents.
 *
 * Its own component (not part of `AgentsTab`) so it can be dropped into the
 * tab with one tag and evolve independently. It edits the `delegation`
 * config section - `mode` (off / auto / always), `max_concurrent` and an
 * optional sub-agent `model` - through the same `PUT /config` delta every
 * other Settings block uses. The section is global config with a per-key
 * project override: the scope selector says where a change is written, and
 * a note tells the user when the project layer overrides the global one
 * (with a one-click "remove project override").
 */

import { ChangeDetectionStrategy, Component, computed, effect, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { EngineClient } from '../../core/engine-client.service';
import { DelegationConfig, DelegationMode } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';
import { MessageKey } from '../../i18n';
import { SettingsStore } from './settings.store';

export const DELEGATION_MODES: readonly DelegationMode[] = ['off', 'auto', 'always'];
export const DEFAULT_DELEGATION: DelegationConfig = { mode: 'auto', max_concurrent: 3, model: null };
export const MAX_CONCURRENT_LIMIT = 16;

const MODE_LABEL: Record<DelegationMode, MessageKey> = {
  off: 'settings.delegationModeOff',
  auto: 'settings.delegationModeAuto',
  always: 'settings.delegationModeAlways',
};

const MODE_HINT: Record<DelegationMode, MessageKey> = {
  off: 'settings.delegationModeOffHint',
  auto: 'settings.delegationModeAutoHint',
  always: 'settings.delegationModeAlwaysHint',
};

type Scope = 'project' | 'global';

/** Parse one config layer's `delegation` section (raw JSON text from `files`). */
export function readLayerDelegation(json: string): Partial<DelegationConfig> | null {
  try {
    const parsed = JSON.parse(json) as { delegation?: unknown };
    const raw = parsed?.delegation;
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
      return null;
    }
    const d = raw as Record<string, unknown>;
    const out: Partial<DelegationConfig> = {};
    if (typeof d['mode'] === 'string' && DELEGATION_MODES.includes(d['mode'] as DelegationMode)) {
      out.mode = d['mode'] as DelegationMode;
    }
    const n = d['max_concurrent'] ?? d['maxConcurrent'];
    if (typeof n === 'number' && n > 0) {
      out.max_concurrent = n;
    }
    if ('model' in d) {
      out.model = typeof d['model'] === 'string' ? d['model'] : null;
    }
    return out;
  } catch {
    return null;
  }
}

@Component({
  selector: 'app-settings-delegation',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  styleUrls: ['./settings-shared.css'],
  template: `
    <div class="card delegation" data-testid="delegation-card">
      <h3>{{ t('settings.delegationTitle') }}</h3>
      <p class="card-sub">{{ t('settings.delegationHint') }}</p>

      <fieldset class="modes" [disabled]="store.saving()">
        <legend class="micro-label">{{ t('settings.delegationMode') }}</legend>
        @for (mode of modes; track mode) {
          <label class="mode" [class.selected]="effective().mode === mode">
            <input
              type="radio"
              name="delegation-mode"
              [value]="mode"
              [checked]="effective().mode === mode"
              (change)="setMode(mode)"
              [attr.data-testid]="'delegation-mode-' + mode"
            />
            <span class="mode-text">
              <span class="mode-name">{{ modeLabel(mode) }}</span>
              <span class="mode-hint small faint">{{ modeHint(mode) }}</span>
            </span>
          </label>
        }
      </fieldset>

      <div class="grid-2">
        <label class="field">
          <span class="micro-label">{{ t('settings.delegationMaxConcurrent') }}</span>
          <input
            type="number"
            min="1"
            [max]="maxLimit"
            step="1"
            class="mono"
            data-testid="delegation-max-concurrent"
            [ngModel]="maxDraft()"
            (ngModelChange)="maxDraft.set($event)"
            (blur)="commitMax()"
            (keydown.enter)="commitMax()"
            [disabled]="store.saving()"
          />
          <span class="small faint">{{ t('settings.delegationMaxConcurrentHint') }}</span>
        </label>

        <label class="field">
          <span class="micro-label">{{ t('settings.delegationModel') }}</span>
          <select
            class="mono"
            data-testid="delegation-model"
            [ngModel]="effective().model ?? ''"
            (ngModelChange)="setModel($event)"
            [disabled]="store.saving()"
          >
            <option value="">{{ t('settings.delegationModelDefault') }}</option>
            @if (effective().model && !store.availableModels().includes(effective().model!)) {
              <option [value]="effective().model">{{ effective().model }}</option>
            }
            @for (model of store.availableModels(); track model) {
              <option [value]="model">{{ model }}</option>
            }
          </select>
          <span class="small faint">{{ t('settings.delegationModelHint') }}</span>
        </label>
      </div>

      <div class="scope-row">
        <label class="field scope">
          <span class="micro-label">{{ t('settings.delegationScope') }}</span>
          <select
            data-testid="delegation-scope"
            [ngModel]="scope()"
            (ngModelChange)="scope.set($event)"
            [disabled]="store.saving()"
          >
            <option value="project">{{ t('settings.delegationScopeProject') }}</option>
            <option value="global">{{ t('settings.delegationScopeGlobal') }}</option>
          </select>
        </label>
        @if (projectOverride(); as override) {
          <div class="note override" data-testid="delegation-override-note">
            <span>{{ t('settings.delegationOverrideNote', { keys: overrideKeys(override) }) }}</span>
            <button type="button" (click)="removeProjectOverride()" [disabled]="store.saving()">
              {{ t('settings.delegationRemoveOverride') }}
            </button>
          </div>
        } @else {
          <span class="small faint">{{ t('settings.delegationNoOverride') }}</span>
        }
      </div>

      <p class="small faint tools-note">{{ t('settings.delegationToolsNote') }}</p>
    </div>
  `,
  styles: [
    `
      .modes {
        border: none;
        margin: 0 0 var(--space-12);
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-6);
      }
      .modes legend {
        margin-bottom: var(--space-6);
        padding: 0;
      }
      .mode {
        display: flex;
        align-items: flex-start;
        gap: var(--space-8);
        padding: var(--space-6) var(--space-8);
        border: 1px solid var(--border);
        border-radius: var(--radius-control-sm);
        cursor: pointer;
      }
      .mode.selected {
        border-color: var(--accent);
        background: var(--surface-2);
      }
      .mode input {
        margin-top: 3px;
        flex: none;
      }
      .mode-text {
        display: flex;
        flex-direction: column;
        gap: 2px;
        min-width: 0;
      }
      .mode-name {
        font-size: var(--fs-12-5);
        font-weight: 600;
      }
      .scope-row {
        display: flex;
        align-items: flex-end;
        gap: var(--space-12);
        flex-wrap: wrap;
        margin-top: var(--space-8);
      }
      .scope {
        flex: 0 0 200px;
      }
      .override {
        display: flex;
        align-items: center;
        gap: var(--space-8);
        flex: 1 1 auto;
      }
      .tools-note {
        margin: var(--space-12) 0 0;
      }
    `,
  ],
})
export class DelegationBlock {
  private readonly i18n = inject(I18nService);
  private readonly engine = inject(EngineClient);
  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly modes = DELEGATION_MODES;
  readonly maxLimit = MAX_CONCURRENT_LIMIT;

  /** Where the next change is written. */
  readonly scope = signal<Scope>('project');

  /** Effective (resolved) section from `GET /config`. */
  readonly effective = computed<DelegationConfig>(() => {
    const raw = this.store.config()?.config.delegation;
    return {
      mode: raw?.mode && DELEGATION_MODES.includes(raw.mode) ? raw.mode : DEFAULT_DELEGATION.mode,
      max_concurrent:
        typeof raw?.max_concurrent === 'number' && raw.max_concurrent > 0
          ? raw.max_concurrent
          : DEFAULT_DELEGATION.max_concurrent,
      model: raw?.model?.trim() ? raw.model : null,
    };
  });

  /** Keys the project layer sets (null when it has no `delegation` section). */
  readonly projectOverride = computed<Partial<DelegationConfig> | null>(() => {
    const layer = readLayerDelegation(this.store.rawProjectText());
    return layer && Object.keys(layer).length > 0 ? layer : null;
  });

  /** Draft of the numeric field (committed on blur / Enter). */
  readonly maxDraft = signal<number>(DEFAULT_DELEGATION.max_concurrent);

  constructor() {
    // Keep the draft in sync with the loaded value (a reload after save must
    // not leave a stale number in the box).
    effect(() => {
      this.maxDraft.set(this.effective().max_concurrent);
    });
  }

  modeLabel(mode: DelegationMode): string {
    return this.t(MODE_LABEL[mode]);
  }

  modeHint(mode: DelegationMode): string {
    return this.t(MODE_HINT[mode]);
  }

  overrideKeys(override: Partial<DelegationConfig>): string {
    return Object.keys(override).join(', ');
  }

  setMode(mode: DelegationMode): void {
    if (mode !== this.effective().mode) {
      void this.save({ mode });
    }
  }

  commitMax(): void {
    const raw = Number(this.maxDraft());
    const value = Number.isFinite(raw) ? Math.min(this.maxLimit, Math.max(1, Math.round(raw))) : 1;
    this.maxDraft.set(value);
    if (value !== this.effective().max_concurrent) {
      void this.save({ max_concurrent: value });
    }
  }

  setModel(value: string): void {
    const model = (value ?? '').trim();
    if ((model || null) !== (this.effective().model ?? null)) {
      void this.save({ model: model || '' });
    }
  }

  /** Drop the project layer's `delegation` section (global applies again). */
  async removeProjectOverride(): Promise<void> {
    const dir = this.store.directory();
    if (!dir || this.store.saving()) {
      return;
    }
    let parsed: Record<string, unknown>;
    try {
      parsed = JSON.parse(this.store.rawProjectText()) as Record<string, unknown>;
    } catch {
      return;
    }
    if (!('delegation' in parsed)) {
      return;
    }
    delete parsed['delegation'];
    this.store.saving.set(true);
    this.store.error.set(null);
    this.store.saved.set(null);
    try {
      const cfg = await this.engine.putConfig(dir, parsed, { scope: 'project', replace: true });
      this.store.applyConfig(cfg);
      this.store.saved.set(this.t('settings.delegationSaved'));
    } catch (err) {
      this.store.error.set(this.store.describe(err));
    } finally {
      this.store.saving.set(false);
    }
  }

  private async save(delta: Partial<DelegationConfig>): Promise<void> {
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
        { delegation: delta },
        this.scope() === 'global' ? { scope: 'global' } : undefined,
      );
      this.store.applyConfig(cfg);
      this.store.saved.set(this.t('settings.delegationSaved'));
    } catch (err) {
      this.store.error.set(this.store.describe(err));
    } finally {
      this.store.saving.set(false);
    }
  }
}
