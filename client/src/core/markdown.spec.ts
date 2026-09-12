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
    // F8-3: none of these classify as anything special, so they keep the
    // plain fallback chip (`class="ic"`, no kind modifier).
    const html = renderMarkdown('- uses a real `<main>`, `<header>`\n- and `<nav>`');
    expect(html).toContain('<ul><li>uses a real <code class="ic">&lt;main&gt;</code>, <code class="ic">&lt;header&gt;</code></li>');
    expect(html).toContain('<li>and <code class="ic">&lt;nav&gt;</code></li></ul>');
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
    // Once the `xml` grammar is registered (by any earlier spec), TypeScript
    // tokenizes `<b>` as embedded markup; compare the text between the spans.
    expect(html.replace(/<\/?span[^>]*>/g, '')).toContain('&lt;b&gt;1&lt;/b&gt;');
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

  describe('F8-3 inline code classification', () => {
    it('gives a file path its own chip and makes a relative one clickable', () => {
      const html = renderMarkdown('see `apps/api`');
      expect(html).toContain('<a href="apps/api" class="preview-link ic ic-path">apps/api</a>');
    });

    it('does not linkify a Windows absolute path, only colors it', () => {
      const html = renderMarkdown('open `C:\\projects\\bebok\\README.md`');
      expect(html).toContain('<code class="ic ic-path">C:\\projects\\bebok\\README.md</code>');
      expect(html).not.toContain('<a href="C:');
    });

    it('badges an HTTP method + route', () => {
      const html = renderMarkdown('call `GET /api/health`');
      expect(html).toContain(
        '<code class="ic ic-http"><span class="ic-method ic-method-get">GET</span> /api/health</code>',
      );
    });

    it('renders a shell command as a terminal-like chip', () => {
      const html = renderMarkdown('run `npm exec nx -- generate lib`');
      expect(html).toContain('<code class="ic ic-shell"><span class="ic-shell-glyph">$</span>npm exec nx -- generate lib</code>');
    });

    it('tokenizes an inline JSON object', () => {
      const html = renderMarkdown('returns `{"status":"ok"}`');
      expect(html).toContain('<code class="ic ic-json">');
      expect(html).toContain('<span class="ic-jt-key">&quot;status&quot;</span>');
      expect(html).toContain('<span class="ic-jt-string">&quot;ok&quot;</span>');
    });

    it('splits a package@version chip into name and faint version', () => {
      const html = renderMarkdown('needs `@nx/angular@23.2.1`');
      expect(html).toContain('<code class="ic ic-package">@nx/angular<span class="ic-pkg-version">@23.2.1</span></code>');
    });

    it('keeps everything HTML-escaped no matter the classified kind', () => {
      const html = renderMarkdown('`<script>alert(1)</script>`');
      expect(html).not.toContain('<script>');
      expect(html).toContain('&lt;script&gt;alert(1)&lt;/script&gt;');
    });
  });
});
