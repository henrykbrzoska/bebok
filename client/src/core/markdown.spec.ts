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
    expect(html).toContain(
      '<ul><li>uses a real <code class="ic">&lt;main&gt;</code>, <code class="ic">&lt;header&gt;</code></li>',
    );
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
    expect(html).toContain(
      '<a href="https://example.com/?a=1&amp;b=2" target="_blank" rel="noopener noreferrer">site</a>',
    );
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
    const html = renderMarkdown(
      '## Positive aspects\n- one\n\n### High priority\ntext <x>\n\n# Top',
    );
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
      expect(html).toContain(
        '<code class="ic ic-shell"><span class="ic-shell-glyph">$</span>npm exec nx -- generate lib</code>',
      );
    });

    it('tokenizes an inline JSON object', () => {
      const html = renderMarkdown('returns `{"status":"ok"}`');
      expect(html).toContain('<code class="ic ic-json">');
      expect(html).toContain('<span class="ic-jt-key">&quot;status&quot;</span>');
      expect(html).toContain('<span class="ic-jt-string">&quot;ok&quot;</span>');
    });

    it('splits a package@version chip into name and faint version', () => {
      const html = renderMarkdown('needs `@nx/angular@23.2.1`');
      expect(html).toContain(
        '<code class="ic ic-package">@nx/angular<span class="ic-pkg-version">@23.2.1</span></code>',
      );
    });

    it('keeps everything HTML-escaped no matter the classified kind', () => {
      const html = renderMarkdown('`<script>alert(1)</script>`');
      expect(html).not.toContain('<script>');
      expect(html).toContain('&lt;script&gt;alert(1)&lt;/script&gt;');
    });
  });

  describe('F9-11 bare URL linkification', () => {
    it('linkifies a bare URL in prose with target=_blank and the shared rel', () => {
      const html = renderMarkdown('See https://example.com for details.');
      expect(html).toBe(
        '<p>See <a href="https://example.com" target="_blank" rel="noopener noreferrer">https://example.com</a> for details.</p>',
      );
    });

    it('excludes trailing punctuation from bare URLs', () => {
      const html = renderMarkdown('Visit https://example.com/page, then https://example.org.');
      expect(html).toContain(
        '<a href="https://example.com/page" target="_blank" rel="noopener noreferrer">https://example.com/page</a>,',
      );
      expect(html).toContain(
        '<a href="https://example.org" target="_blank" rel="noopener noreferrer">https://example.org</a>.',
      );
    });

    it('linkifies a bare URL inside a list item', () => {
      const html = renderMarkdown('- see https://example.com');
      expect(html).toBe(
        '<ul><li>see <a href="https://example.com" target="_blank" rel="noopener noreferrer">https://example.com</a></li></ul>',
      );
    });

    it('linkifies a bare URL inside bold text', () => {
      const html = renderMarkdown('**https://example.com is important**');
      expect(html).toBe(
        '<p><strong><a href="https://example.com" target="_blank" rel="noopener noreferrer">https://example.com</a> is important</strong></p>',
      );
    });

    it('does not double-wrap a URL already in markdown link syntax', () => {
      const html = renderMarkdown('[site](https://example.com)');
      expect((html.match(/<a /g) ?? []).length).toBe(1);
    });

    it('does not double-wrap a URL already turned into the `url` inline chip', () => {
      const html = renderMarkdown('`https://example.com`');
      expect((html.match(/<a /g) ?? []).length).toBe(1);
      expect(html).toContain('class="ic ic-url"');
    });

    it('linkifies a localhost URL', () => {
      const html = renderMarkdown('running at http://localhost:4200');
      expect(html).toContain(
        '<a href="http://localhost:4200" target="_blank" rel="noopener noreferrer">http://localhost:4200</a>',
      );
    });
  });
});

describe('renderMarkdown: diagrams and images (1.8)', () => {
  it('turns a ```mermaid fence into a mermaid block that keeps the escaped source', () => {
    const html = renderMarkdown('Flow:\n```mermaid\ngraph TD\n  A-->B\n```\ndone');
    expect(html).toContain(
      '<div class="mermaid-block"><pre class="mermaid-src"><code>graph TD\n  A--&gt;B</code></pre>',
    );
    expect(html).not.toContain('<svg');
  });

  it('renders ![alt](src) as an image for http(s) and data URLs only', () => {
    expect(renderMarkdown('![shot](https://x.test/a.png)')).toContain(
      '<img class="md-image" src="https://x.test/a.png" alt="shot" loading="lazy">',
    );
    expect(renderMarkdown('![](data:image/png;base64,AAAA)')).toContain(
      '<img class="md-image" src="data:image/png;base64,AAAA"',
    );
    const local = renderMarkdown('![diagram](docs/diagram.png)');
    expect(local).not.toContain('<img');
    expect(local).toContain('diagram');
    expect(renderMarkdown('![x](javascript:alert(1))')).not.toContain('<img');
  });
});
