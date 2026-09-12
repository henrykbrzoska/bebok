/**
 * F8-3: `classifyInline` picks which chip an inline (backtick) code span
 * gets in chat. Cases below are grouped by kind, plus a block of
 * false-positive guards - the classifier must stay conservative and leave
 * ordinary prose or ambiguous tokens as the plain `code` fallback rather
 * than over-claiming a kind.
 */

import { classifyInline, highlightInlineJson, renderClassifiedInlineCode } from './inline-classify';

describe('classifyInline', () => {
  // --- path ---------------------------------------------------------
  it('classifies a relative dir/file path', () => {
    expect(classifyInline('apps/api')).toBe('path');
  });
  it('classifies a relative path with an extension', () => {
    expect(classifyInline('docs/x.md')).toBe('path');
  });
  it('classifies a nested source path', () => {
    expect(classifyInline('src/core/markdown.ts')).toBe('path');
  });
  it('classifies a bare filename with a known extension', () => {
    expect(classifyInline('package.json')).toBe('path');
  });
  it('classifies a Windows absolute path', () => {
    expect(classifyInline('C:\\projects\\bebok\\README.md')).toBe('path');
  });
  it('classifies a dotted-relative path', () => {
    expect(classifyInline('./src/main.ts')).toBe('path');
  });

  // --- url / route ----------------------------------------------------
  it('classifies an http(s) URL', () => {
    expect(classifyInline('http://127.0.0.1:8787')).toBe('url');
  });
  it('classifies an https URL with a path', () => {
    expect(classifyInline('https://example.com/docs')).toBe('url');
  });
  it('classifies a bare API route with no extension', () => {
    expect(classifyInline('/api/health')).toBe('route');
  });
  it('classifies a deeper bare route', () => {
    expect(classifyInline('/session/123/events')).toBe('route');
  });

  // --- http method + route --------------------------------------------
  it('classifies GET + route as http', () => {
    expect(classifyInline('GET /api/health')).toBe('http');
  });
  it('classifies POST + route as http', () => {
    expect(classifyInline('POST /session')).toBe('http');
  });
  it('classifies DELETE + route as http', () => {
    expect(classifyInline('DELETE /session/42')).toBe('http');
  });

  // --- shell ------------------------------------------------------------
  it('classifies an npm invocation as shell', () => {
    expect(classifyInline('npm exec nx -- generate lib')).toBe('shell');
  });
  it('classifies a git command as shell', () => {
    expect(classifyInline('git commit -m "fix"')).toBe('shell');
  });
  it('classifies a cargo command as shell', () => {
    expect(classifyInline('cargo build --release')).toBe('shell');
  });
  it('classifies a windows `set` + chained command as shell', () => {
    expect(classifyInline('set "NX_IGNORE_UNSUPPORTED_TS_SETUP=true" && npm exec nx -- generate lib')).toBe('shell');
  });
  it('classifies a $-prompt-prefixed command as shell', () => {
    expect(classifyInline('$ npm install')).toBe('shell');
  });
  it('classifies flag+pipe combination as shell even with an unknown program', () => {
    expect(classifyInline('some-tool --verbose | grep error')).toBe('shell');
  });

  // --- env / constant -----------------------------------------------
  it('classifies an ALL_CAPS env var', () => {
    expect(classifyInline('NX_IGNORE_UNSUPPORTED_TS_SETUP')).toBe('env');
  });
  it('classifies a generic ALL_CAPS_WITH_UNDERSCORES constant', () => {
    expect(classifyInline('ALL_CAPS_WITH_UNDERSCORES')).toBe('env');
  });

  // --- package@version --------------------------------------------------
  it('classifies a scoped package@version', () => {
    expect(classifyInline('@nx/angular@23.2.1')).toBe('package');
  });
  it('classifies a plain package@version', () => {
    expect(classifyInline('react@18')).toBe('package');
  });

  // --- json ---------------------------------------------------------
  it('classifies a JSON object literal', () => {
    expect(classifyInline('{"status":"ok"}')).toBe('json');
  });
  it('classifies a JSON array literal', () => {
    expect(classifyInline('[1,2,3]')).toBe('json');
  });

  // --- identifier -----------------------------------------------------
  it('classifies a camelCase identifier', () => {
    expect(classifyInline('emitDeclarationOnly')).toBe('identifier');
  });
  it('classifies a snake_case identifier', () => {
    expect(classifyInline('snake_case')).toBe('identifier');
  });
  it('classifies a function call', () => {
    expect(classifyInline('fn()')).toBe('identifier');
  });
  it('classifies a dotted member access', () => {
    expect(classifyInline('foo.bar')).toBe('identifier');
  });

  // --- number/port ----------------------------------------------------
  it('classifies a bare port number', () => {
    expect(classifyInline('3333')).toBe('number');
  });
  it('classifies another bare port number', () => {
    expect(classifyInline('4200')).toBe('number');
  });

  // --- false-positive guards -----------------------------------------
  it('leaves ordinary prose as the plain code fallback', () => {
    expect(classifyInline('this is just code')).toBe('code');
  });
  it('leaves a single lowercase word as the plain code fallback', () => {
    expect(classifyInline('hello')).toBe('code');
  });
  it('does not classify a single known command word with no arguments as shell', () => {
    expect(classifyInline('cargo')).toBe('code');
  });
  it('does not classify a single known command word "git" alone as shell', () => {
    expect(classifyInline('git')).toBe('code');
  });
  it('does not classify a boolean-expression && as shell without another shell tell', () => {
    expect(classifyInline('isValid && isReady')).toBe('code');
  });
  it('does not classify a capitalized prose word as an env var (no underscore)', () => {
    expect(classifyInline('TODO')).toBe('code');
  });
  it('does not classify a capitalized single word as an identifier', () => {
    expect(classifyInline('React')).toBe('code');
  });
  it('does not classify "e.g." as a path', () => {
    expect(classifyInline('e.g.')).toBe('code');
  });
  it('does not classify "i.e." as a path', () => {
    expect(classifyInline('i.e.')).toBe('code');
  });
  it('does not classify a bare version-looking number as a path', () => {
    expect(classifyInline('23.2.1')).toBe('code');
  });
  it('does not classify escaped HTML tag text as anything but code', () => {
    expect(classifyInline('&lt;main&gt;')).toBe('code');
  });
  it('does not classify an empty string', () => {
    expect(classifyInline('   ')).toBe('code');
  });
  it('is insensitive to whether the input is pre- or post-HTML-escape', () => {
    expect(classifyInline('set "X=1" && npm run build')).toBe('shell');
    expect(classifyInline('set &quot;X=1&quot; &amp;&amp; npm run build')).toBe('shell');
  });
});

