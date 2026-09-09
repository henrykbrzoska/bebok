import { Component, computed, input } from '@angular/core';

import { Part, TextPart } from '../../../core/engine.dtos';
import { renderMarkdown } from '../../../core/markdown';

@Component({
  selector: 'app-text-part',
  template: `<div class="md" [innerHTML]="html()"></div>`,
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
  `,
})
export class TextPartComponent {
  readonly part = input.required<Part>();
  private readonly textPart = computed(() => this.part() as TextPart);
  readonly html = computed(() => renderMarkdown(this.textPart().text));
}
