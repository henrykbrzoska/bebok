import { Component, ViewEncapsulation, computed, inject, input, signal } from '@angular/core';

import { Part, TextPart } from '../../../core/engine.dtos';
import { renderMarkdown } from '../../../core/markdown';
import { I18nService } from '../../../i18n/i18n.service';
import { HtmlPreviewComponent } from '../../../ui/html-preview/html-preview';

/** Extract fenced ```html blocks from raw markdown (untrusted source). */
function extractHtmlBlocks(source: string): string[] {
  const out: string[] = [];
  const lines = source.split('\n');
  let i = 0;
  while (i < lines.length) {
    const fence = /^```(html?|xhtml)\s*$/i.exec(lines[i] ?? '');
    if (fence) {
      const code: string[] = [];
      i += 1;
      while (i < lines.length && !/^```\s*$/.test(lines[i] ?? '')) {
        code.push(lines[i] ?? '');
        i += 1;
      }
      i += 1;
      out.push(code.join('\n'));
    } else {
      i += 1;
    }
  }
  return out;
}

@Component({
  selector: 'app-text-part',
  // Markdown nodes come from innerHTML and do not receive Angular's scoped
  // attributes. Scope these rules by the component element instead.
  encapsulation: ViewEncapsulation.None,
  imports: [HtmlPreviewComponent],
  template: `
    <div
      class="md"
      [innerHTML]="html()"
      (contextmenu)="onContextMenu($event)"
      [title]="t('htmlPreview.rightClickHint')"
    ></div>
    @if (previewHtml() !== null) {
      <div class="preview-wrap">
        <app-html-preview
          [html]="previewHtml() ?? ''"
          [label]="t('htmlPreview.title')"
          (closed)="previewHtml.set(null)"
        />
      </div>
    }
  `,
  styles: `
    app-text-part .md {
      word-wrap: break-word;
      font-size: 13px;
      line-height: 1.45;
    }
    app-text-part .md p {
      margin: 0 0 4px;
    }
    app-text-part .md p:last-child {
      margin-bottom: 0;
    }
    app-text-part .md pre {
      background: var(--terminal-bg);
      border: 1px solid var(--border);
      border-radius: var(--radius-panel);
      padding: 8px 10px;
      overflow-x: auto;
      margin: 6px 0;
      font-size: var(--fs-12-5);
      line-height: 1.5;
      color: var(--code-text-strong);
    }
    app-text-part .md code {
      background: rgba(255, 255, 255, 0.07);
      border-radius: 3px;
      padding: 1px 4px;
      font-size: 0.92em;
    }
    app-text-part .md pre code {
      background: none;
      padding: 0;
    }
    app-text-part .md ul,
    app-text-part .md ol {
      margin: 3px 0 6px;
      padding-left: 22px;
    }
    app-text-part .md a {
      color: var(--accent);
    }
    app-text-part .md blockquote {
      margin: 4px 0;
      padding: 1px 10px;
      border-left: 3px solid var(--border);
      color: var(--fg-muted);
    }
    app-text-part .preview-wrap {
      margin-top: 8px;
      border-top: 1px dashed var(--border);
      padding-top: 8px;
    }

    /* Keep links and code readable on the muted user bubble background. */
    .bubble app-text-part .md a {
      color: inherit;
      text-decoration: underline;
    }
    .bubble app-text-part .md code {
      background: rgba(0, 0, 0, 0.14);
      color: inherit;
    }
    .bubble app-text-part .md pre {
      background: rgba(0, 0, 0, 0.2);
      border-color: rgba(0, 0, 0, 0.18);
      color: inherit;
    }
    .bubble app-text-part .md blockquote {
      border-left-color: rgba(0, 0, 0, 0.25);
      color: inherit;
    }
  `,
})
export class TextPartComponent {
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly part = input.required<Part>();
  private readonly textPart = computed(() => this.part() as TextPart);
  readonly html = computed(() => renderMarkdown(this.textPart().text));

  /** Sandboxed preview of the first ```html block (right-click to open). */
  readonly previewHtml = signal<string | null>(null);

  /** Right-click a rendered HTML code block: open it sandboxed below. */
  onContextMenu(event: MouseEvent): void {
    const target = event.target as HTMLElement | null;
    const code = target?.closest?.('pre code');
    if (!code) {
      return;
    }
    const blocks = extractHtmlBlocks(this.textPart().text);
    if (blocks.length === 0) {
      return;
    }
    event.preventDefault();
    this.previewHtml.set(blocks[0] ?? null);
  }
}
