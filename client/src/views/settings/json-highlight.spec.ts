/**
 * F2-29: the Raw JSON tab's tokenizer and config-schema validator.
 *
 * Both are plain functions, so these specs need no TestBed.
 */

import {
  highlightJson,
  positionAt,
  tokenizeJson,
  topLevelKeyRanges,
  validateConfig,
} from './json-highlight';

describe('json-highlight (F2-29)', () => {
  it('tells object keys apart from string values', () => {
    const text = '{ "model": "zai/glm-4.6" }';
    const tokens = tokenizeJson(text).filter((t) => t.kind === 'key' || t.kind === 'string');
    expect(tokens.map((t) => t.kind)).toEqual(['key', 'string']);
    expect(text.slice(tokens[0].start, tokens[0].end)).toBe('"model"');
    expect(text.slice(tokens[1].start, tokens[1].end)).toBe('"zai/glm-4.6"');
  });

  it('colors numbers, booleans and null distinctly from punctuation', () => {
    const kinds = tokenizeJson('{"a":1,"b":true,"c":null}')
      .filter((t) => t.kind !== 'space')
      .map((t) => t.kind);
    expect(kinds).toContain('number');
    expect(kinds).toContain('boolean');
    expect(kinds).toContain('null');
    expect(kinds).toContain('punct');
  });

  it('only reports nested keys at their own depth', () => {
    const ranges = topLevelKeyRanges('{"ui": {"customCss": "x"}, "yolo": true}');
    expect([...ranges.keys()]).toEqual(['ui', 'yolo']);
  });

  it('accepts a valid config layer', () => {
    const text = '{\n  "model": "zai/glm-4.6",\n  "yolo": false,\n  "providers": []\n}\n';
    expect(validateConfig(text)).toEqual([]);
  });

  it('flags an unknown top-level key as a warning with its position', () => {
    const text = '{\n  "model": "x",\n  "nope": 1\n}\n';
    const diagnostics = validateConfig(text);
    expect(diagnostics.length).toBe(1);
    expect(diagnostics[0].severity).toBe('warning');
    expect(diagnostics[0].key).toBe('unknownKey');
    expect(diagnostics[0].params['name']).toBe('nope');
    expect(diagnostics[0].line).toBe(3);
    expect(text.slice(diagnostics[0].start, diagnostics[0].end)).toBe('"nope"');
  });

  it('flags a type mismatch as an error', () => {
    const diagnostics = validateConfig('{"yolo": "yes"}');
    expect(diagnostics.length).toBe(1);
    expect(diagnostics[0].severity).toBe('error');
    expect(diagnostics[0].key).toBe('typeMismatch');
    expect(diagnostics[0].params['expected']).toBe('boolean');
    expect(diagnostics[0].params['actual']).toBe('string');
  });

  it('reports a parse error instead of throwing', () => {
    const diagnostics = validateConfig('{"model": }');
    expect(diagnostics.length).toBe(1);
    expect(diagnostics[0].key).toBe('parse');
    expect(diagnostics[0].severity).toBe('error');
  });

  it('rejects a non-object document', () => {
    expect(validateConfig('[1, 2]')[0].key).toBe('notObject');
  });

  it('maps an offset to a 1-based line and column', () => {
    expect(positionAt('a\nbc', 3)).toEqual({ line: 2, column: 2 });
  });

  it('escapes HTML and marks diagnosed tokens', () => {
    const text = '{"nope": "<script>"}';
    const html = highlightJson(text, validateConfig(text));
    expect(html).not.toContain('<script>');
    expect(html).toContain('&lt;script&gt;');
    expect(html).toContain('jt-warn');
    expect(html).toContain('jt-key');
  });
});
