/**
 * Shared syntax-highlighting core (F7-4).
 *
 * Wraps `highlight.js`'s tree-shakeable core build: the ~10 kB tokenizer
 * engine (`highlight.js/lib/core`) is imported eagerly, but every language
 * grammar is a separate `import()` registered on first use, so a session
 * that never shows Rust never pays for the Rust grammar. Grammars are
 * memoized (`registered`/`pending`) - loading the same language twice is a
 * no-op after the first call.
 *
 * Safety: `hljs.highlight()` treats its input as plain text and HTML-escapes
 * it while tokenizing (the only markup it ever emits is the `<span
 * class="hljs-...">` wrappers it authors itself), so feeding it raw,
 * untrusted model/file/diff text is safe. The "no grammar (yet)" fallback
 * path escapes manually with the same rules the rest of the app uses
 * (`core/markdown.ts`, `markdown-view.render.ts`). Every caller still binds
 * the result with Angular's own `[innerHTML]`, which sanitizes on top of
 * that; nothing here bypasses `DomSanitizer`.
 *
 * Reactivity: `highlightLanguagesVersion` is a plain module-level signal
 * bumped whenever a lazily-loaded grammar finishes registering. Any
 * `computed()` that calls into `highlightCode`/`highlightBlockHtml` (however
 * indirectly - e.g. through `renderMarkdown`) picks it up as a dependency
 * simply by being evaluated while it reads the signal, so a code block shown
 * before its grammar loaded re-renders, highlighted, once the grammar lands -
 * no manual wiring needed at each call site.
 */

import { signal } from '@angular/core';
import hljs from 'highlight.js/lib/core';
import type { LanguageFn } from 'highlight.js';

/** Files/snippets larger than this skip highlighting entirely (perf guard). */
const MAX_HIGHLIGHT_CHARS = 300 * 1024; // ~300 kB
/** `highlightAuto` is only attempted for a hint-less snippet this short. */
const AUTO_DETECT_MAX_CHARS = 400;
/** Minimum hljs relevance score to trust an auto-detected guess. */
const AUTO_DETECT_MIN_RELEVANCE = 5;
/** Seeded in the background the first time auto-detect is needed, so later
 *  hint-less snippets in the same session have something to guess against. */
const AUTO_DETECT_SEED = ['javascript', 'typescript', 'python', 'bash', 'json'] as const;

interface LanguageSpec {
  /** hljs registration id (also the `language` passed to `hljs.highlight`). */
  id: string;
  load: () => Promise<{ default: LanguageFn }>;
}

/**
 * The most popular languages (F7-4's list), keyed by a canonical short name.
 * `ALIASES` below maps fence hints and file extensions onto these keys.
 */
const LANGUAGES: Record<string, LanguageSpec> = {
  typescript: { id: 'typescript', load: () => import('highlight.js/lib/languages/typescript') },
  javascript: { id: 'javascript', load: () => import('highlight.js/lib/languages/javascript') },
  xml: { id: 'xml', load: () => import('highlight.js/lib/languages/xml') },
  css: { id: 'css', load: () => import('highlight.js/lib/languages/css') },
  scss: { id: 'scss', load: () => import('highlight.js/lib/languages/scss') },
  json: { id: 'json', load: () => import('highlight.js/lib/languages/json') },
  yaml: { id: 'yaml', load: () => import('highlight.js/lib/languages/yaml') },
  ini: { id: 'ini', load: () => import('highlight.js/lib/languages/ini') },
  markdown: { id: 'markdown', load: () => import('highlight.js/lib/languages/markdown') },
  rust: { id: 'rust', load: () => import('highlight.js/lib/languages/rust') },
  python: { id: 'python', load: () => import('highlight.js/lib/languages/python') },
  go: { id: 'go', load: () => import('highlight.js/lib/languages/go') },
  java: { id: 'java', load: () => import('highlight.js/lib/languages/java') },
  kotlin: { id: 'kotlin', load: () => import('highlight.js/lib/languages/kotlin') },
  c: { id: 'c', load: () => import('highlight.js/lib/languages/c') },
  cpp: { id: 'cpp', load: () => import('highlight.js/lib/languages/cpp') },
  csharp: { id: 'csharp', load: () => import('highlight.js/lib/languages/csharp') },
  bash: { id: 'bash', load: () => import('highlight.js/lib/languages/bash') },
  powershell: { id: 'powershell', load: () => import('highlight.js/lib/languages/powershell') },
  sql: { id: 'sql', load: () => import('highlight.js/lib/languages/sql') },
  dockerfile: { id: 'dockerfile', load: () => import('highlight.js/lib/languages/dockerfile') },
};

