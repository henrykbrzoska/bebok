/**
 * Shared derivation of the selectable-model lists (WP-SETTINGS / new-session
 * dialog / chat switcher).
 *
 * Every model `<select>` in the GUI must offer the same set: only providers
 * that can actually serve a request and only non-empty model names. Before
 * this helper each site rolled its own loop and they drifted: the settings
 * store listed providers without keys, and none of the sites dropped blank
 * `""` model entries, so selects grew unusable rows like `openai/`.
 *
 * A provider is usable for one of two independent reasons:
 *
 * 1. keyless - `has_key` is false but no credential is needed at all. The
 *    engine's `ProviderSpec` carries no `auth` field over `GET /config`, so
 *    the set below mirrors `bebok_llm::spec::provider_auth` (`auth: none`,
 *    today only Ollama). `has_key` already covers the top-level
 *    `config.api_key` fallback - the engine ORs it into the flag server-side.
 * 2. a CLI provider (`kind: "cli"`) - it never has an API key because there is
 *    no HTTP endpoint to authenticate against: the engine spawns an external
 *    agentic CLI (Claude Code) instead. Keying off `has_key` alone would hide
 *    every `claude-cli/sonnet` from every model picker, so `kind` decides too.
 */

import { ProviderSpec } from './engine.dtos';

/** Providers that need no credential at all (engine `auth: "none"`). */
const KEYLESS_PROVIDERS: ReadonlySet<string> = new Set(['ollama']);

/**
 * True when the provider can serve requests: a CLI provider (keyless by
 * definition), a keyless local server, or a provider whose key resolves.
 */
export function providerUsable(provider: ProviderSpec): boolean {
  if (provider.kind === 'cli') {
    return true;
  }
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
