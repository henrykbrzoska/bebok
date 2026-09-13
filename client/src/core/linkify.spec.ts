/**
 * F9-11: every http(s) URL mentioned in assistant/user/tool-result text must
 * become a clickable `target="_blank" rel="noopener noreferrer"` link,
 * including a bare URL in plain prose - not only markdown `[x](y)` links or
 * backticked ones - without double-wrapping a URL already inside an `<a>`
 * (markdown syntax, or the `url` inline-code chip from
 * `core/inline-classify.ts`) or inside a `<code>` span.
 */

import { LINK_REL, escapeAndLinkify, linkifyOutsideTags, protectUrls, renderSchemeLink } from './linkify';

describe('linkifyOutsideTags', () => {
  it('linkifies a bare http URL in plain prose', () => {
    const html = linkifyOutsideTags('Server at http://example.com is up');
    expect(html).toBe(
      'Server at <a href="http://example.com" target="_blank" rel="noopener noreferrer">http://example.com</a> is up',
    );
  });

  it('linkifies a bare https URL in plain prose', () => {
    const html = linkifyOutsideTags('Check out https://example.com for more');
    expect(html).toContain(
      '<a href="https://example.com" target="_blank" rel="noopener noreferrer">https://example.com</a>',
    );
  });

  it('excludes a trailing period from the link', () => {
    const html = linkifyOutsideTags('See https://example.com.');
    expect(html).toBe(
      'See <a href="https://example.com" target="_blank" rel="noopener noreferrer">https://example.com</a>.',
    );
  });

  it('excludes a trailing comma from the link', () => {
    const html = linkifyOutsideTags('Visit https://example.com, then continue');
    expect(html).toContain('<a href="https://example.com" target="_blank" rel="noopener noreferrer">https://example.com</a>,');
  });

  it('excludes a trailing semicolon from the link', () => {
    const html = linkifyOutsideTags('Per https://example.com; see below');
    expect(html).toContain('<a href="https://example.com" target="_blank" rel="noopener noreferrer">https://example.com</a>;');
  });

  it('excludes a sentence-wrapping closing paren that is not part of the URL', () => {
    const html = linkifyOutsideTags('(https://example.com/page) end');
    expect(html).toBe(
      '(<a href="https://example.com/page" target="_blank" rel="noopener noreferrer">https://example.com/page</a>) end',
    );
  });

  it('keeps a balanced closing paren that IS part of the URL (Wikipedia-style link)', () => {
    const html = linkifyOutsideTags('https://en.wikipedia.org/wiki/Foo_(bar)');
    expect(html).toBe(
      '<a href="https://en.wikipedia.org/wiki/Foo_(bar)" target="_blank" rel="noopener noreferrer">https://en.wikipedia.org/wiki/Foo_(bar)</a>',
    );
  });

  it('strips only the sentence paren around a Wikipedia-style link, keeping the URL intact', () => {
    const html = linkifyOutsideTags('See (https://en.wikipedia.org/wiki/Foo_(bar)) for background.');
    expect(html).toBe(
      'See (<a href="https://en.wikipedia.org/wiki/Foo_(bar)" target="_blank" rel="noopener noreferrer">https://en.wikipedia.org/wiki/Foo_(bar)</a>) for background.',
    );
  });

  it('includes a query string and fragment, decoding an escaped ampersand as a literal one', () => {
    const html = linkifyOutsideTags('Go to https://example.com/search?q=test&amp;lang=en#section now');
    expect(html).toContain(
      '<a href="https://example.com/search?q=test&amp;lang=en#section" target="_blank" rel="noopener noreferrer">' +
        'https://example.com/search?q=test&amp;lang=en#section</a>',
    );
  });

  it('stops the URL before an escaped quote/angle-bracket entity instead of swallowing it', () => {
    const html = linkifyOutsideTags('quoted &quot;https://example.com&quot; here');
    expect(html).toBe(
      'quoted &quot;<a href="https://example.com" target="_blank" rel="noopener noreferrer">https://example.com</a>&quot; here',
    );
  });

  it('does not double-wrap a URL already inside an <a> element', () => {
    const html = '<a href="https://example.com">https://example.com</a>';
    expect(linkifyOutsideTags(html)).toBe(html);
  });

  it('does not linkify a URL inside a <code> span, but does linkify plain text around it', () => {
    const html = linkifyOutsideTags('https://a.com <code>https://b.com</code> https://c.com');
    expect(html).toContain('<a href="https://a.com" target="_blank" rel="noopener noreferrer">https://a.com</a> ');
    expect(html).toContain('<code>https://b.com</code>');
    expect(html).not.toContain('<code><a');
    expect(html).toContain(' <a href="https://c.com" target="_blank" rel="noopener noreferrer">https://c.com</a>');
  });

  it('linkifies a URL inside bold (<strong>) text', () => {
    const html = linkifyOutsideTags('<strong>Note: https://example.com is important</strong>');
    expect(html).toBe(
      '<strong>Note: <a href="https://example.com" target="_blank" rel="noopener noreferrer">https://example.com</a> is important</strong>',
    );
  });

  it('linkifies a URL inside a list item', () => {
    const html = linkifyOutsideTags('<ul><li>See https://example.com</li></ul>');
    expect(html).toBe(
      '<ul><li>See <a href="https://example.com" target="_blank" rel="noopener noreferrer">https://example.com</a></li></ul>',
    );
  });

  it('linkifies a URL immediately adjacent to a closing </code> tag, no space needed', () => {
    const html = linkifyOutsideTags('<code>x</code>https://example.com');
    expect(html).toBe(
      '<code>x</code><a href="https://example.com" target="_blank" rel="noopener noreferrer">https://example.com</a>',
    );
  });

  it('linkifies a localhost URL', () => {
    const html = linkifyOutsideTags('Server running at http://localhost:3000/api');
    expect(html).toContain(
      '<a href="http://localhost:3000/api" target="_blank" rel="noopener noreferrer">http://localhost:3000/api</a>',
    );
  });

  it('linkifies a 127.0.0.1 URL', () => {
    const html = linkifyOutsideTags('Bound to http://127.0.0.1:8080');
    expect(html).toContain(
      '<a href="http://127.0.0.1:8080" target="_blank" rel="noopener noreferrer">http://127.0.0.1:8080</a>',
    );
  });

  it('linkifies multiple independent URLs in one string', () => {
    const html = linkifyOutsideTags('Compare http://a.com and https://b.com now');
    expect(html).toContain('<a href="http://a.com" target="_blank" rel="noopener noreferrer">http://a.com</a>');
    expect(html).toContain('<a href="https://b.com" target="_blank" rel="noopener noreferrer">https://b.com</a>');
  });

  it('always pairs target="_blank" with the shared LINK_REL constant', () => {
    expect(LINK_REL).toBe('noopener noreferrer');
    const html = linkifyOutsideTags('https://example.com');
    expect(html).toContain(`target="_blank" rel="${LINK_REL}"`);
  });

  it('does not touch text with no URL in it', () => {
    const html = '<p>nothing to see here</p>';
    expect(linkifyOutsideTags(html)).toBe(html);
  });
});

