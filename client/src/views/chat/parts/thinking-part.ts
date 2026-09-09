import { Component, computed, inject, input, signal } from '@angular/core';

import { Part, ThinkingPart } from '../../../core/engine.dtos';
import { renderMarkdown } from '../../../core/markdown';
import { I18nService } from '../../../i18n/i18n.service';

@Component({
  selector: 'app-thinking-part',
  template: `
    <div class="thinking" [class.open]="open()">
      <button class="toggle" (click)="open.set(!open())">
        <span class="chevron">{{ open() ? '▾' : '▸' }}</span>
        <span class="label">{{ label() }}</span>
      </button>
      @if (open()) {
        <div class="body" [innerHTML]="html()"></div>
      }
    </div>
  `,
  styles: `
    .thinking {
      border-left: 3px solid var(--border);
      margin: 2px 0 8px;
      padding-left: 8px;
      color: var(--fg-muted);
    }
    .toggle {
      display: inline-flex;
      align-items: center;
      gap: 6px;
      background: none;
      border: none;
      padding: 2px 0;
      color: var(--fg-muted);
      font-size: 12.5px;
      cursor: pointer;
    }
    .chevron {
      display: inline-block;
      width: 12px;
      font-size: 10px;
    }
    .body {
      font-size: 13px;
      color: var(--fg-muted);
      padding-top: 4px;
    }
    .body pre {
      background: var(--bg-raised);
      border-radius: var(--radius-sm);
      padding: 8px 10px;
      overflow-x: auto;
      font-size: 12px;
    }
  `,
})
export class ThinkingPartComponent {
  private readonly i18n = inject(I18nService);

  readonly part = input.required<Part>();
  private readonly thinkingPart = computed(() => this.part() as ThinkingPart);
  readonly open = signal(false);
  readonly html = computed(() => renderMarkdown(this.thinkingPart().text));
  readonly label = computed(() => {
    const text = this.thinkingPart().text;
    const first = text.trim().split('\n', 1)[0] ?? '';
    const snippet = first.length > 80 ? `${first.slice(0, 80)}…` : first;
    return this.i18n.t('thinking.label', { text: snippet });
  });
}
