/**
 * Selectable-model list derivation (shared by the settings store, the chat
 * header switcher and the new-session dialog).
 *
 * New projects often carry provider entries without a resolvable API key and
 * blank model rows (`""` from the editor, or a partially merged catalog) -
 * none of those may reach a model `<select>`.
 */

import { ProviderSpec } from './engine.dtos';
import { providerUsable, selectableModels } from './model-list';

function provider(overrides: Partial<ProviderSpec>): ProviderSpec {
  return {
    name: 'openai',
    kind: 'openai',
    endpoint: null,
    api_key: null,
    models: [],
    ...overrides,
  };
}

describe('selectableModels', () => {
  it('keeps only providers with a resolvable key', () => {
    const models = selectableModels([
      provider({ name: 'anthropic', models: ['claude-sonnet'], has_key: true }),
      provider({ name: 'openai', models: ['gpt'], has_key: false }),
    ]);
    expect(models).toEqual(['anthropic/claude-sonnet']);
  });

  it('does not drop keyless providers such as ollama', () => {
    const models = selectableModels([
      provider({ name: 'ollama', models: ['llama3.2'], has_key: false }),
    ]);
    expect(models).toEqual(['ollama/llama3.2']);
  });

  it('drops blank model names, blank providers and duplicate ids', () => {
    const models = selectableModels([
      provider({ name: 'openai', has_key: true, models: ['', '   ', 'gpt-5', 'gpt-5'] }),
      provider({ name: '   ', has_key: true, models: ['weird-model'] }),
      provider({ name: 'zai', has_key: true, models: ['glm-4.6'] }),
    ]);
    expect(models).toEqual(['openai/gpt-5', 'zai/glm-4.6']);
  });

  it('trims model names in the composed id', () => {
    const models = selectableModels([
      provider({ name: 'openai', has_key: true, models: [' gpt-5 '] }),
    ]);
    expect(models).toEqual(['openai/gpt-5']);
  });

  it('accepts a null provider list', () => {
    expect(selectableModels(null)).toEqual([]);
    expect(selectableModels(undefined)).toEqual([]);
  });

  it('providerUsable is case-insensitive on the keyless set', () => {
    expect(providerUsable(provider({ name: 'Ollama', has_key: false }))).toBeTrue();
    expect(providerUsable(provider({ name: 'mycorp', has_key: false }))).toBeFalse();
    expect(providerUsable(provider({ name: 'mycorp', has_key: true }))).toBeTrue();
  });
});
