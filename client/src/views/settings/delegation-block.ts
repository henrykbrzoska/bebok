/**
 * WP-DELEGATION (F8-2): the "Delegation" block of Settings -> Agents.
 *
 * Its own component (not part of `AgentsTab`) so it can be dropped into the
 * tab with one tag and evolve independently. It edits the `delegation`
 * config section - `mode` (off / auto / always), `max_concurrent` and the
 * sub-agent `model_policy` (F9-10: inherit / cheaper / an explicit model;
 * the legacy `model` key is an explicit policy) - through the same
 * `PUT /config` delta every other Settings block uses. A small table below
 * the policy shows what `GET /delegation/models` resolves right now. The section is global config with a per-key
 * project override: the scope selector says where a change is written, and
 * a note tells the user when the project layer overrides the global one
 * (with a one-click "remove project override").
 */

import { ChangeDetectionStrategy, Component, computed, effect, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { EngineClient } from '../../core/engine-client.service';
import { DelegationConfig, DelegationMode, DelegationModelsResponse } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';
import { MessageKey } from '../../i18n';
import { SettingsStore } from './settings.store';

export const DELEGATION_MODES: readonly DelegationMode[] = ['off', 'auto', 'always'];
export const DEFAULT_DELEGATION: DelegationConfig = {
  mode: 'auto',
  max_concurrent: 3,
  model: null,
  model_policy: 'cheaper',
};
export const MAX_CONCURRENT_LIMIT = 16;

/** F9-10: the three radio choices; `explicit` stores the model id as the policy string. */
export type ModelPolicyKind = 'inherit' | 'cheaper' | 'explicit';
export const MODEL_POLICY_KINDS: readonly ModelPolicyKind[] = ['inherit', 'cheaper', 'explicit'];

/**
 * Classify a `model_policy` string (with the legacy `model` key as a
 * fallback): `inherit` / `cheaper` are keywords, anything else is an explicit
 * `provider/model`. Missing both = the engine default (`cheaper`).
 */
export function policyKindOf(
  policy: string | null | undefined,
  legacyModel: string | null | undefined,
): { kind: ModelPolicyKind; model: string | null } {
  const p = (policy ?? '').trim();
  if (p === 'inherit' || p === 'cheaper') {
    return { kind: p, model: null };
  }
  if (p) {
    return { kind: 'explicit', model: p };
  }
  const legacy = (legacyModel ?? '').trim();
  return legacy ? { kind: 'explicit', model: legacy } : { kind: 'cheaper', model: null };
}

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
    const policy = d['model_policy'] ?? d['modelPolicy'];
    if (typeof policy === 'string') {
      out.model_policy = policy;
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

        <fieldset class="field policy" [disabled]="store.saving()" data-testid="delegation-policy">
          <legend class="micro-label">{{ t('settings.delegationModelPolicy') }}</legend>
          <label class="policy-opt">
            <input
              type="radio"
              name="delegation-policy"
              value="inherit"
              [checked]="policyKind() === 'inherit'"
              (change)="setPolicy('inherit')"
              data-testid="delegation-policy-inherit"
            />
            <span>{{ t('settings.delegationPolicyInherit') }}</span>
          </label>
          <label class="policy-opt">
            <input
              type="radio"
              name="delegation-policy"
              value="cheaper"
              [checked]="policyKind() === 'cheaper'"
              (change)="setPolicy('cheaper')"
              data-testid="delegation-policy-cheaper"
            />
            <span class="policy-text">
              <span>{{ t('settings.delegationPolicyCheaper') }}</span>
              <span class="small faint">{{ t('settings.delegationPolicyCheaperHelp') }}</span>
            </span>
          </label>
          <label class="policy-opt explicit">
            <input
              type="radio"
              name="delegation-policy"
              value="explicit"
              [checked]="policyKind() === 'explicit'"
              (change)="setPolicy('explicit')"
              data-testid="delegation-policy-explicit"
            />
            <span class="policy-text">
              <span>{{ t('settings.delegationPolicyExplicit') }}</span>
              <select
                class="mono"
                data-testid="delegation-model"
                [ngModel]="explicitDraft()"
                (ngModelChange)="setModel($event)"
                [disabled]="store.saving() || policyKind() !== 'explicit'"
              >
                <option value="">{{ t('settings.delegationModelDefault') }}</option>
                @if (explicitDraft() && !store.availableModels().includes(explicitDraft())) {
                  <option [value]="explicitDraft()">{{ explicitDraft() }}</option>
                }
                @for (model of store.availableModels(); track model) {
                  <option [value]="model">{{ model }}</option>
                }
              </select>
            </span>
          </label>
        </fieldset>
      </div>

      <div class="resolved" data-testid="delegation-resolved">
        <div class="resolved-head">
          <span class="micro-label">{{ t('settings.delegationResolvedTitle') }}</span>
          @if (resolved(); as r) {
            <span class="small faint mono" data-testid="delegation-resolved-now">
              {{ t('settings.delegationResolvedNow', { model: r.resolved || '-' }) }}
            </span>
          }
          <button type="button" class="link" (click)="loadResolved()" [disabled]="resolvedLoading()">
            {{ t('changes.refresh') }}
          </button>
        </div>
        @if (resolvedError()) {
          <div class="small faint error">{{ resolvedError() }}</div>
        } @else if (resolved(); as r) {
          @if (r.mappings.length === 0) {
            <div class="small faint">{{ t('settings.delegationNoCheaper') }}</div>
          } @else {
            <table class="mappings">
              <tbody>
                @for (m of r.mappings; track m.provider + '/' + m.model) {
                  <tr data-testid="delegation-mapping">
                    <td class="mono provider">{{ m.provider }}</td>
                    <td class="mono">{{ m.model }}</td>
                    <td class="arrow" aria-hidden="true">&rarr;</td>
                    <td class="mono" [class.faint]="!m.cheaper">
                      {{ m.cheaper || t('settings.delegationNoCheaper') }}
                    </td>
                  </tr>
                }
              </tbody>
            </table>
          }
        } @else if (resolvedLoading()) {
          <div class="small faint">{{ t('changes.loading') }}</div>
        }
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
      /* F9-10: model policy radio + resolved table. */
      .policy {
        border: none;
        margin: 0;
        padding: 0;
        gap: var(--space-6);
      }
      .policy legend {
        padding: 0;
        margin-bottom: var(--space-4);
      }
      .policy-opt {
        display: flex;
        align-items: flex-start;
        gap: var(--space-8);
        cursor: pointer;
        font-size: var(--fs-12-5);
      }
      .policy-opt input {
        margin-top: 3px;
        flex: none;
      }
      .policy-text {
        display: flex;
        flex-direction: column;
        gap: 3px;
        min-width: 0;
        flex: 1 1 auto;
      }
      .policy-opt select {
        max-width: 100%;
      }
      .resolved {
        margin-top: var(--space-12);
        padding-top: var(--space-8);
        border-top: 1px dashed var(--border);
      }
      .resolved-head {
        display: flex;
        align-items: baseline;
        gap: var(--space-8);
        margin-bottom: var(--space-4);
      }
      .resolved-head .link {
        margin-left: auto;
        background: none;
        border: none;
        padding: 0;
        color: var(--text-muted);
        font-size: var(--fs-11);
        cursor: pointer;
      }
      .resolved-head .link:hover:not(:disabled) {
        color: var(--accent);
      }
      .mappings {
        border-collapse: collapse;
        font-size: var(--fs-11-5);
      }
      .mappings td {
        padding: 2px var(--space-8) 2px 0;
        vertical-align: baseline;
        white-space: nowrap;
      }
      .mappings .provider {
        color: var(--text-faint);
      }
      .mappings .arrow {
        color: var(--text-faint);
      }
      .mappings .faint {
        color: var(--text-faint);
      }
      .error {
        color: var(--danger);
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

  /** F9-10: `GET /delegation/models` snapshot (null until loaded / on error). */
  readonly resolved = signal<DelegationModelsResponse | null>(null);
  readonly resolvedLoading = signal(false);
  readonly resolvedError = signal<string | null>(null);

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
      ...(raw?.model_policy?.trim() ? { model_policy: raw.model_policy } : {}),
    };
  });

  /** F9-10: which radio is on, and the explicit model when the policy is one. */
  readonly policy = computed(() => {
    const e = this.effective();
    return policyKindOf(e.model_policy, e.model);
  });
  /** "Explicit" chosen but no model picked yet (keeps the select enabled). */
  readonly pendingExplicit = signal(false);
  readonly policyKind = computed<ModelPolicyKind>(() =>
    this.pendingExplicit() ? 'explicit' : this.policy().kind,
  );
  /** Model shown in the explicit select (the policy's model, else the legacy key, else empty). */
  readonly explicitDraft = computed(() => this.policy().model ?? '');

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
    // The resolved table follows the directory; a save re-reads it below.
    effect(() => {
      const dir = this.store.directory();
      if (dir) {
        void this.loadResolved(dir);
      } else {
        this.resolved.set(null);
      }
    });
  }

  /** F9-10: (re)load "Resolved cheaper models" for the current directory. */
  async loadResolved(directory: string | null = this.store.directory()): Promise<void> {
    if (!directory) {
      return;
    }
    this.resolvedLoading.set(true);
    try {
      const res = await this.engine.delegationModels(directory);
      if (this.store.directory() !== directory) {
        return;
      }
      this.resolved.set({ ...res, mappings: res.mappings ?? [] });
      this.resolvedError.set(null);
    } catch (err) {
      if (this.store.directory() === directory) {
        this.resolved.set(null);
        this.resolvedError.set(this.store.describe(err));
      }
    } finally {
      this.resolvedLoading.set(false);
    }
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

  /**
   * F9-10: pick a policy. `inherit` / `cheaper` save the keyword; `explicit`
   * saves the currently drafted model id (or falls through to `inherit`
   * until one is picked from the select). The legacy `model` key is cleared
   * on every policy write so it can no longer shadow the policy.
   */
  setPolicy(kind: ModelPolicyKind): void {
    if (kind === this.policyKind()) {
      return;
    }
    if (kind === 'explicit') {
      const model = this.explicitDraft();
      if (!model) {
        // Nothing to store yet: the select becomes enabled once the user
        // picks a model, `setModel` writes the policy then.
        this.pendingExplicit.set(true);
        return;
      }
      void this.save({ model_policy: model, model: '' });
      return;
    }
    this.pendingExplicit.set(false);
    void this.save({ model_policy: kind, model: '' });
  }

  /** Explicit model picked from the select: it becomes the policy string. */
  setModel(value: string): void {
    const model = (value ?? '').trim();
    this.pendingExplicit.set(false);
    if (!model) {
      if (this.policyKind() === 'explicit') {
        void this.save({ model_policy: 'inherit', model: '' });
      }
      return;
    }
    if (model !== (this.policy().model ?? null)) {
      void this.save({ model_policy: model, model: '' });
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
      if ('model_policy' in delta || 'model' in delta) {
        void this.loadResolved(dir);
      }
    } catch (err) {
      this.store.error.set(this.store.describe(err));
    } finally {
      this.store.saving.set(false);
    }
  }
}
