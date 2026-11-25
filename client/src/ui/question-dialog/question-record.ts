import { Component, inject, input } from '@angular/core';

import { I18nService } from '../../i18n/i18n.service';
import type { AnsweredQuestion } from './question-dialog';

/**
 * Read-only transcript entry for an answered (or skipped) question.
 *
 * Styled like a collapsed-by-default tool call: every option is listed, the
 * ones the user picked are marked, and a free-text answer is shown below its
 * question. Nothing here is interactive and nothing is sent back to the engine.
 */
@Component({
  selector: 'app-question-record',
  template: `
    <div class="qrec" data-testid="question-record" [class.skipped]="!record().answers">
      <div class="qrec-head">
        <span class="dot" aria-hidden="true"></span>
        <span class="qrec-title">{{ t('question.recordTitle') }}</span>
        @if (!record().answers) {
          <span class="qrec-state">{{ t('question.skipped') }}</span>
        }
      </div>

      @for (q of record().questions; track $index; let qi = $index) {
        <div class="qrec-item">
          @if (q.header) {
            <div class="qrec-header">{{ q.header }}</div>
          }
          <div class="qrec-question">{{ q.question }}</div>

          @if (q.options.length > 0) {
            <ul class="qrec-options">
              @for (opt of q.options; track $index) {
                <li [class.picked]="isPicked(qi, opt.label)">
                  <span class="mark" aria-hidden="true">{{ mark(qi, opt.label, q.multiple) }}</span>
                  <span class="opt-label">{{ opt.label }}</span>
                  @if (opt.description) {
                    <span class="opt-desc">{{ opt.description }}</span>
                  }
                </li>
              }
            </ul>
          }

          @if (freeText(qi); as text) {
            <div class="qrec-text" data-testid="question-record-text">{{ text }}</div>
          }
        </div>
      }
    </div>
  `,
  styles: `
    :host {
      display: block;
    }

    .qrec {
      border: 1px solid var(--border);
      border-left: 3px solid var(--success);
      border-radius: var(--radius-panel);
      background: var(--surface);
      padding: var(--space-10) var(--space-14);
      display: flex;
      flex-direction: column;
      gap: var(--space-8);
      margin: var(--space-8) 0;
    }

    .qrec.skipped {
      border-left-color: var(--text-faint);
    }

    .qrec-head {
      display: flex;
      align-items: center;
      gap: var(--space-8);
      font-size: var(--fs-12);
    }

    .dot {
      width: 7px;
      height: 7px;
      border-radius: 50%;
      background: var(--success);
      flex: none;
    }

    .qrec.skipped .dot {
      background: var(--text-faint);
    }

    .qrec-title {
      font-weight: 600;
      color: var(--text);
    }

    .qrec-state {
      margin-left: auto;
      font-size: var(--fs-11);
      color: var(--text-faint);
    }

    .qrec-item {
      display: flex;
      flex-direction: column;
      gap: var(--space-4);
    }

    .qrec-header {
      font-size: var(--fs-11);
      text-transform: uppercase;
      letter-spacing: var(--label-tracking);
      color: var(--text-faint);
    }

    .qrec-question {
      font-size: var(--fs-13);
      color: var(--text);
    }

    .qrec-options {
      list-style: none;
      margin: 0;
      padding: 0;
      display: flex;
      flex-direction: column;
      gap: 2px;
    }

    .qrec-options li {
      display: flex;
      align-items: baseline;
      flex-wrap: wrap;
      gap: var(--space-6);
      font-size: var(--fs-12-5);
      color: var(--text-faint);
    }

    .qrec-options li.picked {
      color: var(--text);
      font-weight: 600;
    }

    .mark {
      font-family: var(--font-mono);
      width: 1.2em;
      flex: none;
    }

    .opt-desc {
      width: 100%;
      padding-left: calc(1.2em + var(--space-6));
      font-size: var(--fs-11-5);
      font-weight: 400;
      color: var(--text-faint);
    }

    .qrec-text {
      font-size: var(--fs-12-5);
      color: var(--text);
      background: var(--bg);
      border: 1px solid var(--border);
      border-radius: var(--radius-control-sm);
      padding: 4px 8px;
      overflow-wrap: anywhere;
    }
  `,
})
export class QuestionRecord {
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly record = input.required<AnsweredQuestion>();

  isPicked(questionIndex: number, label: string): boolean {
    const id = this.record().questions[questionIndex]?.id;
    if (!id) {
      return false;
    }
    const answer = this.record().answers?.[id];
    const values = Array.isArray(answer) ? answer : answer !== undefined ? [answer] : [];
    return values.includes(label);
  }

  mark(questionIndex: number, label: string, multiple: boolean): string {
    const picked = this.isPicked(questionIndex, label);
    if (multiple) {
      return picked ? '☑' : '☐';
    }
    return picked ? '●' : '○';
  }

  freeText(questionIndex: number): string {
    const question = this.record().questions[questionIndex];
    if (!question) {
      return '';
    }
    const answer = this.record().answers?.[question.id];
    if (typeof answer !== 'string') {
      return '';
    }
    // A single-select answer equal to an option label is a pick, not free text.
    if (!question.multiple && question.options.some((opt) => opt.label === answer)) {
      return '';
    }
    return answer;
  }
}