/** Fence hint / file extension (lowercased, no leading dot) -> `LANGUAGES` key. */
const ALIASES: Record<string, string> = {
  ts: 'typescript',
  tsx: 'typescript',
  mts: 'typescript',
  cts: 'typescript',
  js: 'javascript',
  jsx: 'javascript',
  mjs: 'javascript',
  cjs: 'javascript',
  node: 'javascript',
  html: 'xml',
  htm: 'xml',
  xhtml: 'xml',
  xml: 'xml',
  svg: 'xml',
  vue: 'xml',
  css: 'css',
  scss: 'scss',
  sass: 'scss',
  json: 'json',
  jsonc: 'json',
  json5: 'json',
  yaml: 'yaml',
  yml: 'yaml',
  toml: 'ini',
  ini: 'ini',
  cfg: 'ini',
  md: 'markdown',
  markdown: 'markdown',
  rs: 'rust',
  rust: 'rust',
  py: 'python',
  py3: 'python',
  python: 'python',
  go: 'go',
  golang: 'go',
  java: 'java',
  kt: 'kotlin',
  kts: 'kotlin',
  kotlin: 'kotlin',
  c: 'c',
  h: 'c',
  cpp: 'cpp',
  cc: 'cpp',
  cxx: 'cpp',
  'c++': 'cpp',
  hpp: 'cpp',
  hh: 'cpp',
  hxx: 'cpp',
  cs: 'csharp',
  csharp: 'csharp',
  sh: 'bash',
  bash: 'bash',
  zsh: 'bash',
  shell: 'bash',
  ps1: 'powershell',
  psm1: 'powershell',
  psd1: 'powershell',
  powershell: 'powershell',
  sql: 'sql',
  dockerfile: 'dockerfile',
  docker: 'dockerfile',
  containerfile: 'dockerfile',
};

const registered = new Set<string>();
const pending = new Map<string, Promise<void>>();
let autoDetectSeeded = false;

/**
 * Bumped after every lazily-loaded grammar registers. A pure module-level
 * Angular signal - reading it inside any `computed()`/`effect()` (even
 * transitively, through `highlightCode`) makes that computation re-run once
 * a grammar this call needed finishes loading.
 */
export const highlightLanguagesVersion = signal(0);

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/** File extension (no dot, lowercased), with `Dockerfile`-style names handled. */
export function extensionOf(filename: string | null | undefined): string | null {
  if (!filename) {
    return null;
  }
  const base = filename.split(/[\\/]/).pop() ?? filename;
  if (/^dockerfile(\.[^.]+)?$/i.test(base)) {
    return 'dockerfile';
  }
  const match = /\.([a-z0-9+]+)$/i.exec(base);
  return match ? match[1].toLowerCase() : null;
}

function toLanguageKey(hint: string | null | undefined): string | null {
  if (!hint) {
    return null;
  }
  const key = hint.trim().toLowerCase().replace(/^\./, '');
  if (!key) {
    return null;
  }
  return ALIASES[key] ?? (LANGUAGES[key] ? key : null);
}

/** Resolve a fence hint / language name and/or filename to a supported language key. */
export function resolveLanguage(
  hint?: string | null,
  filename?: string | null,
): string | null {
  return toLanguageKey(hint) ?? toLanguageKey(extensionOf(filename ?? null));
}

/**
 * Kick off (idempotently) loading + registering one grammar and return a
 * promise that resolves once it is ready (or immediately if it already is).
 * The highlighting path itself never awaits this - it stays synchronous and
 * fire-and-forget, relying on `highlightLanguagesVersion` for reactivity -
 * but `preloadLanguage` below exposes it for callers (mainly specs) that
 * want a deterministic "after the grammar loaded" assertion.
 */
