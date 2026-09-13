/**
 * F9-11: bare `http(s)://` URL linkification, shared by the chat markdown
 * renderer (`core/markdown.ts`), the Preview panel renderer
 * (`ui/markdown-view/markdown-view.render.ts`) and the tool-output rendering
 * helper (`ui/diff-view/diff-view.ts`) - anywhere assistant/user text or a
 * tool's result can mention a plain `https://…` in prose, not already
 * wrapped in `[label](url)` markdown syntax or a backtick code span (which,
 * for a bare URL, `core/inline-classify.ts` already turns into its own `<a
 * class="ic ic-url">` chip - that anchor is the "linkified" form there and
 * must never be wrapped again).
 *
 * Safety contract: every function below takes text that is already
 * HTML-escaped (the `escapeHtml` used by each of those call sites turns `&`,
 * `<`, `>`, `"` and `'` into entities first). This module only ever wraps an
 * already-safe substring in a new `<a href="…">…</a>` - it never introduces
 * an unescaped character, and the matcher only ever recognizes a literal
 * `http://`/`https://` prefix, so a `javascript:`/`data:`/etc. URI can never
 * be produced by the bare-URL path regardless of what the source text
 * contains. `renderSchemeLink` below additionally guards the *markdown*
 * `[label](url)` link path (which does accept an arbitrary URI scheme) by
 * refusing to ever emit a `javascript:` href.
 */

/** `target="_blank"` always pairs with this - `noopener` so the new tab
 *  can't reach back via `window.opener`, `noreferrer` so it also drops the
 *  `Referer` header (matches the existing convention in
 *  `ui/right-drawer/panels/browser-panel.ts`). */
export const LINK_REL = 'noopener noreferrer';

/**
 * A run of already-escaped, URL-safe characters: anything but whitespace,
 * with one exception - a literal `&` is only consumed when it opens the
 * `&amp;` entity (a real ampersand in a query string, e.g. `?a=1&amp;b=2`).
 * Any other entity that can appear in escaped text - `&lt;`, `&gt;`,
 * `&quot;`, `&#39;` - always marks a boundary a bare URL must stop before,
 * since none of those ever legitimately continue a URL mentioned in prose
 * (they stand in for a literal `<`, `>`, `"` or `'`).
 */
const BARE_URL_RE = /\bhttps?:\/\/(?:[^\s&]|&(?=amp;))+/gi;

/**
 * Trim trailing punctuation off a matched URL - e.g. "see http://x.com."
 * keeps the period out of the link, "list: http://x.com," keeps the comma
 * out, "clause; http://x.com;" keeps the semicolon out. A trailing `)` is
 * kept when the match itself contains more `(` than `)` (a Wikipedia-style
 * `.../wiki/Foo_(bar)` URL), so only a sentence's own wrapping paren gets
 * stripped and a paren that is genuinely part of the URL path survives.
 */
function trimTrailingPunctuation(url: string): string {
  let end = url.length;
  while (end > 0) {
    const ch = url[end - 1];
    if (ch === ')') {
      const slice = url.slice(0, end);
      const opens = (slice.match(/\(/g) ?? []).length;
      const closes = (slice.match(/\)/g) ?? []).length;
      if (closes > opens) {
        end -= 1;
        continue;
      }
      break;
    }
    if (ch === '.' || ch === ',' || ch === ';') {
      end -= 1;
      continue;
    }
    break;
  }
  return url.slice(0, end);
}

/** Replace bare URLs in a plain run of already-escaped text with no markup
 *  of its own. Callers touching a larger HTML string with existing tags
 *  should use `linkifyOutsideTags` instead. */
function linkifyRun(text: string): string {
  return text.replace(BARE_URL_RE, (match) => {
    const url = trimTrailingPunctuation(match);
    if (!url) {
      return match;
    }
    const trailing = match.slice(url.length);
    return `<a href="${url}" target="_blank" rel="${LINK_REL}">${url}</a>${trailing}`;
  });
}

/**
 * Run bare-URL linkification over `html`, skipping the text already inside
 * any element whose tag name is in `skipTags` (default `a`/`code`). Skipping
 * `<a>` keeps this from double-wrapping a URL already turned into a link by
 * markdown-link syntax or the `url` inline-code chip; skipping `<code>`
 * keeps it out of an inline code span that should stay plain text (a fenced
 * code block never reaches this function - it is rendered by a separate,
 * untouched path in every caller).
 */
export function linkifyOutsideTags(html: string, skipTags: readonly string[] = ['a', 'code']): string {
  const skip = new Set(skipTags.map((tag) => tag.toLowerCase()));
  const segments = html.split(/(<[^>]+>)/g);
  let depth = 0;
  return segments
    .map((segment) => {
      if (segment.startsWith('<')) {
        const match = /^<\/?([a-zA-Z][\w-]*)/.exec(segment);
        const tag = match?.[1]?.toLowerCase();
        if (tag && skip.has(tag)) {
          if (segment.startsWith('</')) {
            depth = Math.max(0, depth - 1);
          } else if (!/\/>\s*$/.test(segment)) {
            depth += 1;
          }
        }
        return segment;
      }
      return depth > 0 ? segment : linkifyRun(segment);
    })
    .join('');
}

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/**
 * Escape + linkify a plain-text (non-markdown) tool result, e.g. a failed
 * tool call's error message shown verbatim (`views/chat/parts/tool-part.ts`)
 * - "linkify only, no markdown" per F9-11.
 */