describe('escapeAndLinkify (plain-text tool output: linkify only, no markdown)', () => {
  it('escapes raw HTML and linkifies a bare URL in the same pass', () => {
    const html = escapeAndLinkify('Error at <module> - see https://example.com/help for details');
    expect(html).toContain('&lt;module&gt;');
    expect(html).not.toContain('<module>');
    expect(html).toContain(
      '<a href="https://example.com/help" target="_blank" rel="noopener noreferrer">https://example.com/help</a>',
    );
  });

  it('leaves markdown-looking syntax as literal text (no markdown rendering)', () => {
    const html = escapeAndLinkify('**not bold** and `not code` at https://example.com');
    expect(html).toContain('**not bold**');
    expect(html).toContain('`not code`');
    expect(html).not.toContain('<strong>');
    expect(html).not.toContain('<code>');
    expect(html).toContain('<a href="https://example.com" target="_blank" rel="noopener noreferrer">https://example.com</a>');
  });

  it('escapes a literal ampersand in a raw (unescaped) query string before linkifying', () => {
    const html = escapeAndLinkify('Go to https://example.com/a?x=1&y=2 now');
    expect(html).toContain(
      '<a href="https://example.com/a?x=1&amp;y=2" target="_blank" rel="noopener noreferrer">' +
        'https://example.com/a?x=1&amp;y=2</a>',
    );
  });
});

