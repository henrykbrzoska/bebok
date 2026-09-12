import { Component, computed, inject, input, output } from '@angular/core';

import { Message, Part } from '../../../core/engine.dtos';
import { I18nService } from '../../../i18n/i18n.service';
import { formatMs } from '../../../core/format';
import { PartRendererComponent } from './part-renderer';

/** One part plus the info the renderer needs to pick its default state. */
interface RenderedPart {
  part: Part;
  /** 0-based index of this part among the tool parts of the same message. */
  toolIndex: number;
}

/**
 * One message in the transcript (F2-4).
 *
 * User turns are right-aligned, compact `--accent` bubbles; assistant turns have
 * no bubble at all - just an `ASSISTANT` micro-label above a readable body.
 */
@Component({
  selector: 'app-message-row',
  imports: [PartRendererComponent],
  host: { '[id]': 'rowId()', '[class.user]': 'isUser()' },
  template: `
    @if (isUser()) {
      <div class="user-row">
        @if (rollbackEnabled()) {
          <button
            type="button"
            class="rollback"
            [title]="t('chat.rollback')"
            [attr.aria-label]="t('chat.rollback')"
            (click)="rollback.emit(message().id)"
          >&#8617;</button>
        }
        <div class="bubble">
          @for (row of parts(); track $index) {
            <app-part-renderer
              [part]="row.part"
              [toolIndex]="row.toolIndex"
              [taskLinks]="taskLinks()"
            />
          }
        </div>
      </div>
    } @else {
      <div class="assistant-row">
        <div class="label-row">
          <span class="who">{{ t('chat.assistantLabel') }}</span>
          @if (agentModel()) {
            <span class="model">{{ agentModel() }}</span>
          }
          @if (time()) {
            <span class="time">{{ time() }}</span>
          }
        </div>
        <div class="body">
          @for (row of parts(); track $index) {
            <app-part-renderer
              [part]="row.part"
              [toolIndex]="row.toolIndex"
              [taskLinks]="taskLinks()"
            />
          }
        </div>
      </div>
    }
  `,
  styles: `
    :host {
      display: block;
    }

    /* --- user turn: right-aligned accent bubble --- */
    .user-row {
      display: flex;
      align-items: flex-start;
      justify-content: flex-end;
      gap: var(--space-6);
    }
    .bubble {
      max-width: min(68%, 620px);
      padding: 5px 9px;
      background: color-mix(in srgb, var(--accent) 16%, var(--surface-3));
      color: var(--text);
      border-radius: var(--radius-bubble) var(--radius-bubble) 3px var(--radius-bubble);
      font-size: 13px;
      line-height: 1.4;
      overflow-wrap: anywhere;
    }
    .rollback {
      align-self: center;
      flex: none;
      border: 1px solid var(--border);
      background: var(--surface-2);
      color: var(--text-faint);
      border-radius: var(--radius-control-sm);
      font-size: var(--fs-12);
      line-height: 1;
      padding: 3px 7px;
      opacity: 0;
      transition: opacity 120ms ease;
    }
    .user-row:hover .rollback,
    .rollback:focus-visible {
      opacity: 1;
    }
    .rollback:hover {
      color: var(--accent);
      border-color: var(--accent);
    }

    /* --- assistant turn: no bubble --- */
    .assistant-row {
      display: flex;
      flex-direction: column;
      gap: 3px;
      max-width: 100%;
    }
    .label-row {
      display: flex;
      align-items: baseline;
      gap: var(--space-8);
    }
    .who {
      font-size: var(--fs-11);
      font-weight: 600;
      letter-spacing: var(--label-tracking);
      text-transform: uppercase;
      color: var(--text-faint);
    }
    .model,
    .time {
      font-family: var(--font-mono);
      font-size: var(--fs-11);
      color: var(--text-faint);
    }
    .body {
      font-size: 13px;
      line-height: 1.45;
      color: var(--text);
      display: flex;
      flex-direction: column;
      gap: var(--space-4);
      overflow-wrap: anywhere;
    }

    @media (max-width: 700px) {
      .bubble {
        max-width: 86%;
      }
    }
  `,
})
export class MessageRowComponent {
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);
  readonly message = input.required<Message>();
  readonly rollbackEnabled = input(false);
  readonly rollback = output<string>();
  readonly rowId = input('');
  /** Task name/ID → childSessionID map for clickable sub-agent links. */
  readonly taskLinks = input<Map<string, string>>(new Map());
  readonly isUser = computed(() => this.message().role === 'user');

  /**
   * Parts plus their tool ordinal: the first tool call of a turn renders
   * expanded, every later one collapsed (F2-6).
   */
  readonly parts = computed<RenderedPart[]>(() => {
    let toolIndex = -1;
    return this.message().parts.map((part) => {
      if (part.type === 'tool') {
        toolIndex += 1;
        return { part, toolIndex };
      }
      return { part, toolIndex: -1 };
    });
  });

  readonly time = computed(() => {
    const created = this.message().meta?.created_at;
    return created ? formatMs(created) : '';
  });
  readonly agentModel = computed(() => {
    const meta = this.message().meta;
    if (!meta?.agent && !meta?.model) {
      return '';
    }
    return [meta?.agent, meta?.model].filter(Boolean).join(' · ');
  });
}
