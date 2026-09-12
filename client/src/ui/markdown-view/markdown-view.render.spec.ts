/**
 * F6-10: the shared markdown-view renderer. Pins headings/code/table/link
 * parsing - the four things the Preview panel needs that `explorer-markdown.ts`
 * deliberately does not support (see that file's header comment).
 */

import { renderMarkdownView } from './markdown-view.render';

describe('renderMarkdownView', () => {
  it('renders headings h1-h3', () => {
    const html = renderMarkdownView('# Title\n\n## Sub\n\n### Sub sub');
    expect(html).toContain('<h1>Title</h1>');
    expect(html).toContain('<h2>Sub</h2>');
    expect(html).toContain('<h3>Sub sub</h3>');
  });

  it('renders a fenced code block verbatim, escaped', () => {
    const html = renderMarkdownView('```ts\nconst a = <b>1</b>;\n```');
    expect(html).toContain('<pre><code data-lang="ts">const a = &lt;b&gt;1&lt;/b&gt;;</code></pre>');
  });

  it('parses a GFM pipe table with alignment', () => {
    const html = renderMarkdownView(
      ['| Name | Score |', '| :--- | ----: |', '| a | 1 |', '| b | 2 |'].join('\n'),
    );
    expect(html).toContain('<table>');
    expect(html).toContain('<th style="text-align:left">Name</th>');
    expect(html).toContain('<th style="text-align:right">Score</th>');
    expect(html).toContain('<td style="text-align:left">a</td>');
    expect(html).toContain('<td style="text-align:right">1</td>');
  });

  it('does not treat a plain paragraph containing `|` as a table', () => {
    const html = renderMarkdownView('a | b | c');
    expect(html).not.toContain('<table>');
    expect(html).toContain('<p>a | b | c</p>');
  });

  it('marks a relative link for click interception, leaves absolute links alone', () => {
    const html = renderMarkdownView('[see](./other.md) and [site](https://example.com)');
    expect(html).toContain('<a href="./other.md" data-relative="1">see</a>');
    expect(html).toContain('<a href="https://example.com" target="_blank" rel="noreferrer">site</a>');
  });

  it('renders bullet and numbered lists', () => {
    expect(renderMarkdownView('- a\n- b')).toBe('<ul><li>a</li><li>b</li></ul>');
    expect(renderMarkdownView('1. a\n2. b')).toBe('<ol><li>a</li><li>b</li></ol>');
  });

  it('renders a blockquote', () => {
    expect(renderMarkdownView('> quoted')).toBe('<blockquote><p>quoted</p></blockquote>');
  });

  it('escapes stray HTML in plain text so it is never injected verbatim', () => {
    const html = renderMarkdownView('<script>alert(1)</script>');
    expect(html).not.toContain('<script>');
    expect(html).toContain('&lt;script&gt;');
  });
});
