/**
 * Provider catalog client (WP-SETTINGS / F2-23, consumes WP-LLM's F3-1).
 *
 * `GET /providers/catalog` describes the *shape* of every built-in provider's
 * configuration - label, wire protocol, credential style, environment
 * variable, default base URL and the provider-specific extra fields. The
 * Providers tab renders its form from this instead of hard-coding one flat
 * `kind/endpoint/api_key` row per provider; in particular it hides the API-key
 * field entirely when `auth` is `none` (Ollama).
 *
 * The endpoint is read with `authFetch` directly rather than through
 * `EngineClient`, because `core/` is owned by another package in this wave
 * (see the report: a `providerCatalog()` method there would be the natural
 * home once core is editable again).
 */

import { Injectable, inject, signal } from '@angular/core';

import { authFetch } from '../../core/auth.interceptor';
import { EngineClient } from '../../core/engine-client.service';
import { ProviderSpec } from '../../core/engine.dtos';

/** Input type of a catalog-declared extra field. */
export type ProviderFieldType = 'text' | 'password' | 'url' | 'number' | 'bool';

/** One provider-specific configuration field (lands in `ProviderSpec.extra`). */
export interface ProviderExtraField {
  key: string;
  label: string;
  type: ProviderFieldType;
  required: boolean;
  placeholder?: string | null;
}

/** `GET /providers/catalog` entry (engine `ProviderUiSpec`, camelCase). */
export interface ProviderUiSpec {
  id: string;
  label: string;
  kind: 'openai' | 'anthropic';
  auth: 'bearer' | 'x-api-key' | 'none';
  envVar: string;
  baseUrlDefault: string;
  extraFields: ProviderExtraField[];
}

/**
 * A provider row as edited in the GUI: the engine's `ProviderSpec` plus the
 * `extra` map added by WP-LLM (F3-2). `core/engine.dtos.ts` has no `extra`
 * field yet - that one-line addition belongs to whoever owns `core/`.
 */
export type ProviderDraft = ProviderSpec & { extra?: Record<string, string> };

/**
 * Built-in provider ids, used as the offline fallback for F3-5 when the
 * catalog endpoint is unavailable (old engine). Mirrors
 * `bebok_llm::spec::builtin_provider_specs()`.
 */
export const BUILTIN_PROVIDER_IDS: readonly string[] = [
  'zai',
  'openai',
  'anthropic',
  'xai',
  'deepseek',
  'google',
  'mistralai',
  'groq',
  'qwen',
  'openrouter',
  'ollama',
];

@Injectable({ providedIn: 'root' })
export class ProviderCatalog {
  private readonly engine = inject(EngineClient);

  /** Catalog entries, empty until `load()` succeeds. */
  readonly entries = signal<ProviderUiSpec[]>([]);
  /** True once a load attempt finished (successfully or not). */
  readonly loaded = signal(false);

  private inFlight: Promise<void> | null = null;

  /** Fetch the catalog once per session; failures degrade to the fallback. */
  load(): Promise<void> {
    if (this.inFlight) {
      return this.inFlight;
    }
    this.inFlight = this.fetchCatalog().finally(() => {
      this.loaded.set(true);
    });
    return this.inFlight;
  }

  /** Catalog entry for a provider id, or null for a genuinely custom one. */
  find(id: string): ProviderUiSpec | null {
    return this.entries().find((e) => e.id === id) ?? null;
  }

  /**
   * True for providers that come from the engine's built-in registry. Their
   * `kind` is fixed by the engine, so the GUI must not let it be switched
   * (F3-5). Falls back to the static id list when the catalog is unavailable.
   */
  isBuiltin(id: string): boolean {
    if (this.entries().length > 0) {
      return this.entries().some((e) => e.id === id);
    }
    return BUILTIN_PROVIDER_IDS.includes(id);
  }

  /** Allowed `kind` values for a provider: one fixed value for built-ins. */
  allowedKinds(id: string): Array<'openai' | 'anthropic'> {
    const entry = this.find(id);
    if (entry) {
      return [entry.kind];
    }
    if (this.entries().length === 0 && BUILTIN_PROVIDER_IDS.includes(id)) {
      // No catalog: we know it is built-in but not which kind - keep whatever
      // the config says by offering both, the select is disabled anyway.
      return ['openai', 'anthropic'];
    }
    return ['openai', 'anthropic'];
  }

  /** Extra fields declared for a provider (empty for custom ones). */
  extraFields(id: string): ProviderExtraField[] {
    return this.find(id)?.extraFields ?? [];
  }

  /** True when the provider needs no credential at all (`auth: "none"`). */
  needsApiKey(id: string): boolean {
    const entry = this.find(id);
    return entry ? entry.auth !== 'none' : true;
  }

  private async fetchCatalog(): Promise<void> {
    const conn = this.engine.connection();
    if (!conn) {
      return;
    }
    try {
      const res = await authFetch(`${conn.baseUrl}/providers/catalog`);
      if (!res.ok) {
        return;
      }
      const body = (await res.json()) as { providers?: ProviderUiSpec[] };
      if (Array.isArray(body?.providers)) {
        this.entries.set(body.providers);
      }
    } catch {
      /* old engine or offline: the static fallback above covers F3-5 */
    }
  }
}
