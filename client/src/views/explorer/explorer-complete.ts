/**
 * Pure helpers for the Explorer editor's completion popup (Step 3 of
 * `docs/editor-completion.md`): identifier prefix at the caret, byte offset
 * for `POST /fs/complete`, client-side filtering between requests, popup
 * placement and caret measurement in the transparent `<textarea>`.
 */

import type { FsCompleteItem } from '../../core/engine.dtos';

const IDENT = /[\p{L}\p{N}_]/u;

/** Identifier that ends at `offset` (PHP keeps a leading `$`). */
export function prefixAt(text: string, offset: number, php: boolean): string {
  const end = Math.max(0, Math.min(offset, text.length));
  let start = end;
  while (start > 0) {
    const ch = text[start - 1];
    if (IDENT.test(ch) || (php && ch === '$')) {
      start -= 1;
    } else {
      break;
    }
  }
  return text.slice(start, end);
}

/** UTF-8 byte offset of a UTF-16 index (`POST /fs/complete` uses bytes). */
export function utf8Offset(text: string, index: number): number {
  return new TextEncoder().encode(text.slice(0, Math.max(0, index))).length;
}

/** True for a PHP source path (variables carry a `$` in completion labels). */
export function isPhpPath(path: string | null | undefined): boolean {
  return !!path && /\.php$/i.test(path);
}

/** Keep the items whose label starts with `prefix` (case-insensitive, `$` ignored). */
export function filterCompletions(items: FsCompleteItem[], prefix: string): FsCompleteItem[] {
  const key = prefix.replace(/^\$/, '').toLowerCase();
  return items.filter((item) => item.label.replace(/^\$/, '').toLowerCase().startsWith(key));
}

/** Caret position relative to the textarea's content box, plus line height. */
export interface CaretBox {
  top: number;
  left: number;
  lineHeight: number;
}

const MIRROR_PROPS = [
  'boxSizing',
  'width',
  'fontFamily',
  'fontSize',
  'fontWeight',
  'fontStyle',
  'letterSpacing',
  'lineHeight',
  'tabSize',
  'paddingTop',
  'paddingRight',
  'paddingBottom',
  'paddingLeft',
  'borderTopWidth',
  'borderLeftWidth',
] as const;

/**
 * Measure where the caret at `offset` (UTF-16 index) is drawn. A hidden
 * mirror `<div>` with the textarea's typography holds the text before the
 * caret, and a marker span reports its offset. Scroll is subtracted so the
 * result is relative to the visible area.
 */
export function caretBox(area: HTMLTextAreaElement, offset: number): CaretBox {
  const style = getComputedStyle(area);
  const mirror = document.createElement('div');
  for (const prop of MIRROR_PROPS) {
    mirror.style[prop] = style[prop];
  }
  mirror.style.position = 'absolute';
  mirror.style.top = '0';
  mirror.style.left = '-99999px';
  mirror.style.visibility = 'hidden';
  mirror.style.whiteSpace = 'pre';
  mirror.style.overflow = 'hidden';
  mirror.textContent = area.value.slice(0, offset);
  const marker = document.createElement('span');
  marker.textContent = '​';
  mirror.appendChild(marker);
  document.body.appendChild(mirror);
  const top = marker.offsetTop - area.scrollTop;
  const left = marker.offsetLeft - area.scrollLeft;
  const lineHeight = parseFloat(style.lineHeight) || 20;
  mirror.remove();
  return { top, left, lineHeight };
}

/** Popup placement inside the editor box: below the caret, or above it when
 *  there is no room, clamped horizontally to the box. */
export function popupPlacement(
  caret: CaretBox,
  box: { width: number; height: number },
  popupWidth = 320,
  popupHeight = 240,
): { left: number; top?: number; bottom?: number } {
  const left = Math.max(0, Math.min(caret.left, box.width - popupWidth));
  if (caret.top + caret.lineHeight + popupHeight > box.height && caret.top > popupHeight) {
    return { left, bottom: box.height - caret.top };
  }
  return { left, top: caret.top + caret.lineHeight };
}
