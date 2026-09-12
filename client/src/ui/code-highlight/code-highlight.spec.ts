/**
 * F7-4: the shared syntax-highlighting core. Covers language resolution
 * (fence hint / extension / `Dockerfile`-style filename), the lazy-load ->
 * `highlightLanguagesVersion` bump -> re-highlight cycle, the large-file
 * skip guard, and that untrusted code is always HTML-escaped one way or
 * another (tokenized or not).
 */

import {
  extensionOf,
  highlightCode,
  preloadLanguage,
  resolveLanguage,
} from './code-highlight';

describe('resolveLanguage / extensionOf (F7-4)', () => {
  it('maps common fence hints and aliases to a canonical hljs id', () => {
    expect(resolveLanguage('ts')).toBe('typescript');
    expect(resolveLanguage('tsx')).toBe('typescript');
    expect(resolveLanguage('js')).toBe('javascript');
    expect(resolveLanguage('jsx')).toBe('javascript');
    expect(resolveLanguage('py')).toBe('python');
    expect(resolveLanguage('sh')).toBe('bash');
    expect(resolveLanguage('ps1')).toBe('powershell');
    expect(resolveLanguage('yml')).toBe('yaml');
    expect(resolveLanguage('toml')).toBe('ini');
    expect(resolveLanguage('md')).toBe('markdown');
    expect(resolveLanguage('rs')).toBe('rust');
    expect(resolveLanguage('unknown-lang-xyz')).toBeNull();
  });

  it('resolves a language from a filename extension when no hint is given', () => {
    expect(resolveLanguage(null, 'src/main.rs')).toBe('rust');
    expect(resolveLanguage(null, 'app\\component.tsx')).toBe('typescript');
    expect(resolveLanguage(null, 'style.scss')).toBe('scss');
    expect(resolveLanguage(null, 'README')).toBeNull();
  });

  it('recognizes a bare `Dockerfile`-style filename with no extension', () => {
    expect(extensionOf('Dockerfile')).toBe('dockerfile');
    expect(extensionOf('Dockerfile.prod')).toBe('dockerfile');
    expect(resolveLanguage(null, 'Dockerfile')).toBe('dockerfile');
  });

  it('prefers an explicit hint over the filename', () => {
    expect(resolveLanguage('python', 'main.rs')).toBe('python');
  });
});

describe('highlightCode (F7-4)', () => {
  it('escapes plain text when no language is known', () => {
    const { html, language } = highlightCode('<b>hi</b>', { language: 'not-a-real-lang' });
    expect(html).toBe('&lt;b&gt;hi&lt;/b&gt;');
    expect(language).toBeNull();
  });

  it('skips highlighting for a file over the size guard, but still escapes', () => {
    const big = `<script>${'a'.repeat(300 * 1024 + 1)}</script>`;
    const { html, language } = highlightCode(big, { language: 'javascript' });
    expect(language).toBeNull();
    expect(html).not.toContain('<script>');
    expect(html).toContain('&lt;script&gt;');
  });

  it('tokenizes once the grammar is loaded and registered', async () => {
    await preloadLanguage('python');
    const { html, language } = highlightCode('def f():\n    return 1\n', { language: 'py' });
    expect(language).toBe('python');
    expect(html).toContain('hljs-keyword');
    // Still safe: no raw `<`/`>` outside of the spans hljs itself authors.
    expect(html).not.toMatch(/<(?!\/?span)/);
  });

  it('resolves by filename when no explicit language is given', async () => {
    await preloadLanguage(null, 'main.go');
    const { language } = highlightCode('func main() {}', { filename: 'main.go' });
    expect(language).toBe('go');
  });
});
