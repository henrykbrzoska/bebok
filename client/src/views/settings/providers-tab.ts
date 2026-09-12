/**
 * Providers tab (WP-SETTINGS / F2-23).
 *
 * 220px list (status dot + provider name) + a dashed "+ Add provider" button,
 * and a detail card driven by `GET /providers/catalog` (WP-LLM / F3-1): kind,
 * endpoint, API key (hidden when the provider's `auth` is `none`), the
 * provider-specific `extraFields[]`, a "Test connection" button with a spinner
 * and the discovered models as a monospace list, which can then be assigned to
 * an agent type.
 */

import { Component, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { I18nService } from '../../i18n/i18n.service';
import { EngineClient } from '../../core/engine-client.service';
import { ProviderCatalog, ProviderDraft, ProviderExtraField } from './provider-catalog';
import { SettingsStore } from './settings.store';

@Component({
  selector: 'app-settings-providers',
  imports: [FormsModule],
  templateUrl: './providers-tab.html',
  styleUrls: ['./settings-shared.css', './providers-tab.css'],
})
export class ProvidersTab {
  private readonly i18n = inject(I18nService);
  private readonly engine = inject(EngineClient);

  readonly store = inject(SettingsStore);
  readonly catalog = inject(ProviderCatalog);
  readonly t = this.i18n.t.bind(this.i18n);

  /** The "+ Add provider" form is collapsed until the button is pressed. */
  readonly adding = signal(false);
  readonly draftName = signal('');
  readonly draftKind = signal<'openai' | 'anthropic'>('openai');

  /** Agent type the discovered model is about to be assigned to. */
  readonly assignType = signal('code');
  readonly assignModel = signal('');

  readonly selected = computed<ProviderDraft | null>(
    () => this.store.providers().find((p) => p.name === this.store.selectedProvider()) ?? null,
  );

  /** Extra fields declared for the selected provider by the catalog. */
  readonly extraFields = computed<ProviderExtraField[]>(() => {
    const provider = this.selected();
    return provider ? this.catalog.extraFields(provider.name) : [];
  });

  /** Models discovered by the last successful "Test connection". */
  readonly discovered = computed<string[]>(() => {
    const provider = this.selected();
    return provider ? (this.store.checkedModels()[provider.name] ?? []) : [];
  });

  select(name: string): void {
    this.store.selectedProvider.set(name);
    this.store.providerModelsError.set(null);
  }

  /** True when the provider is usable: keyless, or a key is resolvable. */
  isConfigured(provider: ProviderDraft): boolean {
    if (!this.catalog.needsApiKey(provider.name)) {
      return true;
    }
    return !!provider.has_key || (this.store.keyDrafts()[provider.name] ?? '').trim().length > 0;
  }

  statusLabel(provider: ProviderDraft): string {
    if (!this.catalog.needsApiKey(provider.name)) {
      return this.t('settings.providerKeyless');
    }
    return this.isConfigured(provider)
      ? this.t('settings.providerConfigured')
      : this.t('settings.providerNoKey');
  }

  /** Placeholder for the endpoint field: the catalog's default base URL. */
  endpointPlaceholder(provider: ProviderDraft): string {
    return this.catalog.find(provider.name)?.baseUrlDefault ?? '';
  }

  /** Environment variable consulted when no key is stored. */
  envVar(provider: ProviderDraft): string {
    return (
      this.catalog.find(provider.name)?.envVar ??
      `${provider.name.toUpperCase().replace(/-/g, '_')}_API_KEY`
    );
  }

  /** Draft key typed for a provider (the stored key is never rendered). */
  keyDraft(name: string): string {
    return this.store.keyDrafts()[name] ?? '';
  }

  extraValue(provider: ProviderDraft, key: string): string {
    const value = provider.extra?.[key];
    return value === undefined || value === null ? '' : String(value);
  }

  /** HTML input type for a catalog field type. */
  inputType(field: ProviderExtraField): string {
    switch (field.type) {
      case 'password':
        return 'password';
      case 'number':
        return 'number';
      case 'url':
        return 'url';
      default:
        return 'text';
    }
  }

  showAdd(): void {
    this.adding.set(true);
    this.draftName.set('');
    this.draftKind.set('openai');
  }

  addProvider(): void {
    const name = this.draftName().trim();
    if (!name) {
      return;
    }
    this.store.addProvider({
      name,
      kind: this.draftKind(),
      endpoint: null,
      api_key: null,
      models: [],
    });
    this.adding.set(false);
  }

  /**
   * Test the connection to a provider ("check available models").
   *
   * The current form state is persisted first, because the engine resolves the
   * API key from its saved config, not from the GUI.
   */
  async checkModels(provider: ProviderDraft): Promise<void> {
    const dir = this.store.directory();
    if (!dir || this.store.checkingProvider()) {
      return;
    }
    this.store.checkingProvider.set(provider.name);
    this.store.error.set(null);
    this.store.providerModelsError.set(null);
    try {
      await this.engine.putConfig(dir, { providers: this.store.providersToPersist() });
      const res = await this.engine.listModels(dir, provider.name);
      this.store.checkedModels.update((m) => ({ ...m, [provider.name]: res.models }));
      this.assignModel.set(res.models[0] ?? '');
      await this.store.reload();
    } catch (err) {
      this.store.providerModelsError.set(this.store.describe(err));
    } finally {
      this.store.checkingProvider.set(null);
    }
  }

  /** Assign a discovered model to an agent type (`config.models.<type>`). */
  async assign(): Promise<void> {
    const provider = this.selected();
    const model = this.assignModel();
    if (!provider || !model) {
      return;
    }
    this.store.setTypeModel(this.assignType(), `${provider.name}/${model}`);
    await this.store.saveTypeModels();
  }
}
