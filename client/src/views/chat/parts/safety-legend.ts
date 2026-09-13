/**
 * Safety-dot legend (WP-CHAT4 / F7-7), shared by the single tool-call dot
 * (`tool-part.ts`) and the group header cluster (`tool-group.ts`).
 *
 * Plain-text tooltip composition (the same pattern as other count-based
 * labels in the app): "<label of the current category> · <legend>", where
 * the legend lists every category with its colour word so a gray dot is
 * self-explaining ("uncategorized - set it in Settings > Permissions").
 */

import { SAFETY_CATEGORIES, SafetyCategory } from '../../../core/engine.dtos';
import type { MessageKey } from '../../../i18n';

export type Translate = (key: MessageKey, params?: Record<string, unknown>) => string;

/** i18n key of a category's short label. */
export function safetyCategoryKey(category: SafetyCategory): MessageKey {
  switch (category) {
    case 'safe':
      return 'tool.safetySafe';
    case 'caution':
      return 'tool.safetyCaution';
    case 'dangerous':
      return 'tool.safetyDangerous';
    default:
      return 'tool.safetyUncategorized';
  }
}

export function safetyCategoryLabel(t: Translate, category: SafetyCategory): string {
  return t(safetyCategoryKey(category));
}

/** Colour word shown next to each legend entry. */
function colourKey(category: SafetyCategory): MessageKey {
  switch (category) {
    case 'safe':
      return 'tool.safetyColorGreen';
    case 'caution':
      return 'tool.safetyColorYellow';
    case 'dangerous':
      return 'tool.safetyColorOrange';
    default:
      return 'tool.safetyColorGray';
  }
}

/**
 * The legend line, optionally prefixed by the current category's label:
 * "dangerous · legend: ● green safe · ● yellow caution · ● orange dangerous · ● gray uncategorized".
 */
export function safetyLegend(t: Translate, current?: SafetyCategory): string {
  const entries = SAFETY_CATEGORIES.map(
    (c) => `● ${t(colourKey(c))} ${safetyCategoryLabel(t, c)}`,
  ).join(' · ');
  const legend = `${t('tool.safetyLegend')}: ${entries}`;
  if (!current) {
    return legend;
  }
  const head =
    current === 'uncategorized'
      ? `${safetyCategoryLabel(t, current)} - ${t('tool.safetyUncategorizedHint')}`
      : safetyCategoryLabel(t, current);
  return `${head}\n${legend}`;
}
