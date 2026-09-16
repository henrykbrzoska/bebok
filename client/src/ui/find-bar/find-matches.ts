/**
 * Pure matching helpers for the find bar (Ctrl+F).
 *
 * No DOM, no Angular, no state: a query plus the three option toggles turn
 * into a `RegExp` (`buildFindRegex`), and a text plus a regex turn into the
 * list of match ranges (`findAll`). A view then highlights those ranges - via
 * the CSS Custom Highlight API, or by wrapping them in `<mark>` elements (see
 * `find-highlight.ts`).
 */

import { FindOptions } from '../../core/find.store';

/** Default upper bound of matches collected for one scan. */
export const DEFAULT_MATCH_CAP = 500;

export interface FindRange {
  /** Offset of the match within the scanned text. */
  start: number;
  /** Length of the match in characters (never 0 for real matches). */
  length: number;
}

export interface FindAllResult {
  ranges: FindRange[];
  /** True when the scan stopped at `cap` and more matches may follow. */
  limited: boolean;
}

/** `buildFindRegex` result: either a matcher or a "bad regex" marker. */
export type FindRegexResult = { regex: RegExp } | { error: 'regex' };

/** Quote regex metacharacters so a literal query matches literally. */
export function escapeRegExp(query: string): string {
  return query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * Turn a query into a global `RegExp`.
 *
 * - `useRegex` off: the query is escaped, so `.` and friends are literal.
 * - `wholeWord` on: the pattern is wrapped in `\b...\b` (so `cat` no longer
 *   hits `category`).
 * - `matchCase` off (default): the `i` flag.
 *
 * An invalid regular expression never throws - it returns `{error:'regex'}`
 * so the bar can show "invalid pattern" instead of crashing the view. An
 * empty query yields an empty-source regex; `findAll` treats that as "no
 * matches".
 */
export function buildFindRegex(query: string, options: FindOptions): FindRegexResult {
  const { matchCase, wholeWord, useRegex } = options;
  let source = useRegex ? query : escapeRegExp(query);
  if (wholeWord && source.length > 0) {
    source = `\\b(?:${source})\\b`;
  }
  const flags = matchCase ? 'g' : 'gi';
  try {
    return { regex: new RegExp(source, flags) };
  } catch {
    return { error: 'regex' };
  }
}

/**
 * Collect up to `cap` match ranges of `regex` in `text`.
 *
 * The matcher is always cloned with the `g` flag so a sticky or non-global
 * caller regex cannot loop forever, and a zero-length match advances
 * `lastIndex` by one so `a*`-style patterns terminate. An empty query (empty
 * regex source) or empty text yields no ranges at all - an empty pattern
 * would otherwise match at every position.
 */
export function findAll(
  text: string,
  regex: RegExp,
  cap: number = DEFAULT_MATCH_CAP,
): FindAllResult {
  const ranges: FindRange[] = [];
  // `new RegExp('')` reports its source as `(?:)` - both spellings mean "empty
  // query", which must yield no ranges (it would match between every char).
  if (!text || !regex || regex.source.length === 0 || regex.source === '(?:)') {
    return { ranges, limited: false };
  }
  const limit = Number.isFinite(cap) ? Math.max(0, Math.floor(cap)) : DEFAULT_MATCH_CAP;
  const flags = `${regex.flags.replace(/[gy]/g, '')}g`;
  const matcher = new RegExp(regex.source, flags);

  let limited = false;
  let match: RegExpExecArray | null = matcher.exec(text);
  while (match !== null) {
    if (ranges.length >= limit) {
      limited = true;
      break;
    }
    ranges.push({ start: match.index, length: match[0].length });
    if (match[0].length === 0) {
      matcher.lastIndex += 1;
    }
    match = matcher.exec(text);
  }
  return { ranges, limited };
}
