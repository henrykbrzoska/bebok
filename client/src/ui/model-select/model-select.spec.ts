import { filterModelOptions, groupModelOptions, splitModelOption } from './model-select';

describe('model-select helpers', () => {
  it('splits provider/model for grouped display', () => {
    expect(splitModelOption('openrouter/meta-llama/llama-4')).toEqual({
      provider: 'openrouter',
      short: 'meta-llama/llama-4',
    });
    expect(splitModelOption('plain')).toEqual({ provider: '', short: 'plain' });
  });

  it('filters case-insensitively over the full id', () => {
    const models = ['openrouter/llama-4', 'anthropic/claude-sonnet', 'openai/gpt-4o'];
    expect(filterModelOptions(models, '')).toEqual(models);
    expect(filterModelOptions(models, 'LLAMA')).toEqual(['openrouter/llama-4']);
    expect(filterModelOptions(models, 'openrouter/')).toEqual(['openrouter/llama-4']);
    expect(filterModelOptions(models, 'zzz')).toEqual([]);
  });

  it('groups by provider prefix in first-seen order', () => {
    const groups = groupModelOptions(['openrouter/a', 'anthropic/b', 'openrouter/c']);
    expect(groups.map((g) => g.provider)).toEqual(['openrouter', 'anthropic']);
    expect(groups[0].items).toEqual(['openrouter/a', 'openrouter/c']);
  });
});
