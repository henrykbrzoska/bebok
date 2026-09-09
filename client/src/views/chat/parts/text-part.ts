import { Component, computed, inject, input, signal } from '@angular/core';

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
    .md {
      word-wrap: break-word;
    }
    .md p {
      margin: 0 0 8px;
    }
    .md p:last-child {
      margin-bottom: 0;
    }
    .md pre {
      background: var(--bg-raised);
      border: 1px solid var(--border);
      border-radius: var(--radius-sm);
      padding: 10px 12px;
      overflow-x: auto;
      margin: 8px 0;
      font-size: 12.5px;
      line-height: 1.5;
    }
    .md code {
      background: rgba(255, 255, 255, 0.07);
      border-radius: 3px;
      padding: 1px 4px;
      font-size: 0.92em;
    }
    .md pre code {
      background: none;
      padding: 0;
    }
    .md ul,
    .md ol {
      margin: 4px 0 8px;
      padding-left: 22px;
    }
    .md a {
      color: var(--accent);
    }
    .md blockquote {
      margin: 6px 0;
      padding: 2px 12px;
      border-left: 3px solid var(--border);
      color: var(--fg-muted);
    }
    .preview-wrap {
      margin-top: 10px;
      border-top: 1px dashed var(--border);
      padding-top: 10px;
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
