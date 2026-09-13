/**
 * F9-9: which model a prompt sent from the chat will actually run on.
 *
 * Pure helper behind the toolbar badge: an explicit selection in the model
 * switcher wins; otherwise the engine-attached `meta.effective_model`
 * (session.model -> agent preset -> `models.<agent>` -> config default);
 * for older engines without that field, the selected agent's own resolved
 * model from `GET /agent`, then the config default. `''` when nothing is
 * known, so the badge stays hidden rather than showing "(default)".
 */

import { AgentInfo, SessionMeta } from '../../core/engine.dtos';

export interface EffectiveModel {
  /** Full id, e.g. `openai/gpt-5.6-luna` (`''` = unknown). */
  id: string;
  /** Part before the first `/` (`''` when the id has no provider prefix). */
  provider: string;
  /** Part after the first `/` (the whole id when it has no prefix). */
  model: string;
}

export const NO_MODEL: EffectiveModel = { id: '', provider: '', model: '' };

/** Split `provider/model` into its halves; `provider` is `''` without a slash. */
export function splitModelId(id: string): EffectiveModel {
  const trimmed = id.trim();
  if (!trimmed) {
    return NO_MODEL;
  }
  const slash = trimmed.indexOf('/');
  if (slash <= 0) {
    return { id: trimmed, provider: '', model: trimmed };
  }
  return { id: trimmed, provider: trimmed.slice(0, slash), model: trimmed.slice(slash + 1) };
}

function isPlaceholder(value: string | null | undefined): boolean {
  const v = (value ?? '').trim().toLowerCase();
  return !v || v === 'default' || v === '(default)';
}

export function resolveEffectiveModel(
  selected: string | null | undefined,
  meta: Pick<SessionMeta, 'agent' | 'model' | 'effective_model' | 'effective_provider'> | null | undefined,
  agents: readonly AgentInfo[] | null | undefined,
  configDefault: string | null | undefined,
): EffectiveModel {
  if (!isPlaceholder(selected)) {
    return splitModelId(selected!);
  }
  if (meta && !isPlaceholder(meta.effective_model)) {
    const split = splitModelId(meta.effective_model!);
    // The engine's provider wins when the id carries no prefix.
    return split.provider || !meta.effective_provider
      ? split
      : { ...split, provider: meta.effective_provider };
  }
  if (meta && !isPlaceholder(meta.model)) {
    return splitModelId(meta.model!);
  }
  const preset = meta ? agents?.find((a) => a.name === meta.agent) : undefined;
  if (preset && !isPlaceholder(preset.model)) {
    return splitModelId(preset.model!);
  }
  if (!isPlaceholder(configDefault)) {
    return splitModelId(configDefault!);
  }
  return NO_MODEL;
}
