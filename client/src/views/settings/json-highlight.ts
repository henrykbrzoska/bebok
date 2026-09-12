/**
 * A tiny JSON tokenizer + config-schema validator for the Raw JSON tab
 * (WP-SETTINGS / F2-29).
 *
 * Hand-rolled on purpose: the client deliberately carries no code-editor or
 * JSON-schema dependency, and the whole job is one linear scan. The tokenizer
 * distinguishes keys, strings, numbers, booleans/null and punctuation so the
 * editor can color them; the validator reports parse errors, unknown top-level
 * keys and type mismatches with the exact character range so the offending
 * token can be highlighted inline instead of in a toast.
 *
 * The schema mirrors `bebok-core/src/config/loader.rs::apply_layer` - the keys
 * the engine actually reads out of a `config.json` layer.
 */

export type TokenKind =
  | 'key'
  | 'string'
  | 'number'
  | 'boolean'
  | 'null'
  | 'punct'
  | 'space'
  | 'invalid';

export interface JsonToken {
  kind: TokenKind;
  start: number;
  end: number;
  /** Nesting depth of the token's container (top-level members are 1). */
  depth: number;
}

export type DiagnosticKey = 'parse' | 'unknownKey' | 'typeMismatch' | 'notObject';

export interface JsonDiagnostic {
  severity: 'error' | 'warning';
  key: DiagnosticKey;
  /** Interpolation values for the i18n message. */
  params: Record<string, string>;
  line: number;
  column: number;
  start: number;
  end: number;
}

/** JSON type names used by the schema table below. */
type SchemaType = 'string' | 'number' | 'boolean' | 'object' | 'array';

/** Top-level keys the engine reads from a config layer, with their types. */
export const CONFIG_SCHEMA: Readonly<Record<string, SchemaType>> = {
  model: 'string',
  max_tokens: 'number',
  thinking: 'string',
  api_key: 'string',
  context_budget: 'number',
  tool_output_cap: 'number',
  yolo: 'boolean',
  models: 'object',
  providers: 'array',
  permission: 'object',
  mcp: 'object',
  skills: 'object',
  terminal: 'object',
  runtimes: 'object',
  ui: 'object',
  fleet: 'object',
};

const WHITESPACE = new Set([' ', '\t', '\n', '\r']);
const PUNCT = new Set(['{', '}', '[', ']', ':', ',']);

/**
 * Split JSON text into colorable tokens. Never throws: malformed input yields
 * `invalid` tokens and the scan continues, so a half-typed document still
 * highlights.
 */
export function tokenizeJson(text: string): JsonToken[] {
  const tokens: JsonToken[] = [];
  let i = 0;
  let depth = 0;
  // Inside an object, a string is a key until the ':' has been seen.
  const objectStack: boolean[] = [];
  let expectKey = false;

  while (i < text.length) {
    const ch = text[i];

    if (WHITESPACE.has(ch)) {
      const start = i;
      while (i < text.length && WHITESPACE.has(text[i])) {
        i += 1;
      }
      tokens.push({ kind: 'space', start, end: i, depth });
      continue;
    }

    if (PUNCT.has(ch)) {
      if (ch === '{') {
        depth += 1;
        objectStack.push(true);
        expectKey = true;
      } else if (ch === '[') {
        depth += 1;
        objectStack.push(false);
        expectKey = false;
      } else if (ch === '}' || ch === ']') {
        depth = Math.max(0, depth - 1);
        objectStack.pop();
        expectKey = objectStack[objectStack.length - 1] === true;
      } else if (ch === ':') {
        expectKey = false;
      } else if (ch === ',') {
        expectKey = objectStack[objectStack.length - 1] === true;
      }
      tokens.push({ kind: 'punct', start: i, end: i + 1, depth });
      i += 1;
      continue;
    }

    if (ch === '"') {
      const start = i;
      i += 1;
      let closed = false;
      while (i < text.length) {
        if (text[i] === '\\') {
          i += 2;
          continue;
        }
        if (text[i] === '"') {
          i += 1;
          closed = true;
          break;
        }
        if (text[i] === '\n') {
          break;
        }
        i += 1;
      }
      tokens.push({
        kind: closed ? (expectKey ? 'key' : 'string') : 'invalid',
        start,
        end: i,
        depth,
      });
      continue;
    }

    if (ch === '-' || (ch >= '0' && ch <= '9')) {
      const start = i;
      i += 1;
      while (i < text.length && /[0-9eE+\-.]/.test(text[i])) {
        i += 1;
      }
      tokens.push({ kind: 'number', start, end: i, depth });
      continue;
    }

    if (text.startsWith('true', i) || text.startsWith('false', i)) {
      const end = i + (text[i] === 't' ? 4 : 5);
      tokens.push({ kind: 'boolean', start: i, end, depth });
      i = end;
      continue;
    }

    if (text.startsWith('null', i)) {
      tokens.push({ kind: 'null', start: i, end: i + 4, depth });
      i += 4;
      continue;
    }

    const start = i;
    while (i < text.length && !WHITESPACE.has(text[i]) && !PUNCT.has(text[i]) && text[i] !== '"') {
      i += 1;
    }
    tokens.push({ kind: 'invalid', start, end: Math.max(i, start + 1), depth });
    i = Math.max(i, start + 1);
  }

  return tokens;
}

