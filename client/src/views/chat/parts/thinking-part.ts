import { Component, computed, inject, input, signal } from '@angular/core';

import { Part, ThinkingPart } from '../../../core/engine.dtos';
import { renderMarkdown } from '../../../core/markdown';
import { I18nService } from '../../../i18n/i18n.service';

/**
 * Reasoning block (F2-5): a bordered `--surface` card, clearly distinct from
 * ordinary assistant text - 11.5px italic muted body behind a "💭 Thinking"
 * header that collapses and expands.
 */
@Component({
  selector: 'app-thinking-part',
  template: `
    <div class="thinking" [class.open]="open()">
      <button
        type="button"
        class="toggle"
        (click)="open.set(!open())"
        [attr.aria-expanded]="open()"
      >
        <span class="emoji" aria-hidden="true">💭</span>
        <span class="head">{{ t('thinking.head') }}</span>
        <span class="preview">{{ preview() }}</span>
        <span class="chevron" aria-hidden="true">{{ open() ? '▾' : '▸' }}</span>
      </button>
      @if (open()) {
        <div class="body" [innerHTML]="html()"></div>
      }
    </div>
  `,
  styles: `
    .thinking {
      border: 1px solid var(--border);
      border-radius: var(--radius-panel);
      background: var(--surface);
      overflow: hidden;
    }
    .toggle {
      display: flex;
      align-items: center;
      gap: var(--space-6);
      width: 100%;
      background: none;
      border: none;
      border-radius: 0;
      padding: var(--space-8) var(--space-12);
      color: var(--text-muted);
      font-size: var(--fs-11-5);
      cursor: pointer;
      text-align: left;
    }
    .toggle:hover {
      background: var(--surface-2);
    }
    .emoji {
      font-size: var(--fs-12);
      line-height: 1;
      flex: none;
    }
    .head {
      font-weight: 600;
      flex: none;
    }
    .preview {
      flex: 1 1 auto;
      min-width: 0;
      font-style: italic;
      color: var(--text-faint);
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .chevron {
      flex: none;
      font-size: 10px;
      color: var(--text-faint);
    }
    .body {
      font-size: var(--fs-11-5);
      font-style: italic;
      color: var(--text-muted);
      line-height: 1.65;
      padding: 0 var(--space-12) var(--space-12);
      overflow-wrap: anywhere;
    }
    .body pre {
      background: var(--terminal-bg);
      border: 1px solid var(--border);
      border-radius: var(--radius-control-sm);
      padding: 8px 10px;
      overflow-x: auto;
      font-size: var(--fs-11-5);
      font-style: normal;
      color: var(--code-text);
    }
    .body p {
      margin: 0 0 6px;
    }
    .body p:last-child {
      margin-bottom: 0;
    }
  `,
})
export class ThinkingPartComponent {
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly part = input.required<Part>();
  private readonly thinkingPart = computed(() => this.part() as ThinkingPart);
  readonly open = signal(false);
  readonly html = computed(() => renderMarkdown(this.thinkingPart().text));

  /** First line of the reasoning, shown next to the header while collapsed. */
  readonly preview = computed(() => {
    const text = this.thinkingPart().text;
    const first = text.trim().split('\n', 1)[0] ?? '';
    return first.length > 80 ? `${first.slice(0, 80)}…` : first;
  });
}
