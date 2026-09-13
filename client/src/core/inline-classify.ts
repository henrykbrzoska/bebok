/**
 * F8-3: classifies a chat inline-code span (the content between a single
 * pair of backticks) so the renderer can style it distinctly instead of
 * every span looking like the same gray chip - a file path, a shell
 * command and a raw port number all read very differently once you know
 * which one you're looking at.
 *
 * `classifyInline` is pure and synchronous (no DOM, no async) and accepts
 * either the raw backtick content or the already-HTML-escaped version of
 * it: none of the patterns recognized here hinge on `<`, `>`, `&`, `"` or
 * `'`, and `decodeBasicEntities` restores those five characters internally
 * before any pattern match runs, so classification is identical either
 * way. The render helpers below (`renderClassifiedInlineCode` and
 * friends), by contrast, always take the ESCAPED content, because their
 * output is spliced directly into an `[innerHTML]`-bound string - they
 * only ever wrap/split that string, never re-derive it, so nothing
 * unescaped is ever introduced.
 *
 * This module only changes *inline* code spans. Fenced code blocks are a
 * separate render path (`ui/code-highlight`) and are not touched here.
 */

import { LINK_REL } from './linkify';

export type InlineKind =
  | 'path'
  | 'url'
  | 'route'
  | 'http'
  | 'shell'
  | 'env'
  | 'package'
  | 'json'
  | 'identifier'
  | 'number'
  | 'code';

