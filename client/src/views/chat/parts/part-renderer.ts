import { Component, input } from '@angular/core';

import { Part } from '../../../core/engine.dtos';
import { TextPartComponent } from './text-part';
import { ThinkingPartComponent } from './thinking-part';
import { ToolPartComponent } from './tool-part';
import { UsagePartComponent } from './usage-part';

/** Maps one `Part` to its renderer (SPEC §7: parts drive the chat). */
@Component({
  selector: 'app-part-renderer',
  imports: [TextPartComponent, ThinkingPartComponent, ToolPartComponent, UsagePartComponent],
  template: `
    @switch (part().type) {
      @case ('text') {
        <app-text-part [part]="part()" />
      }
      @case ('thinking') {
        <app-thinking-part [part]="part()" />
      }
      @case ('tool') {
        <app-tool-part [part]="part()" />
      }
      @case ('usage') {
        <app-usage-part [part]="part()" />
      }
    }
  `,
})
export class PartRendererComponent {
  readonly part = input.required<Part>();
}
