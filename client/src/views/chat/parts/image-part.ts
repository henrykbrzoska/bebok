import { Component, computed, input } from '@angular/core';

import { ImagePart, Part } from '../../../core/engine.dtos';

/** Renders an `image` part as an inline image (base64 data URL). */
@Component({
  selector: 'app-image-part',
  template: `<img class="msg-image" [src]="dataUrl()" [alt]="name()" />`,
  styles: `
    .msg-image {
      display: block;
      max-width: 100%;
      max-height: 320px;
      border-radius: var(--radius-sm);
      border: 1px solid var(--border);
      object-fit: contain;
      background: var(--bg-raised);
    }
  `,
})
export class ImagePartComponent {
  readonly part = input.required<Part>();
  private readonly img = computed(() => this.part() as ImagePart);
  readonly dataUrl = computed(
    () => `data:${this.img().media_type};base64,${this.img().data}`,
  );
  readonly name = computed(() => this.img().name ?? 'image');
}
