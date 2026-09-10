import { Component, computed, inject, input, output } from '@angular/core';

import { Message } from '../../../core/engine.dtos';
import { I18nService } from '../../../i18n/i18n.service';
import { formatMs } from '../../../core/format';
import { PartRendererComponent } from './part-renderer';

@Component({
  selector: 'app-message-row',
  imports: [PartRendererComponent],
  host: { '[id]': 'rowId()' },
  template: `
    <div class="message" [class.user-message]="isUser()">
      <div class="message-head">
        <span class="who">{{ isUser() ? 'Ty' : 'Bebok' }}</span>
        @if (!isUser() && agentModel()) {
          <span class="muted tag">{{ agentModel() }}</span>
        }
        <span class="muted time">{{ time() }}</span>
        @if (isUser() && rollbackEnabled()) {
          <button
            class="rollback"
            [title]="t('chat.rollback')"
            (click)="rollback.emit(message().id)"
          >&#8617;</button>
        }
      </div>
      <div class="parts">
        @for (part of message().parts; track $index) {
          <app-part-renderer [part]="part" [taskLinks]="taskLinks()" />
        }
      </div>
    </div>
  `,
  styles: `
    :host {
      display: block;
    }
    .message {
      padding: 12px 16px;
      border-radius: var(--radius);
      background: var(--bg-surface);
      border: 1px solid var(--border);
      max-width: 92%;
    }
    .message.user-message {
      align-self: flex-end;
      background: rgba(63, 111, 224, 0.16);
      border-color: rgba(63, 111, 224, 0.35);
    }
    .message-head {
      display: flex;
      align-items: baseline;
      gap: 10px;
      margin-bottom: 6px;
      flex-wrap: wrap;
    }
    .who {
      font-weight: 700;
      font-size: 12.5px;
      text-transform: uppercase;
      letter-spacing: 0.4px;
    }
    .tag {
      font-size: 11px;
      padding: 1px 6px;
      border: 1px solid var(--border);
      border-radius: 10px;
      background: var(--bg-raised);
    }
    .time {
      font-size: 11px;
    }
    .rollback {
      margin-left: auto;
      border: 1px solid var(--border);
      background: var(--bg-raised);
      color: var(--fg-muted);
      border-radius: 6px;
      font-size: 12px;
      line-height: 1;
      padding: 2px 7px;
      cursor: pointer;
    }
    .rollback:hover {
      color: var(--accent);
      border-color: var(--accent);
    }
    .parts {
      display: flex;
      flex-direction: column;
      gap: 2px;
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
  readonly time = computed(() => {
    const created = this.message().meta?.created_at;
    return created ? formatMs(created) : '';
  });
  readonly agentModel = computed(() => {
    const meta = this.message().meta;
    if (!meta?.agent && !meta?.model) {
      return '';
    }
    return [meta?.agent, meta?.model].filter(Boolean).join(' \u00b7 ');
  });
}
