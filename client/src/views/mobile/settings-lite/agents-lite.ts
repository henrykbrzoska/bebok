/**
 * Agents section of Settings-lite (WP-M5 / F10-19): the agent presets the
 * engine reports (`GET /config` → `agents`) with their effective model, and
 * the per-type model override (`config.models.<type>`) as one select per
 * built-in type, saved to the global layer. Prompt editing stays desktop-only.
 */

import { ChangeDetectionStrategy, Component, computed, inject } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { AgentInfo } from '../../../core/engine.dtos';
import { I18nService } from '../../../i18n/i18n.service';
import { SettingsStore } from '../../settings/settings.store';
import { GlobalSaver, LITE_STYLES } from './lite-shared';

@Component({
  selector: 'app-settings-lite-agents',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  template: `
    <div class="card" data-testid="agents-lite-models">
      <h3>{{ t('settings.typeModelsTitle') }}</h3>
      <p class="hint">{{ t('settings.typeModelsHint') }}</p>
      @for (type of store.agentTypes; track type) {
        <label class="field">
          <span>{{ type }}</span>
          <select
            class="mono"
            [ngModel]="store.typeModelFor(type)"
            (ngModelChange)="store.setTypeModel(type, $event)"
            [attr.data-testid]="'agents-lite-model-' + type"
          >
            <option value="">{{ t('mobile.settings.defaultModel') }}</option>
            @for (model of optionsFor(type); track model) {
              <option [value]="model">{{ model }}</option>
            }
          </select>
        </label>
      }
      <button
        type="button"
        class="btn-primary"
        (click)="save()"
        [disabled]="store.saving()"
        data-testid="agents-lite-save"
      >
        {{ t('settings.saveTypeModels') }}
      </button>
    </div>

    <nav class="list" data-testid="agents-lite-list">
      @for (agent of agents(); track agent.name) {
        <button type="button" disabled>
          <span>
            {{ agent.name }}
            @if (agent.description) {
              <span class="status"> · {{ agent.description }}</span>
            }
          </span>
          <span class="status mono">{{ agent.model || '' }}</span>
        </button>
      } @empty {
        <p class="hint" style="padding: var(--space-12)">{{ t('start.loading') }}</p>
      }
    </nav>
  `,
  styles: [
    LITE_STYLES,
    `
      .list button:disabled {
        cursor: default;
        color: var(--text);
      }
    `,
  ],
})
export class AgentsLite {
  private readonly i18n = inject(I18nService);
  private readonly saver = new GlobalSaver();

  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly agents = computed<AgentInfo[]>(() => this.store.config()?.agents ?? []);

  /** Known models plus the currently set one (so an unknown id is not dropped). */
  optionsFor(type: string): string[] {
    const current = this.store.typeModelFor(type);
    const models = this.store.availableModels();
    return current && !models.includes(current) ? [current, ...models] : models;
  }

  async save(): Promise<void> {
    const models: Record<string, string> = {};
    for (const type of this.store.agentTypes) {
      const v = this.store.typeModels()[type]?.trim();
      if (v) {
        models[type] = v;
      }
    }
    await this.saver.save({ models }, 'settings.savedModels');
  }
}
