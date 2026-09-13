/**
 * Providers section of Settings-lite (WP-M5 / F10-19): provider list →
 * one provider's detail (endpoint, API key, test connection). Drives the
 * same `SettingsStore` drafts as the desktop tab (`keyDrafts`,
 * `providersToPersist`) so the stored key is never rendered; saving writes
 * the global config layer.
 */

import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { EngineClient } from '../../../core/engine-client.service';
import { I18nService } from '../../../i18n/i18n.service';
import { ProviderCatalog, ProviderDraft } from '../../settings/provider-catalog';
import { SettingsStore } from '../../settings/settings.store';
import { GlobalSaver, LITE_STYLES } from './lite-shared';

@Component({
  selector: 'app-settings-lite-providers',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  template: `
    @if (selected(); as provider) {
      <button type="button" class="link" (click)="select(null)" data-testid="providers-lite-list-link">
        ‹ {{ t('settings.tab.providers') }}
      </button>
      <div class="card" data-testid="providers-lite-detail">
        <h3>{{ provider.name }}</h3>
        <p class="hint">{{ t('settings.providerKind') }}: {{ provider.kind }}</p>
        <label class="field">
          <span>{{ t('settings.endpoint') }}</span>
          <input
            type="url"
            class="mono"
            inputmode="url"
            autocapitalize="off"
            spellcheck="false"
            [ngModel]="provider.endpoint ?? ''"
            (ngModelChange)="store.updateProvider(provider.name, { endpoint: $event || null })"
            [placeholder]="endpointPlaceholder(provider)"
          />
        </label>
        @if (needsKey(provider)) {
          <label class="field">
            <span>{{ t('settings.providerKey') }}</span>
            <input
              type="password"
              class="mono"
              autocomplete="off"
              autocapitalize="off"
              spellcheck="false"
              [ngModel]="keyDraft(provider.name)"
              (ngModelChange)="store.setKeyDraft(provider.name, $event)"
              [attr.placeholder]="provider.has_key ? t('settings.providerKeyKeep') : t('settings.providerKeyPlaceholder')"
              data-testid="providers-lite-key"
            />
          </label>
          <p class="hint">{{ t('settings.providerKeyEnv', { env: envVar(provider) }) }}</p>
        } @else {
          <p class="hint">{{ t('settings.providerKeyless') }}</p>
        }
        <div class="row">
          <button
            type="button"
            class="btn-primary"
            (click)="save()"
            [disabled]="store.saving()"
            data-testid="providers-lite-save"
          >
            {{ t('settings.saveProviders') }}
          </button>
          <button
            type="button"
            class="btn"
            (click)="test(provider)"
            [disabled]="store.checkingProvider() !== null || store.saving()"
            data-testid="providers-lite-test"
          >
            {{ store.checkingProvider() === provider.name ? t('settings.checking') : t('settings.testConnection') }}
          </button>
        </div>
        @if (store.providerModelsError(); as err) {
          <p class="error">{{ err }}</p>
        }
        @if (discovered().length > 0) {
          <p class="hint">
            {{ t('settings.discoveredModels') }}: {{ discovered().length }} ·
            <span class="mono">{{ discovered().slice(0, 5).join(', ') }}</span>
          </p>
        }
      </div>
    } @else {
      <nav class="list" data-testid="providers-lite-list">
        @for (provider of store.providers(); track provider.name) {
          <button type="button" (click)="select(provider.name)" [attr.data-testid]="'provider-' + provider.name">
            <span>{{ provider.name }}</span>
            <span class="status" [class.ok]="isConfigured(provider)" [class.warn]="!isConfigured(provider)">
              {{ statusLabel(provider) }}
            </span>
          </button>
        } @empty {
          <p class="hint" style="padding: var(--space-12)">{{ t('start.loading') }}</p>
        }
      </nav>
      <div class="card">
        @if (adding()) {
          <label class="field">
            <span>{{ t('settings.addProviderTitle') }}</span>
            <input
              type="text"
              class="mono"
              autocapitalize="off"
              spellcheck="false"
              [ngModel]="draftName()"
              (ngModelChange)="draftName.set($event)"
              placeholder="mycorp"
              data-testid="providers-lite-new-name"
            />
          </label>
          <label class="field">
            <span>{{ t('settings.providerKind') }}</span>
            <select [ngModel]="draftKind()" (ngModelChange)="draftKind.set($event)">
              <option value="openai">openai</option>
              <option value="anthropic">anthropic</option>
            </select>
          </label>
          <p class="hint">{{ t('settings.addProviderHint') }}</p>
          <div class="row">
            <button type="button" class="btn-primary" (click)="add()" [disabled]="!draftName().trim()">
              {{ t('settings.addProvider') }}
            </button>
            <button type="button" class="btn" (click)="adding.set(false)">{{ t('settings.cancel') }}</button>
          </div>
        } @else {
          <button type="button" class="btn" (click)="adding.set(true)" data-testid="providers-lite-add">
            {{ t('settings.addProvider') }}
          </button>
        }
      </div>
    }
  `,
  styles: [LITE_STYLES],
})
export class ProvidersLite {
  private readonly engine = inject(EngineClient);
  private readonly i18n = inject(I18nService);
  private readonly saver = new GlobalSaver();

