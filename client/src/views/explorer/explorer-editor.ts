/**
 * Pure helpers for the Explorer code editor (textarea over a highlighted
 * `<pre>`): symbol under the caret for Ctrl/Cmd+click go-to-definition and
 * line/column <-> offset conversion for jumping to a definition.
 */

/** Extensions the engine's `GET /fs/definition` understands. */
const GOTO_EXTS = new Set([
  'rs',
  'ts',
  'mts',
  'cts',
  'tsx',
  'js',
  'jsx',
  'mjs',
  'cjs',
  'php',
  'go',
  'py',
]);

/** True when go-to-definition is available for `path`. */
export function supportsGoto(path: string | null | undefined): boolean {
  if (!path) {
    return false;
  }
  const base = path.split(/[\\/]/).pop() ?? path;
  const dot = base.lastIndexOf('.');
  return dot > 0 && GOTO_EXTS.has(base.slice(dot + 1).toLowerCase());
}

const IDENT = /[\p{L}\p{N}_$]/u;

/**
 * Identifier around `offset` in `text` (the caret may sit right after the
 * last character). PHP `$var` loses its `$`; a PHP namespace path
 * (`App\Foo\Bar`) is not crossed, so only `Bar` is returned. Returns `null`
 * when the caret is not on an identifier or it starts with a digit.
 */
export function symbolAt(text: string, offset: number): string | null {
  const at = (i: number) => i >= 0 && i < text.length && IDENT.test(text[i]);
  let pos = Math.max(0, Math.min(offset, text.length));
  if (!at(pos) && at(pos - 1)) {
    pos -= 1;
  }
  if (!at(pos)) {
    return null;
  }
  let start = pos;
  let end = pos + 1;
  while (at(start - 1)) {
    start -= 1;
  }
  while (at(end)) {
    end += 1;
  }
  const word = text.slice(start, end).replace(/^\$+/, '');
  if (!word || /^\d/.test(word)) {
    return null;
  }
  return word;
}

/** Offset of a 1-based `line` / `column` (clamped to the text). */
export function offsetOf(text: string, line: number, column: number): number {
  let offset = 0;
  for (let l = 1; l < line; l++) {
    const nl = text.indexOf('\n', offset);
    if (nl < 0) {
      return text.length;
    }
    offset = nl + 1;
  }
  const lineEnd = text.indexOf('\n', offset);
  const max = lineEnd < 0 ? text.length : lineEnd;
  return Math.min(offset + Math.max(0, column - 1), max);
}

/** 1-based line of `offset`. */
export function lineOf(text: string, offset: number): number {
  let line = 1;
  const end = Math.min(offset, text.length);
  for (let i = 0; i < end; i++) {
    if (text.charCodeAt(i) === 10) {
      line++;
    }
  }
  return line;
}
