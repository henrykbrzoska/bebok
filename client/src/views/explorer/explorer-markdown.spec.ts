/**
 * F2-17: the Explorer's lightweight markdown reading view.
 *
 * The handoff asks for bold headings and colored bullet keywords only - these
 * specs pin that shape, plus the fact that no raw HTML ever survives the parse
 * (rows are rendered with plain bindings, never `innerHTML`).
 */

import { toMarkdownRows } from './explorer-markdown';

describe('Explorer markdown rows (F2-17)', () => {
  it('turns `#`/`##` into heading rows and strips the markers', () => {
    const rows = toMarkdownRows('# Bebok\n\n## Features');
    expect(rows[0]).toEqual({ kind: 'h1', text: 'Bebok' });
    expect(rows[1].kind).toBe('blank');
    expect(rows[2]).toEqual({ kind: 'h2', text: 'Features' });
  });

  it('splits a bullet into the colored keyword and the rest', () => {
    const rows = toMarkdownRows('- Rust engine + SSE - axum, tokio; all logic in the engine.');
    expect(rows[0]).toEqual({
      kind: 'bullet',
      keyword: 'Rust engine + SSE',
      text: 'axum, tokio; all logic in the engine.',
    });
  });

  it('uses a leading bold run as the keyword and drops the emphasis markers', () => {
    const rows = toMarkdownRows('- **Permissions**: globset patterns');
    expect(rows[0]).toEqual({ kind: 'bullet', keyword: 'Permissions', text: 'globset patterns' });
  });

  it('keeps a keyword-less bullet as plain text', () => {
    const rows = toMarkdownRows('- just one phrase');
    expect(rows[0]).toEqual({ kind: 'bullet', keyword: null, text: 'just one phrase' });
  });

  it('keeps fenced code verbatim and never emits markup', () => {
    const rows = toMarkdownRows('```ts\nconst a = <b>1</b>;\n```');
    expect(rows.length).toBe(1);
    expect(rows[0]).toEqual({ kind: 'code', text: 'const a = <b>1</b>;' });
  });
});