/** Character offset -> 1-based line/column. */
export function positionAt(text: string, offset: number): { line: number; column: number } {
  let line = 1;
  let lineStart = 0;
  for (let i = 0; i < offset && i < text.length; i += 1) {
    if (text[i] === '\n') {
      line += 1;
      lineStart = i + 1;
    }
  }
  return { line, column: offset - lineStart + 1 };
}

/** Ranges of the top-level object's key tokens, by key name. */
export function topLevelKeyRanges(text: string): Map<string, JsonToken> {
  const out = new Map<string, JsonToken>();
  for (const token of tokenizeJson(text)) {
    if (token.kind !== 'key' || token.depth !== 1) {
      continue;
    }
    const name = unquote(text.slice(token.start, token.end));
    if (!out.has(name)) {
      out.set(name, token);
    }
  }
  return out;
}

/**
 * Validate a config layer: JSON syntax, then the top-level key names and their
 * types. Unknown keys are warnings (the engine ignores them), type mismatches
 * are errors (the engine would silently skip the value).
 */
export function validateConfig(text: string): JsonDiagnostic[] {
  const diagnostics: JsonDiagnostic[] = [];
  const trimmed = text.trim();
  if (trimmed.length === 0) {
    return diagnostics;
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    const offset = parseErrorOffset(message, text);
    const { line, column } = positionAt(text, offset);
    diagnostics.push({
      severity: 'error',
      key: 'parse',
      params: { msg: message },
      line,
      column,
      start: offset,
      end: Math.min(text.length, offset + 1),
    });
    return diagnostics;
  }

  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    diagnostics.push({
      severity: 'error',
      key: 'notObject',
      params: {},
      line: 1,
      column: 1,
      start: 0,
      end: Math.min(text.length, 1),
    });
    return diagnostics;
  }

  const ranges = topLevelKeyRanges(text);
  for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
    const token = ranges.get(key);
    const start = token?.start ?? 0;
    const end = token?.end ?? 0;
    const { line, column } = positionAt(text, start);
    const expected = CONFIG_SCHEMA[key];
    if (!expected) {
      diagnostics.push({
        severity: 'warning',
        key: 'unknownKey',
        params: { name: key },
        line,
        column,
        start,
        end,
      });
      continue;
    }
    const actual = jsonTypeOf(value);
    if (actual !== expected) {
      diagnostics.push({
        severity: 'error',
        key: 'typeMismatch',
        params: { name: key, expected, actual },
        line,
        column,
        start,
        end,
      });
    }
  }
  return diagnostics;
}

/** Render highlighted HTML for `text`, marking the diagnostics' ranges. */
export function highlightJson(text: string, diagnostics: JsonDiagnostic[] = []): string {
  const tokens = tokenizeJson(text);
  let html = '';
  for (const token of tokens) {
    const slice = escapeHtml(text.slice(token.start, token.end));
    if (token.kind === 'space') {
      html += slice;
      continue;
    }
    const diagnostic = diagnostics.find((d) => d.start === token.start && d.end === token.end);
    const classes = [`jt-${token.kind}`];
    if (diagnostic) {
      classes.push(diagnostic.severity === 'error' ? 'jt-bad' : 'jt-warn');
    }
    html += `<span class="${classes.join(' ')}">${slice}</span>`;
  }
  // A trailing newline keeps the highlighted layer as tall as the textarea.
  return `${html}\n`;
}

function jsonTypeOf(value: unknown): SchemaType | 'null' {
  if (value === null) {
    return 'null';
  }
  if (Array.isArray(value)) {
    return 'array';
  }
  const t = typeof value;
  if (t === 'string' || t === 'number' || t === 'boolean' || t === 'object') {
    return t;
  }
  return 'string';
}

function unquote(raw: string): string {
  try {
    return JSON.parse(raw) as string;
  } catch {
    return raw.replace(/^"|"$/g, '');
  }
}

/** Best-effort character offset out of a `JSON.parse` error message. */
function parseErrorOffset(message: string, text: string): number {
  const atPosition = /position (\d+)/i.exec(message);
  if (atPosition) {
    return Math.min(text.length, Number(atPosition[1]));
  }
  const lineColumn = /line (\d+) column (\d+)/i.exec(message);
  if (lineColumn) {
    const lines = text.split('\n');
    let offset = 0;
    for (let i = 0; i < Number(lineColumn[1]) - 1 && i < lines.length; i += 1) {
      offset += lines[i].length + 1;
    }
    return Math.min(text.length, offset + Number(lineColumn[2]) - 1);
  }
  return 0;
}

function escapeHtml(raw: string): string {
  return raw
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}