describe('protectUrls (used by diff-view.ts to survive syntax highlighting)', () => {
  it('protects a bare URL with a placeholder, then restores it as a real link', () => {
    const guarded = protectUrls('See https://example.com for details');
    expect(guarded.text).not.toContain('https://example.com');
    expect(guarded.restore(guarded.text)).toBe(
      'See <a href="https://example.com" target="_blank" rel="noopener noreferrer">https://example.com</a> for details',
    );
  });

  it('excludes trailing punctuation from the protected/restored URL', () => {
    const guarded = protectUrls('Visit https://example.com.');
    expect(guarded.restore(guarded.text)).toBe(
      'Visit <a href="https://example.com" target="_blank" rel="noopener noreferrer">https://example.com</a>.',
    );
  });

  it('handles multiple URLs independently with separate placeholders', () => {
    const guarded = protectUrls('Compare http://a.com and https://b.com now');
    const restored = guarded.restore(guarded.text);
    expect(restored).toContain('<a href="http://a.com" target="_blank" rel="noopener noreferrer">http://a.com</a>');
    expect(restored).toContain('<a href="https://b.com" target="_blank" rel="noopener noreferrer">https://b.com</a>');
  });

  it('restore is a no-op when there is nothing to restore', () => {
    const guarded = protectUrls('no links here');
    expect(guarded.text).toBe('no links here');
    expect(guarded.restore('<p>no links here</p>')).toBe('<p>no links here</p>');
  });

  it('keeps a URL intact through a highlighter that treats // as a comment start (the bug this guards against)', () => {
    const raw = 'fetched https://example.com/data.json ok';
    const guarded = protectUrls(raw);
    // The placeholder text no longer contains a literal "//" for a C-style
    // line-comment rule to catch - the URL is already swapped out.
    expect(guarded.text).not.toContain('//');

    // A naive stand-in for a highlighter that (like several real hljs
    // grammars) starts a comment span at the first `//` it sees: fed the
    // *raw* URL directly, this is exactly what would otherwise split
    // "https:" from "//example.com/..." into two separate text nodes.
    const fakeHighlighter = (text: string): string => {
      const i = text.indexOf('//');
      return i === -1 ? text : `${text.slice(0, i)}<span class="hljs-comment">${text.slice(i)}</span>`;
    };
    const highlighted = fakeHighlighter(guarded.text);
    expect(guarded.restore(highlighted)).toBe(
      'fetched <a href="https://example.com/data.json" target="_blank" rel="noopener noreferrer">' +
        'https://example.com/data.json</a> ok',
    );
  });
});

describe('renderSchemeLink', () => {
  it('renders a scheme-less href with the given relative class', () => {
    expect(renderSchemeLink('see', './other.md', 'preview-link', false)).toBe(
      '<a href="./other.md" class="preview-link">see</a>',
    );
  });

  it('renders an http(s) href with target=_blank and the shared rel', () => {
    expect(renderSchemeLink('site', 'https://example.com', 'preview-link', true)).toBe(
      `<a href="https://example.com" target="_blank" rel="${LINK_REL}">site</a>`,
    );
  });

  it('refuses a javascript: href, rendering the plain label with no link at all', () => {
    expect(renderSchemeLink('click me', 'javascript:alert(1)', 'preview-link', true)).toBe('click me');
  });

  it('refuses a javascript: href case-insensitively and with surrounding whitespace', () => {
    expect(renderSchemeLink('click me', '  JavaScript:alert(1)', 'relative-link', true)).toBe('click me');
  });
});
