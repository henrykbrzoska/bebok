/**
 * Lightweight markdown "reading view" for the Explorer content pane (F2-17).
 *
 * The design handoff asks for *lightly* styled markdown - bold headings and
 * colored bullet keywords - not a full markdown implementation, and the brief
 * forbids adding a new dependency. `core/markdown.ts` (used by chat) renders to
 * an HTML string for `[innerHTML]`; here we instead emit structured rows that
 * the template renders with plain bindings, so nothing is ever injected as HTML.
 */

export type MarkdownRow =
  | { kind: 'h1' | 'h2' | 'h3'; text: string }
  | { kind: 'bullet'; keyword: string | null; text: string }
  | { kind: 'quote'; text: string }
  | { kind: 'code'; text: string }
  | { kind: 'blank' }
  | { kind: 'text'; text: string };

/** Drop the inline emphasis/code markers we do not render as marks. */
function plain(value: string): string {
  return value
    .replace(/\*\*([^*]+)\*\*/g, '$1')
    .replace(/(^|\W)\*([^*\n]+)\*(?=\W|$)/g, '$1$2')
    .replace(/`([^`\n]+)`/g, '$1')
    .replace(/\[([^\]\n]+)\]\([^)\s]+\)/g, '$1');
}

/**
 * A bullet's "keyword" is the short lead-in before a ` - `/` — ` separator, or
 * a leading `**bold**` run - that is what the mock paints in the add-diff green.
 */
function splitBullet(body: string): { keyword: string | null; text: string } {
  const bold = /^\*\*([^*]+)\*\*\s*[-–—:]?\s*(.*)$/.exec(body);
  if (bold) {
    return { keyword: bold[1].trim(), text: plain(bold[2]).trim() };
  }
  const dashed = /^([^-–—\n]{1,60}?)\s+[-–—]\s+(.*)$/.exec(body);
  if (dashed) {
    return { keyword: plain(dashed[1]).trim(), text: plain(dashed[2]).trim() };
  }
  return { keyword: null, text: plain(body).trim() };
}

/** Parse markdown source into the rows the Explorer content pane renders. */
export function toMarkdownRows(source: string): MarkdownRow[] {
  const rows: MarkdownRow[] = [];
  const lines = source.split(/\r?\n/);
  let inFence = false;

  for (const raw of lines) {
    const line = raw.replace(/\s+$/, '');

    if (/^\s*```/.test(line)) {
      inFence = !inFence;
      continue;
    }
    if (inFence) {
      rows.push({ kind: 'code', text: raw });
      continue;
    }
    if (line.trim() === '') {
      rows.push({ kind: 'blank' });
      continue;
    }

    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      const level = heading[1].length;
      rows.push({
        kind: level === 1 ? 'h1' : level === 2 ? 'h2' : 'h3',
        text: plain(heading[2]).trim(),
      });
      continue;
    }

    const bullet = /^\s*[-*+]\s+(.*)$/.exec(line);
    if (bullet) {
      rows.push({ kind: 'bullet', ...splitBullet(bullet[1]) });
      continue;
    }

    const quote = /^\s*>\s?(.*)$/.exec(line);
    if (quote) {
      rows.push({ kind: 'quote', text: plain(quote[1]) });
      continue;
    }

    rows.push({ kind: 'text', text: plain(line) });
  }

  return rows;
}
