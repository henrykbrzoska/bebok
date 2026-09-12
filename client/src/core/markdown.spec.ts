/**
 * E2E-B1/B4: the chat markdown renderer must HTML-escape model text before
 * applying any markup (raw `<main>` / `<script>` from the model must never
 * reach the DOM), while fenced code keeps its syntax highlighting, and ATX
 * headings render as real heading elements.
 */

import { preloadLanguage } from '../ui/code-highlight/code-highlight';
import { renderMarkdown } from './markdown';

describe('renderMarkdown', () => {
  it('escapes raw HTML tags in plain text', () => {
    const html = renderMarkdown('uses a real <main> and <header>');
    expect(html).toBe('<p>uses a real &lt;main&gt; and &lt;header&gt;</p>');
    expect(html).not.toContain('<main>');
  });

  it('escapes a <script> tag so it can never execute', () => {
    const html = renderMarkdown('hello <script>alert(1)</script> world');
    expect(html).not.toContain('<script>');
    expect(html).toContain('&lt;script&gt;alert(1)&lt;/script&gt;');
  });

  it('escapes tags inside inline code and list items', () => {
    const html = renderMarkdown('- uses a real `<main>`, `<header>`\n- and `<nav>`');
    expect(html).toContain('<ul><li>uses a real <code>&lt;main&gt;</code>, <code>&lt;header&gt;</code></li>');
    expect(html).toContain('<li>and <code>&lt;nav&gt;</code></li></ul>');
    expect(html).not.toMatch(/<(main|header|nav)>/);
  });

  it('escapes generics and ampersands', () => {
    const html = renderMarkdown('Array<string> & Map<K, V>');
    expect(html).toBe('<p>Array&lt;string&gt; &amp; Map&lt;K, V&gt;</p>');
  });

  it('does not let an escaped quote break out of a link attribute', () => {
    const html = renderMarkdown('[x](docs/a"onclick="alert(1).md)');
    expect(html).not.toContain('"onclick="');
    expect(html).toContain('&quot;');
  });

  it('keeps bold, links and bare .md paths working on escaped text', () => {
    const html = renderMarkdown('**bold** [site](https://example.com/?a=1&b=2) see docs/plan.md');
    expect(html).toContain('<strong>bold</strong>');
    expect(html).toContain('<a href="https://example.com/?a=1&amp;b=2" target="_blank" rel="noreferrer">site</a>');
    expect(html).toContain('<a href="docs/plan.md" class="preview-link">docs/plan.md</a>');
  });

  it('keeps fenced code escaped and syntax-highlighted', async () => {
    await preloadLanguage('ts');
    const html = renderMarkdown('```ts\nconst a = <b>1</b>;\n```');
    expect(html).toContain('<pre><code class="hljs language-typescript">');
    expect(html).not.toContain('<b>1</b>');
    expect(html).toContain('&lt;b&gt;1&lt;/b&gt;');
    expect(html).toContain('hljs-keyword');
  });

  it('does not double-escape fenced code', () => {
    const html = renderMarkdown('```not-a-real-language\n<b>x</b> & y\n```');
    expect(html).toContain('&lt;b&gt;x&lt;/b&gt; &amp; y');
    expect(html).not.toContain('&amp;lt;');
  });

  it('renders ATX headings (B4), never as h1', () => {
    const html = renderMarkdown('## Positive aspects\n- one\n\n### High priority\ntext <x>\n\n# Top');
    expect(html).toContain('<h2>Positive aspects</h2><ul><li>one</li></ul>');
    expect(html).toContain('<h3>High priority</h3><p>text &lt;x&gt;</p>');
    expect(html).toContain('<h2>Top</h2>');
    expect(html).not.toContain('<h1>');
    expect(html).not.toContain('## ');
  });

  it('leaves a lone # that is not a heading alone', () => {
    expect(renderMarkdown('#hashtag and C# code')).toBe('<p>#hashtag and C# code</p>');
  });
});
