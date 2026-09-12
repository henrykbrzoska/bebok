/**
 * Shared markdown renderer (F6-10): headings, fenced code, tables and links,
 * rendered to HTML and bound with `[innerHTML]` - see `markdown-view.render.ts`
 * for why this is safe (the source is always escaped first; only structural
 * markup is ever emitted, so this never becomes a sink for a tool's raw
 * untrusted output).
 *
 * A relative link (no URI scheme) is intercepted on click and resolved against
 * `[basePath]` (the engine path of the document currently shown); the
 * resolved path is emitted via `(linkClick)` instead of letting the browser
 * navigate away. An absolute/external link (`http(s)://`, `mailto:`, ...) is
 * left alone and opens as a normal link.
 */

import {
  ChangeDetectionStrategy,
  Component,
  ViewEncapsulation,
  computed,
  input,
  output,
} from '@angular/core';

import { renderMarkdownView } from './markdown-view.render';
import { resolveRelativePath } from './relative-path';

@Component({
  selector: 'app-markdown-view',
  changeDetection: ChangeDetectionStrategy.OnPush,
  // Rendered nodes come from innerHTML and do not receive Angular's scoped
  // attributes - scope the rules by the host element selector instead (matches
  // the pattern already used by `text-part.ts`'s `.md` block).
  encapsulation: ViewEncapsulation.None,
  template: `<div class="markdown-view" [innerHTML]="html()" (click)="onClick($event)"></div>`,
  styles: `
    app-markdown-view .markdown-view {
      font-size: 13px;
      line-height: 1.5;
      word-wrap: break-word;
    }
    app-markdown-view h1,
    app-markdown-view h2,
    app-markdown-view h3,
    app-markdown-view h4,
    app-markdown-view h5,
    app-markdown-view h6 {
      margin: 14px 0 6px;
      line-height: 1.3;
    }
    app-markdown-view h1:first-child,
    app-markdown-view h2:first-child,
    app-markdown-view h3:first-child {
      margin-top: 0;
    }
    app-markdown-view p {
      margin: 0 0 8px;
    }
    app-markdown-view pre {
      background: var(--terminal-bg);
      border: 1px solid var(--border);
      border-radius: var(--radius-panel);
      padding: 8px 10px;
      overflow-x: auto;
      margin: 8px 0;
      font-size: var(--fs-12-5);
      line-height: 1.5;
      color: var(--code-text-strong);
    }
    app-markdown-view code {
      background: rgba(255, 255, 255, 0.07);
      border-radius: 3px;
      padding: 1px 4px;
      font-size: 0.92em;
    }
    app-markdown-view pre code {
      background: none;
      padding: 0;
    }
    app-markdown-view ul,
    app-markdown-view ol {
      margin: 4px 0 8px;
      padding-left: 22px;
    }
    app-markdown-view a {
      color: var(--accent);
      cursor: pointer;
    }
    app-markdown-view blockquote {
      margin: 6px 0;
      padding: 1px 12px;
      border-left: 3px solid var(--border);
      color: var(--text-muted);
    }
    app-markdown-view table {
      border-collapse: collapse;
      margin: 8px 0;
      font-size: var(--fs-12-5);
      width: 100%;
    }
    app-markdown-view th,
    app-markdown-view td {
      border: 1px solid var(--border);
      padding: 4px 8px;
      text-align: left;
    }
    app-markdown-view th {
      background: var(--surface-2);
    }
  `,
})
export class MarkdownViewComponent {
  /** Raw markdown source (untrusted-content-safe - see file header). */
  readonly source = input.required<string>();
  /** Engine path of the document being rendered, for resolving relative links. */
  readonly basePath = input<string | null>(null);

  /** Emits the resolved engine path when a relative link is clicked. */
  readonly linkClick = output<string>();

  readonly html = computed(() => renderMarkdownView(this.source()));

  onClick(event: MouseEvent): void {
    const target = event.target as HTMLElement | null;
    const anchor = target?.closest?.('a.relative-link') as HTMLAnchorElement | null;
    if (!anchor) {
      return;
    }
    const href = anchor.getAttribute('href') ?? '';
    const resolved = resolveRelativePath(this.basePath(), href);
    if (resolved === null) {
      return;
    }
    event.preventDefault();
    this.linkClick.emit(resolved);
  }
}
