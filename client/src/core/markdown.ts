/**
 * Minimal, dependency-free markdown renderer for assistant text parts.
 *
 * The input is first HTML-escaped, then structural markup is applied from the
 * escaped text. Raw HTML is never passed through, so this stays safe to inject
 * with `[innerHTML]`. Scope is intentionally small (M3): paragraphs, fenced
 * code blocks, headings, inline code/bold/links and simple lists.
 *
 * F7-4: fenced code blocks are additionally run through
 * `ui/code-highlight` for syntax highlighting (language resolved from the
 * fence hint). That module escapes/tokenizes safely on its own, so this file
 * still never binds raw text.
 *
 * F8-3: inline (single-backtick) code spans are further classified by
 * `core/inline-classify.ts` so a file path, a shell command, an HTTP route,
 * etc. each get a distinct chip instead of one generic gray one. That
 * module only ever wraps the already-escaped span text, so it introduces
 * no new way for raw model text to reach the DOM.
 */

import { renderClassifiedInlineCode } from './inline-classify';
import { highlightBlockHtml } from '../ui/code-highlight/code-highlight';

interface Block {
  kind: 'code' | 'md';
  lang?: string;
  code?: string;
  text?: string;
}

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/** Tokenize into fenced code blocks and plain text blocks. */
function tokenize(text: string): Block[] {
  const lines = text.split('\n');
  const out: Block[] = [];
  let md: string[] = [];

  const flush = (): void => {
    if (md.length > 0) {
      out.push({ kind: 'md', text: md.join('\n') });
      md = [];
    }
  };

  let i = 0;
  while (i < lines.length) {
    const fence = /^```([\w+-]*)\s*$/.exec(lines[i]);
    if (fence) {
      flush();
      const lang = fence[1] ?? '';
      const code: string[] = [];
      i += 1;
      while (i < lines.length && !/^```\s*$/.test(lines[i])) {
        code.push(lines[i]);
        i += 1;
      }
      i += 1; // skip the closing fence
      out.push({ kind: 'code', lang, code: code.join('\n') });
    } else {
      md.push(lines[i]);
      i += 1;
    }
  }
  flush();
  return out;
}

/** True for an absolute URL (`http:`, `mailto:`, ...) - anything with a scheme. */
function hasUriScheme(href: string): boolean {
  return /^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(href);
}

/**
 * F6-11: a bare `docs/plan.md`-looking token in plain text (not already part
 * of `[label](url)` syntax) is linkified too, so a relative path an assistant
 * just mentions in prose is still clickable. Matched only outside any tag
 * already produced by `renderInline` (skipping `<a>…</a>` and `<code>…</code>`
 * spans) so an already-linked or code-quoted path is never double-wrapped.
 */
const BARE_MD_PATH = /(^|[\s([])((?:\.{1,2}\/)?[\w.-]+(?:\/[\w.-]+)*\.(?:md|markdown))\b/g;

function linkifyBarePaths(html: string): string {
  const segments = html.split(/(<[^>]+>)/g);
  let skipDepth = 0;
  return segments
    .map((segment) => {
      if (segment.startsWith('<')) {
        const lower = segment.toLowerCase();
        if (/^<(a|code)\b/.test(lower)) {
          skipDepth += 1;
        } else if (/^<\/(a|code)>/.test(lower)) {
          skipDepth = Math.max(0, skipDepth - 1);
        }
        return segment;
      }
      if (skipDepth > 0) {
        return segment;
      }
      return segment.replace(
        BARE_MD_PATH,
        (_, pre: string, path: string) => `${pre}<a href="${path}" class="preview-link">${path}</a>`,
      );
    })
    .join('');
}

function renderInline(value: string): string {
  // `value` is already HTML-escaped by `renderMarkdown` (see `escapeHtml`), so
  // model text such as `<main>` or `Array<string>` arrives as `&lt;main&gt;`
  // and only the markup produced here is real HTML.
  let html = value
    .replace(/`([^`\n]+)`/g, (_, code: string) => renderClassifiedInlineCode(code, { pathLinkClass: 'preview-link' }))
    .replace(/\*\*([^*]+)\*\*/g, (_, strong: string) => `<strong>${strong}</strong>`)
    .replace(/\[([^\]\n]+)\]\(([^)\s]+)\)/g, (_, label: string, href: string) =>
      hasUriScheme(href)
        ? `<a href="${href}" target="_blank" rel="noreferrer">${label}</a>`
        : `<a href="${href}" class="preview-link">${label}</a>`,
    );
  // F6-11: a plain-looking relative `.md` path gets the same treatment.
  html = linkifyBarePaths(html);
  // <br> inside block paragraphs from escaped newlines.
  html = html.replace(/\n/g, '<br>');
  return html;
}

/** ATX heading line: `#`..`######`, a space, then the text (`#hashtag` is not one). */
const HEADING = /^(#{1,6})\s+(.+)$/;

/**
 * Render a paragraph: heading lines become `<h2>`..`<h6>` (clamped to h2+ so
 * model output never competes with the view's own title); the runs of lines
 * between them go through `renderLines`.
 */
function renderParagraph(value: string): string {
  const out: string[] = [];
  let run: string[] = [];
  const flush = (): void => {
    if (run.length > 0) {
      out.push(renderLines(run));
      run = [];
    }
  };
  for (const line of value.split('\n')) {
    const heading = HEADING.exec(line);
    if (!heading) {
      run.push(line);
      continue;
    }
    flush();
    const level = Math.min(6, Math.max(2, heading[1].length));
    out.push(`<h${level}>${renderInline(heading[2].trim())}</h${level}>`);
  }
  flush();
  return out.join('');
}

/** Render a run of lines; simple all-bullet / all-numbered runs become lists. */
function renderLines(lines: string[]): string {
  const bullets = /^[-*+]\s+/;
  const numbers = /^\d+[.)]\s+/;
  if (lines.length > 0 && lines.every((l) => bullets.test(l))) {
    const items = lines
      .map((l) => `<li>${renderInline(l.replace(bullets, ''))}</li>`)
      .join('');
    return `<ul>${items}</ul>`;
  }
  if (lines.length > 0 && lines.every((l) => numbers.test(l))) {
    const items = lines
      .map((l) => `<li>${renderInline(l.replace(numbers, ''))}</li>`)
      .join('');
    return `<ol>${items}</ol>`;
  }
  return `<p>${renderInline(lines.join('\n'))}</p>`;
}

export function renderMarkdown(source: string): string {
  const blocks = tokenize(source);
  return blocks
    .map((block) => {
      if (block.kind === 'code') {
        return highlightBlockHtml(block.code ?? '', { language: block.lang || null });
      }
      // Escape once, up front: every transform below only ever sees
      // entity-encoded text, so raw `<tag>`s from the model can never reach
      // the DOM. Fenced code is escaped by `highlightBlockHtml` itself.
      const paragraphs = escapeHtml(block.text ?? '')
        .split(/\n{2,}/)
        .map((p) => p.trim())
        .filter((p) => p.length > 0);
      return paragraphs.map((p) => renderParagraph(p)).join('');
    })
    .join('');
}
