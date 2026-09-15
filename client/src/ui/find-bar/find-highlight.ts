/**
 * DOM helpers for drawing find-bar matches.
 *
 * Two strategies, in order of preference:
 *
 * 1. The CSS Custom Highlight API (`CSS.highlights` + `Highlight`), which
 *    paints ranges without touching the document tree - see
 *    `supportsHighlightApi()`. It keeps Angular's DOM (and any virtual-scroll
 *    bookkeeping) untouched.
 * 2. The fallback used here: split the text node and wrap the matched slice
 *    in a `<mark class="find-hit">` element (`wrapRangeWithMark`), then undo
 *    it with `unwrapMarks` when the bar closes or the matches move.
 *
 * None of these helpers throw when the API or the range is missing: a view
 * may call them on a detached node, with a stale range, or in a browser
 * without `CSS.highlights`, and simply get a no-op.
 */

import { FindRange } from './find-matches';

/** Class carried by the fallback `<mark>` elements. */
export const FIND_HIT_CLASS = 'find-hit';

/** Add this to `.find-hit` to style the match the bar is currently on. */
export const FIND_HIT_ACTIVE_CLASS = 'find-hit-active';

/**
 * True when the browser can highlight arbitrary ranges without DOM surgery.
 * Guarded by `typeof` checks so it is safe in SSR/older runtimes - it returns
 * `false` rather than throwing.
 */
export function supportsHighlightApi(): boolean {
  try {
    const css = (globalThis as { CSS?: { highlights?: unknown } }).CSS;
    const highlight = (globalThis as { Highlight?: unknown }).Highlight;
    return !!css && typeof highlight === 'function' && typeof css.highlights !== 'undefined';
  } catch {
    return false;
  }
}

/**
 * Wrap `[start, start+length)` of `textNode` in a `<mark class="find-hit">`.
 *
 * The slice is cut out with `splitText` (so surrounding text nodes keep their
 * identity) and the resulting node is moved inside a fresh `<mark>`. Out of
 * range or empty ranges are ignored.
 */
export function wrapRangeWithMark(textNode: Text, start: number, length: number): void {
  if (!textNode || textNode.nodeType !== Node.TEXT_NODE) {
    return;
  }
  if (!Number.isFinite(start) || !Number.isFinite(length) || length <= 0 || start < 0) {
    return;
  }
  const content = textNode.data;
  const from = Math.floor(start);
  const size = Math.floor(length);
  if (from + size > content.length) {
    return;
  }
  const parent = textNode.parentNode;
  if (!parent) {
    return;
  }
  let target = textNode;
  if (from > 0) {
    target = target.splitText(from);
  }
  if (size < target.data.length) {
    target.splitText(size);
  }
  const doc = target.ownerDocument ?? (globalThis.document as Document | undefined);
  if (!doc) {
    return;
  }
  const mark = doc.createElement('mark');
  mark.className = FIND_HIT_CLASS;
  parent.replaceChild(mark, target);
  mark.appendChild(target);
}

/**
 * Replace every `mark.find-hit` under `root` with its text and merge the
 * resulting adjacent text nodes, restoring the original text content.
 */
export function unwrapMarks(root: HTMLElement): void {
  if (!root || typeof root.querySelectorAll !== 'function') {
    return;
  }
  const marks = Array.from(root.querySelectorAll(`mark.${FIND_HIT_CLASS}`));
  if (typeof root.classList?.contains === 'function' && root.classList.contains(FIND_HIT_CLASS)) {
    marks.unshift(root);
  }
  for (const mark of marks) {
    const parent = mark.parentNode;
    if (!parent) {
      continue;
    }
    while (mark.firstChild) {
      parent.insertBefore(mark.firstChild, mark);
    }
    parent.removeChild(mark);
  }
  if (typeof root.normalize === 'function') {
    root.normalize();
  }
}

/** Convenience: wrap every range of a single text node. */
export function wrapRangesWithMarks(textNode: Text, ranges: readonly FindRange[]): void {
  // Walk backwards so splitting a later range never shifts an earlier one.
  for (let i = ranges.length - 1; i >= 0; i--) {
    const range = ranges[i];
    if (!range) {
      continue;
    }
    wrapRangeWithMark(textNode, range.start, range.length);
  }
}
