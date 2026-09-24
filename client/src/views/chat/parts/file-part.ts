import { Component, computed, input } from '@angular/core';

import { FilePart, Part } from '../../../core/engine.dtos';

/** Renders a text-file attachment without putting its full contents in the DOM. */
@Component({
  selector: 'app-file-part',
  template: `<div class="msg-file"><span aria-hidden="true">📄</span><span class="name">{{ name() }}</span><span class="type">{{ type() }}</span></div>`,
  styles: `
    .msg-file { display: flex; align-items: center; gap: 8px; padding: 8px 10px; border: 1px solid var(--border); border-radius: var(--radius-sm); background: var(--bg-raised); }
    .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .type { color: var(--text-faint); font-family: var(--font-mono); font-size: var(--fs-11); }
  `,
})
export class FilePartComponent {
  readonly part = input.required<Part>();
  private readonly file = computed(() => this.part() as FilePart);
  readonly name = computed(() => this.file().name);
  readonly type = computed(() => this.file().media_type);
}
