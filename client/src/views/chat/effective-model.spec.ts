/**
 * F9-9: `resolveEffectiveModel` - the pure resolution behind the toolbar's
 * effective-model badge.
 */

import { AgentInfo, SessionMeta } from '../../core/engine.dtos';
import { NO_MODEL, resolveEffectiveModel, splitModelId } from './effective-model';

const AGENTS: AgentInfo[] = [
  { name: 'code', builtin: true, model: 'anthropic/claude-x' },
  { name: 'ask', builtin: true, model: null },
];

function meta(extra: Partial<SessionMeta> = {}): SessionMeta {
  return {
    id: 's1',
    directory: '/p',
    agent: 'code',
    created_at: 0,
    updated_at: 0,
    usage: { input_tokens: 0, output_tokens: 0 },
    ...extra,
  } as SessionMeta;
}

describe('splitModelId (F9-9)', () => {
  it('splits provider/model and tolerates ids without a prefix', () => {
    expect(splitModelId('openai/gpt-5.6-luna')).toEqual({
      id: 'openai/gpt-5.6-luna',
      provider: 'openai',
      model: 'gpt-5.6-luna',
    });
    expect(splitModelId('  zai/glm-4.6  ')).toEqual({ id: 'zai/glm-4.6', provider: 'zai', model: 'glm-4.6' });
    expect(splitModelId('gpt-5')).toEqual({ id: 'gpt-5', provider: '', model: 'gpt-5' });
    expect(splitModelId('')).toBe(NO_MODEL);
  });

  it('keeps everything after the first slash as the model', () => {
    expect(splitModelId('openrouter/meta/llama-4')).toEqual({
      id: 'openrouter/meta/llama-4',
      provider: 'openrouter',
      model: 'meta/llama-4',
    });
  });
});

describe('resolveEffectiveModel (F9-9)', () => {
  it('maps "default" (no selection) to the engine-attached effective id', () => {
    const m = meta({ effective_model: 'openai/gpt-5.6-luna', effective_provider: 'openai' });
    expect(resolveEffectiveModel('', m, AGENTS, 'zai/glm-4.6')).toEqual({
      id: 'openai/gpt-5.6-luna',
      provider: 'openai',
      model: 'gpt-5.6-luna',
    });
    expect(resolveEffectiveModel('default', m, AGENTS, null).id).toBe('openai/gpt-5.6-luna');
    expect(resolveEffectiveModel('(default)', m, AGENTS, null).id).toBe('openai/gpt-5.6-luna');
  });

  it('lets an explicit selection win over everything else', () => {
    const m = meta({ model: 'zai/glm-4.5', effective_model: 'openai/gpt-5.6-luna' });
    expect(resolveEffectiveModel('anthropic/claude-y', m, AGENTS, 'zai/glm-4.6')).toEqual({
      id: 'anthropic/claude-y',
      provider: 'anthropic',
      model: 'claude-y',
    });
  });

  it('splits the provider off the id, using the engine provider when the id has none', () => {
    const m = meta({ effective_model: 'gpt-5.6-luna', effective_provider: 'openai' });
    expect(resolveEffectiveModel(null, m, [], null)).toEqual({
      id: 'gpt-5.6-luna',
      provider: 'openai',
      model: 'gpt-5.6-luna',
    });
  });

  it('falls back to session.model, the agent preset, then the config default on older engines', () => {
    expect(resolveEffectiveModel('', meta({ model: 'zai/glm-4.5' }), AGENTS, 'zai/glm-4.6').id).toBe('zai/glm-4.5');
    expect(resolveEffectiveModel('', meta(), AGENTS, 'zai/glm-4.6').id).toBe('anthropic/claude-x');
    expect(resolveEffectiveModel('', meta({ agent: 'ask' }), AGENTS, 'zai/glm-4.6').id).toBe('zai/glm-4.6');
  });

  it('never yields "(default)": returns the empty model when nothing is known', () => {
    expect(resolveEffectiveModel('', meta({ agent: 'ask' }), AGENTS, null)).toBe(NO_MODEL);
    expect(resolveEffectiveModel(undefined, null, undefined, undefined)).toBe(NO_MODEL);
    expect(resolveEffectiveModel('default', meta({ effective_model: 'default' }), [], '').id).toBe('');
  });
});
