import { Component, computed, input } from '@angular/core';

import { Part, UsagePart } from '../../../core/engine.dtos';

@Component({
  selector: 'app-usage-part',
  template: `
    <div class="usage muted">
      @if (tokensIn() !== null) {
        <span>we {{ tokensIn() }}</span>
        <span>wy {{ tokensOut() }}</span>
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
      font-size: 11.5px;
      justify-content: flex-end;
      border-top: 1px dashed var(--border);
      margin-top: 6px;
      padding-top: 6px;
    }
  `,
})
export class UsagePartComponent {
  readonly part = input.required<Part>();
  private readonly usagePart = computed(() => this.part() as UsagePart);
  readonly tokensIn = computed(() => this.usagePart().input_tokens);
  readonly tokensOut = computed(() => this.usagePart().output_tokens);
  readonly cost = computed(() => this.usagePart().cost ?? null);
}
