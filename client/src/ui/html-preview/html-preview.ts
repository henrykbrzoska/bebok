/**
 * Sandboxed HTML preview (Task 6): renders untrusted HTML in an
 * `<iframe sandbox="">` (no scripts, no same-origin, no forms) via `srcdoc`.
 * Used by explorer (right-click "Preview HTML") and chat text parts
 * (right-click an HTML code block). Pure display — never executed.
 */

import { Component, inject, input, output } from '@angular/core';

import { I18nService } from '../../i18n/i18n.service';

@Component({
  selector: 'app-html-preview',
  template: `
    <div class="html-preview">
      <header class="html-preview-head">
        <span class="muted">{{ label() }}</span>
        <span class="spacer"></span>
        <button type="button" (click)="closed.emit()">{{ t('htmlPreview.close') }}</button>
      </header>
      <iframe class="html-frame" sandbox="" [srcdoc]="html()"></iframe>
      <p class="muted hint">{{ t('htmlPreview.sandboxHint') }}</p>
    </div>
  `,
  styles: `
    .html-preview {
      display: flex;
      flex-direction: column;
      gap: 8px;
      min-height: 0;
    }
    .html-preview-head {
      display: flex;
      align-items: center;
      gap: 8px;
      font-size: 12.5px;
    }
    .html-preview-head .spacer {
      flex: 1 1 auto;
    }
    .html-frame {
      width: 100%;
      min-height: 320px;
      height: 50vh;
      border: 1px solid var(--border);
      border-radius: var(--radius-sm);
      background: #fff;
    }
    .hint {
      font-size: 11.5px;
      margin: 0;
    }
  `,
})
export class HtmlPreviewComponent {
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);

  /** Raw HTML source (untrusted; `sandbox=""` blocks scripts). */
  readonly html = input.required<string>();
  /** Caption shown in the header (e.g. file name). */
  readonly label = input<string>('');
  readonly closed = output<void>();
}
