/**
 * The "Delegation" block of Settings -> Agents.
 *
 * A single numeric field: `delegation.max_concurrent` (1-16, default 3).
 * Saved as a `PUT /config` delta `{ delegation: { max_concurrent } }` like
 * every other Settings block. On save, legacy `mode` / `model_policy` keys
 * are dropped from the project layer so stale policy cannot linger.
 */

import { ChangeDetectionStrategy, Component, computed, effect, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { EngineClient } from '../../core/engine-client.service';
import { DelegationConfig } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';
import { SettingsStore } from './settings.store';

export const DEFAULT_MAX_CONCURRENT = 3;
export const MAX_CONCURRENT_LIMIT = 16;

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
    const n = d['max_concurrent'] ?? d['maxConcurrent'];
    if (typeof n === 'number' && n > 0) {
      out.max_concurrent = n;
    }
    return Object.keys(out).length > 0 ? out : null;
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

      <p class="small faint tools-note">{{ t('settings.delegationToolsNote') }}</p>
    </div>
  `,
  styles: [
    `
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

  readonly maxLimit = MAX_CONCURRENT_LIMIT;

  /** Effective (resolved) value from `GET /config`. */
  readonly effective = computed<number>(() => {
    const raw = this.store.config()?.config.delegation?.max_concurrent;
    return typeof raw === 'number' && raw > 0 ? raw : DEFAULT_MAX_CONCURRENT;
  });

  /** Draft of the numeric field (committed on blur / Enter). */
  readonly maxDraft = signal<number>(DEFAULT_MAX_CONCURRENT);

  constructor() {
    // Keep the draft in sync with the loaded value (a reload after save must
    // not leave a stale number in the box).
    effect(() => {
      this.maxDraft.set(this.effective());
    });
  }

  commitMax(): void {
    const raw = Number(this.maxDraft());
    const value = Number.isFinite(raw) ? Math.min(this.maxLimit, Math.max(1, Math.round(raw))) : 1;
    this.maxDraft.set(value);
    if (value !== this.effective()) {
      void this.save(value);
    }
  }

  private async save(value: number): Promise<void> {
    const dir = this.store.directory();
    if (!dir || this.store.saving()) {
      return;
    }
    this.store.saving.set(true);
    this.store.error.set(null);
    this.store.saved.set(null);
    try {
      // Drop legacy policy keys (`mode`, `model_policy`, `model`) from the
      // project layer so a stale policy cannot shadow the new value.
      let parsed: Record<string, unknown> | null = null;
      try {
        parsed = JSON.parse(this.store.rawProjectText()) as Record<string, unknown>;
      } catch {
        parsed = null;
      }
      const layer = parsed?.['delegation'];
      if (
        parsed &&
        layer &&
        typeof layer === 'object' &&
        !Array.isArray(layer) &&
        Object.keys(layer as Record<string, unknown>).some(
          (k) => k !== 'max_concurrent' && k !== 'maxConcurrent',
        )
      ) {
        const cleaned: Record<string, unknown> = {};
        for (const [k, v] of Object.entries(layer as Record<string, unknown>)) {
          if (k === 'max_concurrent' || k === 'maxConcurrent') {
            cleaned[k] = v;
          }
        }
        const next = { ...parsed, delegation: { ...cleaned, max_concurrent: value } };
        const cfg = await this.engine.putConfig(dir, next, { scope: 'project', replace: true });
        this.store.applyConfig(cfg);
        this.store.saved.set(this.t('settings.delegationSaved'));
        return;
      }
      const cfg = await this.engine.putConfig(dir, { delegation: { max_concurrent: value } });
      this.store.applyConfig(cfg);
      this.store.saved.set(this.t('settings.delegationSaved'));
    } catch (err) {
      this.store.error.set(this.store.describe(err));
    } finally {
      this.store.saving.set(false);
    }
  }
}
