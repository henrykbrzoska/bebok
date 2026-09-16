/**
 * Shared derivation of the selectable-model lists (WP-SETTINGS / new-session
 * dialog / chat switcher).
 *
 * Every model `<select>` in the GUI must offer the same set: only providers
 * that can actually serve a request (a resolvable API key, or keyless local
 * servers such as Ollama) and only non-empty model names. Before this helper
 * each site rolled its own loop and they drifted: the settings store listed
 * providers without keys, and none of the sites dropped blank `""` model
 * entries, so selects grew unusable rows like `openai/`.
 *
 * Keyless providers: the engine's `ProviderSpec` sent over `GET /config`
 * carries no `auth` field, so the set below mirrors
 * `bebok_llm::spec::provider_auth` (`auth: none` only today). `has_key`
 * already covers the top-level `config.api_key` fallback - the engine ORs it
 * into the flag server-side.
 */

import { ProviderSpec } from './engine.dtos';

/** Providers that need no credential at all (engine `auth: "none"`). */
const KEYLESS_PROVIDERS: ReadonlySet<string> = new Set(['ollama']);

/** True when the provider can serve requests: keyless, or a key resolves. */
export function providerUsable(provider: ProviderSpec): boolean {
  return !!provider.has_key || KEYLESS_PROVIDERS.has(provider.name.trim().toLowerCase());
}

/**
 * Flat list of selectable `provider/model` ids: only usable providers, only
 * non-blank model names, no duplicates.
 */
export function selectableModels(providers: ProviderSpec[] | null | undefined): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const provider of providers ?? []) {
    if (!providerUsable(provider)) {
      continue;
    }
    const name = provider.name.trim();
    for (const model of provider.models ?? []) {
      const trimmed = model.trim();
      if (!trimmed || !name) {
        continue;
      }
      const id = `${name}/${trimmed}`;
      if (seen.has(id)) {
        continue;
      }
      seen.add(id);
      out.push(id);
    }
  }
  return out;
}