const HTTP_METHODS = ['GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'HEAD', 'OPTIONS'] as const;
const HTTP_ROUTE_RE = new RegExp(`^(${HTTP_METHODS.join('|')})\\s+(\\S.*)$`);

const ENV_RE = /^[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+$/;

// `name@version`, optionally scoped (`@scope/name@version`). The version
// half deliberately allows the range/qualifier characters npm accepts
// (`^`, `~`, `>=`, ...) as well as plain semver.
const PACKAGE_RE = /^(@?[\w.-]+(?:\/[\w.-]+)?)@([\w.^~+><=-]+)$/;

const URL_RE = /^[a-zA-Z][a-zA-Z0-9+.-]*:\/\/\S+$/;

// A route never has a second leading slash (that would be a scheme-relative
// URL) and is judged by its last path segment: `/api/health` (no dot) is a
// route, `/report.pdf`-style segments fall through to "path" instead - see
// `classifyInline`.
const ROUTE_RE = /^\/(?!\/)[\w\-./:]*$/;

const WIN_PATH_RE = /^[A-Za-z]:[\\/]/;
const BACKSLASH_RE = /\\/;

// A conservative allowlist: only these "look like a real file extension"
// for the purposes of the bare-filename case (`package.json`, `README.md`).
// A slash-delimited path (`apps/api`) never needs this - the slash alone is
// signal enough.
const KNOWN_EXTS = new Set([
  'ts', 'tsx', 'js', 'jsx', 'mjs', 'cjs', 'json', 'md', 'markdown',
  'rs', 'py', 'go', 'java', 'kt', 'kts', 'c', 'h', 'cpp', 'hpp', 'cs',
  'sh', 'bash', 'zsh', 'ps1', 'psm1', 'yml', 'yaml', 'toml', 'css', 'scss',
  'less', 'html', 'htm', 'txt', 'lock', 'env', 'ini', 'xml', 'sql',
  'png', 'jpg', 'jpeg', 'svg', 'gif', 'conf', 'config', 'gitignore',
]);

const SLASH_PATH_RE = /^\.{0,2}\/?(?:[\w.-]+\/)+[\w.-]+$/;
const BARE_FILE_RE = /^[\w-]+\.([A-Za-z0-9]{1,10})$/;

const NUMBER_RE = /^\d+$/;

// A dotted chain of identifier segments, optionally called: `foo`, `foo.bar`,
// `fn()`, `foo.bar()`.
const IDENTIFIER_CHAIN_RE = /^[A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*(?:\(\))?$/;

/** Windows cmd.exe-style builtins get a `>` glyph instead of `$`. */
const WINDOWS_SHELL_CMDS = new Set(['set', 'dir', 'cls', 'copy', 'del', 'rd', 'ren', 'findstr', 'tasklist', 'taskkill']);

const SHELL_KNOWN_CMDS = new Set([
  'npm', 'npx', 'yarn', 'pnpm', 'cargo', 'git', 'cd', 'export', 'docker',
  'node', 'python', 'python3', 'pip', 'pip3', 'brew', 'curl', 'wget',
  'sudo', 'rm', 'mkdir', 'ls', 'cat', 'mv', 'cp', 'chmod', 'chown', 'ssh',
  'scp', 'make', 'go', 'dotnet', 'mvn', 'gradle', 'kubectl', 'ng', 'tsc',
  'vite', 'webpack', ...WINDOWS_SHELL_CMDS,
]);

/** Restore the 5 characters `escapeHtml` (in `core/markdown.ts` and
 *  `markdown-view.render.ts`) turns into entities, so pattern-matching sees
 *  the original text regardless of whether it was called before or after
 *  escaping. Never used to build output - only to decide `InlineKind`. */
function decodeBasicEntities(value: string): string {
  return value
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, '&');
}

function stripQuotes(token: string): string {
  return token.replace(/^["']|["']$/g, '');
}

/**
 * A conservative shell-command heuristic. A bare single word is never shell
 * (prose like `` `cargo` `` on its own should stay plain code); beyond that,
 * either the command starts with a recognized program/builtin, opens with a
 * `$ `/`> ` prompt glyph, or shows at least two independent shell "tells"
 * together (a long flag plus a pipe/`&&` chain, or an `ENV=value` assignment
 * plus a pipe/`&&` chain) - a lone `&&`/`|` is not enough by itself, since
 * that also shows up in ordinary boolean-expression prose like
 * `` `isValid && isReady` ``.
 */
function looksLikeShell(decoded: string): boolean {
  const trimmed = decoded.trim();
  if (!/\s/.test(trimmed)) {
    return false;
  }
  if (/^[$>]\s+\S/.test(trimmed)) {
    return true;
  }
  const tokens = trimmed.split(/\s+/);
  const first = stripQuotes(tokens[0] ?? '').toLowerCase();
  if (SHELL_KNOWN_CMDS.has(first)) {
    return true;
  }
  const hasKnownCmdAnywhere = tokens.some((t) => SHELL_KNOWN_CMDS.has(stripQuotes(t).toLowerCase()));
  const hasFlag = /(^|\s)--[a-zA-Z][\w-]*/.test(trimmed);
  const hasPipeChain = /\s(&&|\|\|)\s/.test(trimmed) || /\s\|\s/.test(trimmed);
  const hasEnvAssignment = /\b[A-Z][A-Z0-9_]*=\S/.test(trimmed);
  if (hasKnownCmdAnywhere && (hasFlag || hasPipeChain || hasEnvAssignment)) {
    return true;
  }
  if (hasFlag && hasPipeChain) {
    return true;
  }
  if (hasEnvAssignment && hasPipeChain) {
    return true;
  }
  return false;
}

function looksLikeIdentifier(decoded: string): boolean {
  if (!IDENTIFIER_CHAIN_RE.test(decoded)) {
    return false;
  }
  const hasUnderscore = decoded.includes('_');
  const hasCamel = /[a-z][A-Z]/.test(decoded);
  const hasCall = decoded.endsWith('()');
  const hasDotChain = decoded.includes('.');
  return hasUnderscore || hasCamel || hasCall || hasDotChain;
}

/** True for `{"...":...}` / `[...]` object-or-array-literal-shaped text. */
function looksLikeJson(decoded: string): boolean {
  if (decoded.length < 2) {
    return false;
  }
  return (
    (decoded.startsWith('{') && decoded.endsWith('}')) ||
    (decoded.startsWith('[') && decoded.endsWith(']'))
  );
}

function lastSegmentHasKnownExtension(pathLike: string): boolean {
  const lastSegment = pathLike.split(/[\\/]/).pop() ?? '';
  const dot = lastSegment.lastIndexOf('.');
  if (dot <= 0 || dot === lastSegment.length - 1) {
    return false;
  }
  return KNOWN_EXTS.has(lastSegment.slice(dot + 1).toLowerCase());
}

/** Classify one inline-code span's content. See module header for the
 *  raw-vs-escaped-input contract. */
export function classifyInline(text: string): InlineKind {
  const trimmed = text.trim();
  if (trimmed.length === 0) {
    return 'code';
  }
  const decoded = decodeBasicEntities(trimmed);

  if (looksLikeJson(decoded)) {
    return 'json';
  }
  if (HTTP_ROUTE_RE.test(decoded)) {
    return 'http';
  }
  if (looksLikeShell(decoded)) {
    return 'shell';
  }
  if (PACKAGE_RE.test(decoded)) {
    return 'package';
  }
  if (ENV_RE.test(decoded)) {
    return 'env';
  }
  if (URL_RE.test(decoded)) {
    return 'url';
  }
  if (ROUTE_RE.test(decoded) && !lastSegmentHasKnownExtension(decoded)) {
    return 'route';
  }
  if (WIN_PATH_RE.test(decoded) || BACKSLASH_RE.test(decoded)) {
    return 'path';
  }
  if (SLASH_PATH_RE.test(decoded)) {
    return 'path';
  }
  if (BARE_FILE_RE.test(decoded) && lastSegmentHasKnownExtension(decoded)) {
    return 'path';
  }
  if (NUMBER_RE.test(decoded)) {
    return 'number';
  }
  if (looksLikeIdentifier(decoded)) {
    return 'identifier';
  }
  return 'code';
}

/** True when a `path`-classified span is a relative filesystem path (as
 *  opposed to a Windows absolute path), i.e. safe to resolve against the
 *  session's working directory the way `core/markdown.ts`'s existing
 *  bare-`.md`-path linkification already does. */
function isRelativePath(decoded: string): boolean {
  return !WIN_PATH_RE.test(decoded) && !BACKSLASH_RE.test(decoded);
}

function parseHttpRoute(escaped: string): { method: string; route: string } | null {
  const m = HTTP_ROUTE_RE.exec(escaped.trim());
  return m ? { method: m[1], route: m[2] } : null;
}

function parsePackage(escaped: string): { name: string; version: string } | null {
  const m = PACKAGE_RE.exec(escaped.trim());
  return m ? { name: m[1], version: m[2] } : null;
}

// Quoted-string / punctuation / literal / number tokens, matched against
// ALREADY-ESCAPED text (so string bodies are delimited by the `&quot;`
// entity, not a literal `"`).
const JSON_TOKEN_RE =
  /(&quot;(?:(?!&quot;)[\s\S])*&quot;)|([{}[\]:,])|(true|false|null)|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/g;

/** Tokenize an already-HTML-escaped `{"...":...}` / `[...]` span into
 *  colorable `<span class="ic-jt-*">` runs. Never throws - anything that
 *  doesn't match a token type (whitespace, stray text) passes through
 *  untouched, so a slightly malformed literal still renders, just with
 *  fewer tokens colored. */
export function highlightInlineJson(escaped: string): string {
  let out = '';
  let last = 0;
  JSON_TOKEN_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = JSON_TOKEN_RE.exec(escaped))) {
    out += escaped.slice(last, m.index);
    const [full, str, punct, boolOrNull, num] = m;
    if (str !== undefined) {
      const rest = escaped.slice(m.index + full.length);
      const isKey = /^\s*:/.test(rest);
      out += `<span class="ic-jt-${isKey ? 'key' : 'string'}">${full}</span>`;
    } else if (punct !== undefined) {
      out += `<span class="ic-jt-punct">${full}</span>`;
    } else if (boolOrNull !== undefined) {
      out += `<span class="ic-jt-bool">${full}</span>`;
    } else if (num !== undefined) {
      out += `<span class="ic-jt-num">${full}</span>`;
    }
    last = m.index + full.length;
  }
  out += escaped.slice(last);
  return out;
}

export interface ClassifiedInlineCodeOptions {
  /**
   * Class to add (alongside `ic ic-path`) when a `path`-classified span is
   * relative and should be wrapped as a clickable `<a>` into an
   * already-wired Preview/Explorer click handler, e.g. `preview-link`
   * (`core/markdown.ts` / `text-part.ts`). Omit to render every path as a
   * plain (non-clickable) chip.
   */
  pathLinkClass?: string;
}

/**
 * Build the full inline-code markup for one already-escaped backtick span:
 * classifies it, then wraps it in the right chip shape. This is the single
 * place that decides the markup, so `core/markdown.ts` never has to special
 * case a kind on its own.
 */
export function renderClassifiedInlineCode(escaped: string, options: ClassifiedInlineCodeOptions = {}): string {
  const kind = classifyInline(escaped);

  switch (kind) {
    case 'json':
      return `<code class="ic ic-json">${highlightInlineJson(escaped)}</code>`;

    case 'http': {
      const parsed = parseHttpRoute(escaped);
      if (!parsed) {
        break;
      }
      const methodClass = `ic-method-${parsed.method.toLowerCase()}`;
      return (
        `<code class="ic ic-http">` +
        `<span class="ic-method ${methodClass}">${parsed.method}</span> ${parsed.route}` +
        `</code>`
      );
    }

    case 'shell': {
      const decoded = decodeBasicEntities(escaped.trim());
      const firstWord = stripQuotes(decoded.trim().split(/\s+/)[0] ?? '').toLowerCase();
      const glyph = WINDOWS_SHELL_CMDS.has(firstWord) ? '&gt;' : '$';
      return `<code class="ic ic-shell"><span class="ic-shell-glyph">${glyph}</span>${escaped}</code>`;
    }

    case 'package': {
      const parsed = parsePackage(escaped);
      if (!parsed) {
        break;
      }
      return `<code class="ic ic-package">${parsed.name}<span class="ic-pkg-version">@${parsed.version}</span></code>`;
    }

    case 'url':
      return `<a href="${escaped}" target="_blank" rel="${LINK_REL}" class="ic ic-url">${escaped}</a>`;

    case 'route':
      return `<code class="ic ic-route">${escaped}</code>`;

    case 'env':
      return `<code class="ic ic-env">${escaped}</code>`;

    case 'number':
      return `<code class="ic ic-number">${escaped}</code>`;

    case 'identifier':
      return `<code class="ic ic-identifier">${escaped}</code>`;

    case 'path': {
      const decoded = decodeBasicEntities(escaped.trim());
      if (options.pathLinkClass && isRelativePath(decoded)) {
        return `<a href="${escaped}" class="${options.pathLinkClass} ic ic-path">${escaped}</a>`;
      }
      return `<code class="ic ic-path">${escaped}</code>`;
    }

    case 'code':
      break;
  }
  return `<code class="ic">${escaped}</code>`;
}
