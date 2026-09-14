/**
 * Clickable/editable JSON tree for the Settings RAW JSON tab (left column).
 *
 * Thin standalone wrapper around `vanilla-jsoneditor` (MIT, zero runtime
 * deps). The component owns the editor instance: the parent passes the parsed
 * object via `value`, receives edits via `changed`, and can push a freshly
 * parsed raw text back with `setValue()`. All sync/validation policy lives in
 * RawJsonTab — this component only bridges signals and the editor.
 */

import {
  Component,
  ElementRef,
  OnDestroy,
  afterNextRender,
  input,
  output,
  viewChild,
} from '@angular/core';
import { createJSONEditor, Mode, type Content } from 'vanilla-jsoneditor';

function isJsonContent(content: Content): content is { json: unknown } {
  return (content as { json?: unknown }).json !== undefined;
}

@Component({
  selector: 'app-tree-json-editor',
  template: `<div #host class="tree-host"></div>`,
  styleUrl: './tree-json-editor.css',
})
export class TreeJsonEditor implements OnDestroy {
  private readonly host = viewChild.required<ElementRef<HTMLElement>>('host');

  /** Parsed object from the store (raw text is the source of truth). */
  readonly value = input<unknown>({});
  readonly changed = output<unknown>();

  private editor?: ReturnType<typeof createJSONEditor>;
  /** Last value pushed into the editor (echo guard for setValue). */
  private lastPushed = '';
  /** Last value emitted via onChange (echo guard for value round-trips). */
  private lastEmitted = '';

  constructor() {
    afterNextRender(() => {
      const initial = this.value() ?? {};
      this.lastPushed = JSON.stringify(initial);
      this.lastEmitted = this.lastPushed;
      this.editor = createJSONEditor({
        target: this.host().nativeElement as HTMLDivElement,
        props: {
          content: { json: initial },
          mode: Mode.tree,
          mainMenuBar: false,
          statusBar: false,
          indentation: 2,
          onChange: (content: Content) => {
            if (!isJsonContent(content)) return;
            const serialized = JSON.stringify(content.json);
            this.lastEmitted = serialized;
            this.changed.emit(content.json);
          },
        },
      });
    });
  }

  /**
   * Push a freshly parsed raw text into the tree. No-op when the value is
   * identical to what the editor already holds (either pushed or emitted),
   * so keystrokes never echo back and collapse state is preserved.
   */
  setValue(next: unknown): void {
    const serialized = JSON.stringify(next ?? {});
    if (serialized === this.lastEmitted || serialized === this.lastPushed) return;
    this.lastPushed = serialized;
    void this.editor?.update?.({ json: next ?? {} });
  }

  ngOnDestroy(): void {
    this.editor?.destroy();
    this.editor = undefined;
  }
}
