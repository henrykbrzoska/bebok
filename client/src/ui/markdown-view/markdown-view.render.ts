/**
 * Markdown-to-HTML renderer for the shared `app-markdown-view` component
 * (F6-10). Adapted from `core/markdown.ts`'s approach (escape-then-markup, so
 * the output is always safe to bind with `[innerHTML]` - the source is never
 * treated as pre-formed HTML) rather than `explorer-markdown.ts`'s row model,
 * because the Preview panel needs GFM-style pipe tables and real `<a>` links
 * with click interception, which are awkward to express as a discriminated
 * row union rendered through plain template bindings. See the "Deviations"
 * note in the work-package report for the full rationale.
 *
 * Supported: headings (h1-h6), fenced code blocks, blockquotes, bullet/
 * numbered lists, pipe tables (`| a | b |` + a `| --- | --- |` divider - the
 * common case, not full GFM), inline `code`/`**bold**`/`*italic*`, and links.
 * A link with no URI scheme (a relative path) is marked `class="relative-link"`
 * (Angular's `[innerHTML]` sanitizer keeps `class` but strips `data-*`)
 * so the host component can intercept the click and resolve it against the
 * document's own path instead of letting the browser navigate away.
 */

import { hasUriScheme } from './relative-path';

type Block =
  | { kind: 'heading'; level: 1 | 2 | 3 | 4 | 5 | 6; text: string }
  | { kind: 'code'; lang: string; code: string }
  | { kind: 'quote'; lines: string[] }
  | { kind: 'list'; ordered: boolean; items: string[] }
  | { kind: 'table'; header: string[]; align: Align[]; rows: string[][] }
  | { kind: 'paragraph'; text: string };

type Align = 'left' | 'center' | 'right' | null;

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/** Split a pipe-table row into trimmed cells, honoring `\|` as a literal pipe. */
function splitTableRow(line: string): string[] {
  let trimmed = line.trim();
  if (trimmed.startsWith('|')) {
    trimmed = trimmed.slice(1);
  }
  if (trimmed.endsWith('|') && !trimmed.endsWith('\\|')) {
    trimmed = trimmed.slice(0, -1);
  }
  const cells: string[] = [];
  let current = '';
  for (let i = 0; i < trimmed.length; i++) {
    const ch = trimmed[i];
    if (ch === '\\' && trimmed[i + 1] === '|') {
      current += '|';
      i += 1;
      continue;
    }
    if (ch === '|') {
      cells.push(current.trim());
      current = '';
      continue;
    }
    current += ch;
  }
  cells.push(current.trim());
  return cells;
}

const SEPARATOR_ROW = /^:?-{1,}:?$/;

function parseAlignRow(line: string): Align[] | null {
  const cells = splitTableRow(line);
  if (cells.length === 0) {
    return null;
  }
  const align: Align[] = [];
  for (const cell of cells) {
    if (!SEPARATOR_ROW.test(cell)) {
      return null;
    }
    const left = cell.startsWith(':');
    const right = cell.endsWith(':');
    align.push(left && right ? 'center' : right ? 'right' : left ? 'left' : null);
  }
  return align;
}

function isTableRow(line: string): boolean {
  return line.includes('|') && line.trim().length > 0;
}

