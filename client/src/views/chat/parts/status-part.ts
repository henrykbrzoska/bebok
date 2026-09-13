/**
 * F9-7: one delegation progress row (`{ type: 'status' }` part).
 *
 * A compact, single-line, system-style row: muted monospace text with a
 * small dot coloured by kind - `task.started` = accent, `task.progress` =
 * muted, `task.ended` = success, or danger when the text reports a failure
 * ("failed" / "aborted"). The engine appends these to the parent's latest
 * assistant message while a child runs; they are never sent to the LLM.
 */

import { Component, computed, inject, input } from '@angular/core';
import { RouterLink } from '@angular/router';

import { Part, StatusPart } from '../../../core/engine.dtos';
import { I18nService } from '../../../i18n/i18n.service';

export type StatusTone = 'started' | 'progress' | 'success' | 'danger' | 'muted';

const FAILURE_RE = /\b(failed|aborted|error|cancell?ed)\b/i;

/** Pure tone resolution so the spec can pin the colour rules. */
export function statusTone(kind: string, text: string): StatusTone {
  switch (kind) {
    case 'task.started':
      return 'started';
    case 'task.progress':
      return 'progress';
    case 'task.ended':
      return FAILURE_RE.test(text) ? 'danger' : 'success';
    default:
      return 'muted';
  }
}

@Component({
  selector: 'app-status-part',
  imports: [RouterLink],
  template: `
    <div
      class="status-row"
      [class]="'status-row tone-' + tone()"
      [attr.data-kind]="statusPart().kind"
      [attr.data-tone]="tone()"
      data-testid="status-part"
      role="status"
    >
      <span class="dot" aria-hidden="true"></span>
      @if (childLink(); as link) {
        <a
          class="text link"
          [routerLink]="['/chat', link]"
          [title]="t('status.openChild')"
        >{{ statusPart().text }}</a>
      } @else {
        <span class="text">{{ statusPart().text }}</span>
      }
      @if (time()) {
        <span class="time">{{ time() }}</span>
      }
    </div>
  `,
  styles: `
    :host {
      display: block;
    }

    .status-row {
      display: flex;
      align-items: center;
      gap: var(--space-8);
      min-height: 20px;
      padding: 1px var(--space-4);
      font-family: var(--font-mono);
      font-size: var(--fs-11-5);
      color: var(--text-faint);
      min-width: 0;
    }

    .dot {
      flex: none;
      width: 6px;
      height: 6px;
      border-radius: 50%;
      background: var(--text-faint);
    }

    .tone-started .dot {
      background: var(--accent);
    }

    .tone-progress .dot {
      background: var(--text-faint);
      opacity: 0.7;
    }

    .tone-success .dot {
      background: var(--success);
    }

    .tone-danger .dot {
      background: var(--danger);
    }

    .tone-danger .text {
      color: var(--danger);
    }

    .text {
      flex: 1 1 auto;
      min-width: 0;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
      color: var(--text-muted);
    }

    .text.link {
      text-decoration: none;
    }

    .text.link:hover {
      color: var(--accent);
      text-decoration: underline;
    }

    .time {
      flex: none;
      font-size: var(--fs-11);
      color: var(--text-faint);
    }
  `,
})
export class StatusPartComponent {
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly part = input.required<Part>();

  readonly statusPart = computed(() => this.part() as StatusPart);
  readonly tone = computed(() => statusTone(this.statusPart().kind, this.statusPart().text));
  readonly childLink = computed(() => this.statusPart().childSessionID || null);
  readonly time = computed(() => {
    const at = this.statusPart().at;
    if (!at) {
      return '';
    }
    try {
      return new Date(at).toLocaleTimeString(this.i18n.lang(), {
        hour: '2-digit',
        minute: '2-digit',
      });
    } catch {
      return '';
    }
  });
}