export function escapeAndLinkify(value: string): string {
  return linkifyRun(escapeHtml(value));
}

const RAW_BARE_URL_RE = /\bhttps?:\/\/[^\s<>"']+/gi;

/** Opaque placeholder delimiters for `protectUrls`: two characters from the
 *  Unicode Private Use Area, which no highlight.js grammar assigns any
 *  syntax meaning to and which `escapeHtml` never touches - so a URL
 *  wrapped in these markers survives a highlighter's tokenizing untouched
 *  and unsplit. */
const PLACEHOLDER_OPEN = '';
const PLACEHOLDER_CLOSE = '';
// The body is a letter followed by digits (`L0`, `L1`, ...), never a bare
// number: right after `PLACEHOLDER_OPEN` a highlighter's tokenizer sees a
// word-boundary (the private-use char is not a `\w` character), and a
// numeric-literal rule anchored to that boundary would otherwise still be
// able to match a bare digit run and wrap just the digits in their own span,
// breaking this exact-match restore regex.
const PLACEHOLDER_RE = new RegExp(`${PLACEHOLDER_OPEN}L(\\d+)${PLACEHOLDER_CLOSE}`, 'g');

export interface ProtectedUrls {
  /** `rawText` with every bare URL replaced by an opaque placeholder token. */
  text: string;
  /** Replace each placeholder in already-highlighted/escaped HTML with the
   *  real `<a target="_blank" rel="noopener noreferrer">` link. Call this
   *  on the highlighter's own output, after highlighting `text` above. */
  restore: (html: string) => string;
}

/**
 * F9-11: protect every bare `http(s)://` URL in raw, not-yet-escaped text
 * before it goes through a syntax highlighter (`ui/code-highlight`). Several
 * highlight.js grammars (JS, TS, C, C++, C#, Java, Kotlin, Go, Rust) treat a
 * bare `//` as the start of a line comment; fed a literal URL, that rule
 * matches *inside* it (right after the scheme's `:`), splitting `https:` and
 * `//example.com/...` into two separate text nodes/spans and defeating
 * `linkifyOutsideTags`'s single-text-run matching (it would see neither half
 * as a full `http(s)://…` token). Swapping the URL out for an inert
 * placeholder before highlighting - and back for a real link after - keeps
 * the URL intact regardless of what the highlighter (or its language
 * auto-detection) decides to do with the surrounding text. The one caller
 * that both highlights and linkifies the same text is `diff-view.ts`'s
 * non-diff "plain" fallback block.
 */
export function protectUrls(rawText: string): ProtectedUrls {
  const urls: string[] = [];
  const text = rawText.replace(RAW_BARE_URL_RE, (match) => {
    const url = trimTrailingPunctuation(match);
    if (!url) {
      return match;
    }
    const trailing = match.slice(url.length);
    const index = urls.push(url) - 1;
    return `${PLACEHOLDER_OPEN}L${index}${PLACEHOLDER_CLOSE}${trailing}`;
  });
  return {
    text,
    restore: (html: string): string =>
      html.replace(PLACEHOLDER_RE, (_, idx: string) => {
        const url = urls[Number(idx)];
        if (url === undefined) {
          return _;
        }
        const escaped = escapeHtml(url);
        return `<a href="${escaped}" target="_blank" rel="${LINK_REL}">${escaped}</a>`;
      }),
  };
}

/** True for an `href` whose scheme would execute script or otherwise act as
 *  more than a navigation target if a browser followed it - only ever
 *  reached for an `href` that already has a URI scheme (`hasUriScheme`), so
 *  a scheme-less relative path is never affected. */
function isDangerousHref(href: string): boolean {
  return /^\s*javascript\s*:/i.test(href);
}

/**
 * Render one markdown `[label](href)` link once its URI-scheme-ness is
 * known: a scheme-less `href` becomes a `relativeClass`-marked link (for the
 * host component's own click interception - `preview-link` in
 * `core/markdown.ts`, `relative-link` in `markdown-view.render.ts`); a
 * `javascript:` `href` is refused outright and rendered as plain label text
 * (defense in depth against a model/tool trying to smuggle a
 * click-to-execute link past the escape-then-markup pipeline); anything else
 * with a scheme opens in a new tab.
 */
export function renderSchemeLink(label: string, href: string, relativeClass: string, hasScheme: boolean): string {
  if (!hasScheme) {
    return `<a href="${href}" class="${relativeClass}">${label}</a>`;
  }
  if (isDangerousHref(href)) {
    return label;
  }
  return `<a href="${href}" target="_blank" rel="${LINK_REL}">${label}</a>`;
}
