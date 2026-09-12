import { Component, computed, inject, input } from '@angular/core';

import { Part, UsagePart } from '../../../core/engine.dtos';
import { I18nService } from '../../../i18n/i18n.service';

/** Per-turn token/cost footer under an assistant message. */
@Component({
  selector: 'app-usage-part',
  template: `
    <div class="usage">
      @if (tokensIn() !== null) {
        <span>{{ t('drawer.tokensIn') }} {{ tokensIn() }}</span>
        <span>{{ t('drawer.tokensOut') }} {{ tokensOut() }}</span>
      }
      @if (cost() !== null) {
        <span>· {{ cost() }} USD</span>
      }
    </div>
  `,
  styles: `
    .usage {
      display: flex;
      gap: 10px;
      font-family: var(--font-mono);
      font-size: var(--fs-11);
      color: var(--text-faint);
      justify-content: flex-end;
      border-top: 1px dashed var(--border);
      margin-top: 6px;
      padding-top: 6px;
    }
  `,
})
export class UsagePartComponent {
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly part = input.required<Part>();
  private readonly usagePart = computed(() => this.part() as UsagePart);
  readonly tokensIn = computed(() => this.usagePart().input_tokens);
  readonly tokensOut = computed(() => this.usagePart().output_tokens);
  readonly cost = computed(() => this.usagePart().cost ?? null);
}