describe('renderClassifiedInlineCode', () => {
  it('wraps a relative path as a clickable preview link when pathLinkClass is given', () => {
    const html = renderClassifiedInlineCode('apps/api', { pathLinkClass: 'preview-link' });
    expect(html).toBe('<a href="apps/api" class="preview-link ic ic-path">apps/api</a>');
  });

  it('does not make a Windows absolute path clickable', () => {
    const html = renderClassifiedInlineCode('C:\\projects\\bebok', { pathLinkClass: 'preview-link' });
    expect(html).toBe('<code class="ic ic-path">C:\\projects\\bebok</code>');
  });

  it('renders a plain path chip when no pathLinkClass is configured', () => {
    const html = renderClassifiedInlineCode('apps/api');
    expect(html).toBe('<code class="ic ic-path">apps/api</code>');
  });

  it('opens an http(s) URL as a real external link', () => {
    const html = renderClassifiedInlineCode('http://127.0.0.1:8787');
    expect(html).toBe('<a href="http://127.0.0.1:8787" target="_blank" rel="noreferrer" class="ic ic-url">http://127.0.0.1:8787</a>');
  });

  it('renders a bare route as plain colored text, not a link', () => {
    const html = renderClassifiedInlineCode('/api/health');
    expect(html).toBe('<code class="ic ic-route">/api/health</code>');
    expect(html).not.toContain('<a ');
  });

  it('renders the fallback chip for unclassified content', () => {
    const html = renderClassifiedInlineCode('&lt;main&gt;');
    expect(html).toBe('<code class="ic">&lt;main&gt;</code>');
  });
});

describe('highlightInlineJson', () => {
  it('colors keys, string values, numbers and booleans', () => {
    const html = highlightInlineJson('{&quot;status&quot;:&quot;ok&quot;,&quot;n&quot;:3,&quot;ok&quot;:true}');
    expect(html).toContain('<span class="ic-jt-key">&quot;status&quot;</span>');
    expect(html).toContain('<span class="ic-jt-string">&quot;ok&quot;</span>');
    expect(html).toContain('<span class="ic-jt-key">&quot;n&quot;</span>');
    expect(html).toContain('<span class="ic-jt-num">3</span>');
    expect(html).toContain('<span class="ic-jt-key">&quot;ok&quot;</span>');
    expect(html).toContain('<span class="ic-jt-bool">true</span>');
  });

  it('never reintroduces a raw quote for an already-escaped string value', () => {
    const html = highlightInlineJson('{&quot;a&quot;:&quot;b&quot;}');
    expect(html).toContain('&quot;a&quot;');
    expect(html).toContain('&quot;b&quot;');
    // The escaped string bodies themselves never regain a literal quote -
    // only the span-tag markup this function adds uses `"` (for its own
    // `class="..."` attributes).
    expect(html.replace(/<span class="ic-jt-\w+">|<\/span>/g, '')).not.toContain('"');
  });
});