  readonly store = inject(SettingsStore);
  readonly catalog = inject(ProviderCatalog);
  readonly t = this.i18n.t.bind(this.i18n);

  /** Provider open in the detail (null = the list). Independent of the desktop's `selectedProvider`. */
  readonly selectedName = signal<string | null>(null);
  readonly selected = computed<ProviderDraft | null>(
    () => this.store.providers().find((p) => p.name === this.selectedName()) ?? null,
  );
  readonly discovered = computed<string[]>(() => {
    const provider = this.selected();
    return provider ? (this.store.checkedModels()[provider.name] ?? []) : [];
  });

  readonly adding = signal(false);
  readonly draftName = signal('');
  readonly draftKind = signal<'openai' | 'anthropic'>('openai');

  select(name: string | null): void {
    this.selectedName.set(name);
    this.store.providerModelsError.set(null);
    this.store.saved.set(null);
  }

  needsKey(provider: ProviderDraft): boolean {
    return this.catalog.needsApiKey(provider.name);
  }

  isConfigured(provider: ProviderDraft): boolean {
    if (!this.needsKey(provider)) {
      return true;
    }
    return !!provider.has_key || (this.store.keyDrafts()[provider.name] ?? '').trim().length > 0;
  }

  statusLabel(provider: ProviderDraft): string {
    if (!this.needsKey(provider)) {
      return this.t('settings.providerKeyless');
    }
    return this.isConfigured(provider)
      ? this.t('settings.providerConfigured')
      : this.t('settings.providerNoKey');
  }

  endpointPlaceholder(provider: ProviderDraft): string {
    return this.catalog.find(provider.name)?.baseUrlDefault ?? '';
  }

  envVar(provider: ProviderDraft): string {
    return (
      this.catalog.find(provider.name)?.envVar ??
      `${provider.name.toUpperCase().replace(/-/g, '_')}_API_KEY`
    );
  }

  keyDraft(name: string): string {
    return this.store.keyDrafts()[name] ?? '';
  }

  /** Persist every provider (drafted keys included) to the global layer. */
  async save(): Promise<void> {
    await this.saver.save({ providers: this.store.providersToPersist() }, 'settings.savedProviders');
  }

  /** `GET /models` against the saved configuration (same contract as the desktop). */
  async test(provider: ProviderDraft): Promise<void> {
    const dir = this.store.directory();
    if (!dir || this.store.checkingProvider()) {
      return;
    }
    this.store.checkingProvider.set(provider.name);
    this.store.providerModelsError.set(null);
    try {
      const res = await this.engine.listModels(dir, provider.name);
      this.store.checkedModels.update((m) => ({ ...m, [provider.name]: res.models }));
      this.store.updateProvider(provider.name, { models: res.models });
    } catch (err) {
      this.store.providerModelsError.set(this.store.describe(err));
    } finally {
      this.store.checkingProvider.set(null);
    }
  }

  add(): void {
    const name = this.draftName().trim().toLowerCase();
    if (!name) {
      return;
    }
    this.store.addProvider({ name, kind: this.draftKind(), endpoint: null, api_key: null, models: [] });
    this.adding.set(false);
    this.draftName.set('');
    this.select(name);
  }
}