function ensureLanguage(key: string): Promise<void> {
  const spec = LANGUAGES[key];
  if (!spec || registered.has(spec.id)) {
    return Promise.resolve();
  }
  const existing = pending.get(spec.id);
  if (existing) {
    return existing;
  }
  const promise = spec
    .load()
    .then((mod) => {
      if (!registered.has(spec.id)) {
        hljs.registerLanguage(spec.id, mod.default);
        registered.add(spec.id);
        highlightLanguagesVersion.update((v) => v + 1);
      }
    })
    .catch(() => {
      // Grammar failed to load (offline chunk fetch, etc.) - leave the
      // fallback (escaped, unhighlighted) text in place.
    })
    .finally(() => {
      pending.delete(spec.id);
    });
  pending.set(spec.id, promise);
  return promise;
}

/**
 * Resolve `hint`/`filename` to a language and wait for its grammar to be
 * loaded and registered (a no-op resolved promise when neither names a
 * supported language). Exported for tests; the runtime highlighting path
 * does not use it.
 */
export function preloadLanguage(
  hint?: string | null,
  filename?: string | null,
): Promise<void> {
  const key = resolveLanguage(hint, filename);
  return key ? ensureLanguage(key) : Promise.resolve();
}

/** Seed a handful of common grammars in the background for `highlightAuto`. */
function seedAutoDetect(): void {
  if (autoDetectSeeded) {
    return;
  }
  autoDetectSeeded = true;
  for (const key of AUTO_DETECT_SEED) {
    void ensureLanguage(key);
  }
}

export interface HighlightOptions {
  /** Fenced-code-block hint or explicit language name (e.g. `ts`, `python`). */
  language?: string | null;
  /** File name/path, used to guess a language when `language` is absent. */
  filename?: string | null;
  /** Allow `highlightAuto` for a short, hint-less snippet. Default `true`. */
  autoDetect?: boolean;
}

export interface HighlightResult {
  /** Token-highlighted, HTML-escaped markup (no wrapping `<pre>`/`<code>`). */
  html: string;
  /** Resolved hljs language id, or `null` when nothing was highlighted. */
  language: string | null;
}

/**
 * Highlight `code` per `options`. Always returns *some* safe HTML (escaped
 * plain text at minimum) - callers never need a separate escape step.
 *
 * Language resolution order: explicit `language` hint, then `filename`
 * extension, then (for a short snippet with neither) a best-effort
 * `highlightAuto` guess. A large file (> 300 kB) is never highlighted, only
 * escaped, to keep big Explorer files fast.
 */
export function highlightCode(code: string, options: HighlightOptions = {}): HighlightResult {
  // Read the version signal unconditionally so a `computed()` around this
  // call is invalidated when any grammar it might need finishes loading.
  highlightLanguagesVersion();

  if (code.length > MAX_HIGHLIGHT_CHARS) {
    return { html: escapeHtml(code), language: null };
  }

  const key = resolveLanguage(options.language, options.filename);
  if (key) {
    const spec = LANGUAGES[key];
    if (registered.has(spec.id)) {
      try {
        const result = hljs.highlight(code, { language: spec.id, ignoreIllegals: true });
        return { html: result.value, language: spec.id };
      } catch {
        return { html: escapeHtml(code), language: null };
      }
    }
    void ensureLanguage(key);
    return { html: escapeHtml(code), language: null };
  }

  if ((options.autoDetect ?? true) && code.length > 0 && code.length <= AUTO_DETECT_MAX_CHARS) {
    seedAutoDetect();
    if (registered.size > 0) {
      try {
        const guess = hljs.highlightAuto(code, [...registered]);
        if (guess.language && (guess.relevance ?? 0) >= AUTO_DETECT_MIN_RELEVANCE) {
          return { html: guess.value ?? escapeHtml(code), language: guess.language };
        }
      } catch {
        // fall through to plain text
      }
    }
  }

  return { html: escapeHtml(code), language: null };
}

/** `highlightCode`, wrapped in `<pre><code class="hljs language-...">`. */
export function highlightBlockHtml(code: string, options: HighlightOptions = {}): string {
  const { html, language } = highlightCode(code, options);
  const cls = language ? `hljs language-${language}` : 'hljs';
  return `<pre><code class="${cls}">${html}</code></pre>`;
}
