/**
 * Status dot (WP-SHELL): a colored dot that is ALWAYS paired with a text
 * label - the design handoff forbids conveying status by color alone.
 *
 * `label` is required and rendered next to the dot; pass `labelHidden` only
 * for dense rails where the label would not fit, in which case it is still
 * exposed to assistive tech through `aria-label`/`title`.
 */

import { ChangeDetectionStrategy, Component, input } from '@angular/core';

export type StatusTone = 'success' | 'warning' | 'danger' | 'muted' | 'accent' | 'idle';

@Component({
  selector: 'app-status-dot',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <span class="status" [class.label-hidden]="labelHidden()" [attr.title]="label()">
      <span class="dot" [class]="'tone-' + tone()" aria-hidden="true"></span>
      <span class="label">{{ label() }}</span>
    </span>
  `,
  styles: [
    `
      :host {
        display: inline-flex;
        min-width: 0;
        /* Callers may recolor the whole chip (e.g. the Start status row). */
        color: var(--text-muted);
        /* F9-1: the visually-hidden label below is position: absolute.
         * Without a positioned ancestor its containing block is the *document*,
         * so every hidden label of a row scrolled out of the sidebar session
         * list (or any other clipped container) still extends the document's
         * scrollable overflow - the page grew a second scrollbar and the whole
         * shell (topbar included) could be scrolled away. Anchoring the label
         * to the host keeps it inside the row's own clip. */
        position: relative;
      }

      .status {
        display: inline-flex;
        align-items: center;
        gap: 6px;
        min-width: 0;
        font-size: var(--fs-11-5);
        color: inherit;
      }

      .dot {
        width: 7px;
        height: 7px;
        border-radius: 50%;
        flex: none;
        background: var(--text-faint);
      }

      .dot.tone-success {
        background: var(--success);
      }

      .dot.tone-warning {
        background: var(--warning);
      }

      .dot.tone-danger {
        background: var(--danger);
      }

      .dot.tone-accent {
        background: var(--accent);
      }

      .dot.tone-muted {
        background: var(--text-muted);
      }

      .dot.tone-idle {
        background: transparent;
        border: 1px solid var(--border-strong);
      }

      .label {
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .label-hidden .label {
        position: absolute;
        width: 1px;
        height: 1px;
        padding: 0;
        margin: -1px;
        overflow: hidden;
        clip: rect(0 0 0 0);
        white-space: nowrap;
      }
    `,
  ],
})
export class StatusDot {
  /** Required: the textual status shown next to the dot. */
  readonly label = input.required<string>();
  readonly tone = input<StatusTone>('muted');
  /** Visually hide the label (still announced); use only in icon rails. */
  readonly labelHidden = input(false);
}