/** Parse markdown source into a flat block list. */
function parseBlocks(source: string): Block[] {
  const lines = source.replace(/\r\n/g, '\n').split('\n');
  const blocks: Block[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];

    if (line.trim() === '') {
      i += 1;
      continue;
    }

    const fence = /^\s*```([\w+-]*)\s*$/.exec(line);
    if (fence) {
      const lang = fence[1] ?? '';
      const code: string[] = [];
      i += 1;
      while (i < lines.length && !/^\s*```\s*$/.test(lines[i])) {
        code.push(lines[i]);
        i += 1;
      }
      i += 1; // closing fence
      blocks.push({ kind: 'code', lang, code: code.join('\n') });
      continue;
    }

    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      blocks.push({
        kind: 'heading',
        level: heading[1].length as 1 | 2 | 3 | 4 | 5 | 6,
        text: heading[2].trim(),
      });
      i += 1;
      continue;
    }

    // Pipe table: a row followed by a valid `---`/`:--:` divider row.
    if (isTableRow(line) && i + 1 < lines.length) {
      const align = parseAlignRow(lines[i + 1]);
      if (align) {
        const header = splitTableRow(line);
        i += 2;
        const rows: string[][] = [];
        while (i < lines.length && isTableRow(lines[i])) {
          rows.push(splitTableRow(lines[i]));
          i += 1;
        }
        blocks.push({ kind: 'table', header, align, rows });
        continue;
      }
    }

    const quote = /^\s*>\s?(.*)$/.exec(line);
    if (quote) {
      const qlines: string[] = [quote[1]];
      i += 1;
      while (i < lines.length) {
        const next = /^\s*>\s?(.*)$/.exec(lines[i]);
        if (!next) {
          break;
        }
        qlines.push(next[1]);
        i += 1;
      }
      blocks.push({ kind: 'quote', lines: qlines });
      continue;
    }

    const bulletMatch = /^\s*[-*+]\s+(.*)$/.exec(line);
    const numberMatch = /^\s*\d+[.)]\s+(.*)$/.exec(line);
    if (bulletMatch || numberMatch) {
      const ordered = !!numberMatch;
      const re = ordered ? /^\s*\d+[.)]\s+(.*)$/ : /^\s*[-*+]\s+(.*)$/;
      const items: string[] = [(bulletMatch ?? numberMatch)![1]];
      i += 1;
      while (i < lines.length) {
        const m = re.exec(lines[i]);
        if (!m || lines[i].trim() === '') {
          break;
        }
        items.push(m[1]);
        i += 1;
      }
      blocks.push({ kind: 'list', ordered, items });
      continue;
    }

    // Paragraph: consume contiguous non-blank, non-special lines.
    const para: string[] = [line];
    i += 1;
    while (i < lines.length && lines[i].trim() !== '' && !/^(#{1,6})\s+/.test(lines[i])) {
      if (/^\s*```/.test(lines[i]) || /^\s*[-*+]\s+/.test(lines[i]) || /^\s*\d+[.)]\s+/.test(lines[i])) {
        break;
      }
      if (/^\s*>\s?/.test(lines[i])) {
        break;
      }
      if (isTableRow(lines[i]) && i + 1 < lines.length && parseAlignRow(lines[i + 1])) {
        break;
      }
      para.push(lines[i]);
      i += 1;
    }
    blocks.push({ kind: 'paragraph', text: para.join('\n') });
  }

  return blocks;
}

/** Inline formatting on already-escaped text: code, bold, italic, links. */
function renderInline(escaped: string): string {
  let html = escaped.replace(/`([^`\n]+)`/g, (_, code: string) => `<code>${code}</code>`);
  html = html.replace(/\*\*([^*\n]+)\*\*/g, (_, s: string) => `<strong>${s}</strong>`);
  html = html.replace(/(^|[^*])\*([^*\n]+)\*(?!\*)/g, (_, pre: string, s: string) => `${pre}<em>${s}</em>`);
  html = html.replace(/\[([^\]\n]+)\]\(([^)\s]+)\)/g, (_, label: string, href: string) => {
    if (hasUriScheme(href)) {
      return `<a href="${href}" target="_blank" rel="noreferrer">${label}</a>`;
    }
    return `<a href="${href}" class="relative-link">${label}</a>`;
  });
  return html;
}

function renderParagraphText(text: string): string {
  return text
    .split('\n')
    .map((l) => renderInline(escapeHtml(l)))
    .join('<br>');
}

function alignAttr(align: Align): string {
  return align ? ` style="text-align:${align}"` : '';
}

export function renderMarkdownView(source: string): string {
  const blocks = parseBlocks(source);
  return blocks
    .map((block) => {
      switch (block.kind) {
        case 'heading':
          return `<h${block.level}>${renderInline(escapeHtml(block.text))}</h${block.level}>`;
        case 'code': {
          const lang = block.lang ? ` data-lang="${escapeHtml(block.lang)}"` : '';
          return `<pre><code${lang}>${escapeHtml(block.code)}</code></pre>`;
        }
        case 'quote':
          return `<blockquote>${block.lines.map((l) => `<p>${renderInline(escapeHtml(l))}</p>`).join('')}</blockquote>`;
        case 'list': {
          const tag = block.ordered ? 'ol' : 'ul';
          const items = block.items.map((it) => `<li>${renderInline(escapeHtml(it))}</li>`).join('');
          return `<${tag}>${items}</${tag}>`;
        }
        case 'table': {
          const head = block.header
            .map((cell, idx) => `<th${alignAttr(block.align[idx] ?? null)}>${renderInline(escapeHtml(cell))}</th>`)
            .join('');
          const body = block.rows
            .map((row) => {
              const cells = block.header
                .map((_, idx) => {
                  const value = row[idx] ?? '';
                  return `<td${alignAttr(block.align[idx] ?? null)}>${renderInline(escapeHtml(value))}</td>`;
                })
                .join('');
              return `<tr>${cells}</tr>`;
            })
            .join('');
          return `<table><thead><tr>${head}</tr></thead><tbody>${body}</tbody></table>`;
        }
        case 'paragraph':
          return `<p>${renderParagraphText(block.text)}</p>`;
      }
    })
    .join('');
}
