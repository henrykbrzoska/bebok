import { ChangeDetectionStrategy, Component, computed, inject, input, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { Part, ToolPart } from '../../../core/engine.dtos';
import { I18nService } from '../../../i18n/i18n.service';
import { SurveyBridge } from '../survey-bridge';

/** `structured.survey` of a completed `ask_user` call (engine: bebok-tools/ask_user.rs). */
export interface SurveyQuestion {
  number: number;
  text: string;
  multi: boolean;
  options: string[];
  allowCustom: boolean;
}

export interface Survey {
  title?: string | null;
  questions: SurveyQuestion[];
}

export function surveyOf(part: Part): Survey | null {
  if (part.type !== 'tool' || part.state.state !== 'completed') {
    return null;
  }
  const structured = part.state.structured as unknown as { survey?: Survey } | undefined;
  const survey = structured?.survey;
  return survey && Array.isArray(survey.questions) && survey.questions.length > 0 ? survey : null;
}

export function optionLetter(index: number): string {
  return String.fromCharCode(65 + index);
}

interface Answer {
  picked: Set<number>;
  custom: string;
}

/**
 * One question's compact answer: `<number><letters>` with the custom slot's
 * letter followed by the text in parentheses, e.g. `2BC`, `3D(email, phone)`.
 */
export function formatAnswer(q: SurveyQuestion, a: Answer): string {
  const letters = [...a.picked]
    .sort((x, y) => x - y)
    .map(optionLetter)
    .join('');
  const custom = a.custom.trim();
  const customLetter = q.allowCustom && custom ? optionLetter(q.options.length) : '';
  const suffix = custom ? `(${custom.replace(/\)/g, '）')})` : '';
  return `${q.number}${letters}${customLetter}${suffix}`;
}

const ANSWERED_PREFIX = 'bebok.survey.answered.';

/**
 * The questionnaire card of an `ask_user` tool call: numbered questions,
 * lettered block options (radio / checkbox), a free-text slot per question,
 * and one "Send answers" button that posts `1A, 2BC, 3E(…)` as the next
 * user message. Answered surveys stay on screen read-only (remembered per
 * call id in localStorage).
 */
@Component({
  selector: 'app-survey-part',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  template: `
    <section class="survey" [class.done]="answered() !== null" data-testid="survey">
      <header class="survey-head">
        <span class="survey-badge">?</span>
        <span class="survey-title">{{ survey().title || t('survey.title') }}</span>
      </header>
      @for (q of survey().questions; track q.number) {
        <fieldset class="question" [attr.data-question]="q.number">
          <legend class="question-text">
            <span class="question-no">{{ q.number }}</span
            >{{ q.text }}
            <span class="question-kind">{{
              q.multi ? t('survey.multi') : t('survey.single')
            }}</span>
          </legend>
          <div class="options">
            @for (opt of q.options; track $index) {
              <label class="option" [class.on]="isPicked(q.number, $index)">
                <input
                  [type]="q.multi ? 'checkbox' : 'radio'"
                  [name]="'q' + q.number"
                  [checked]="isPicked(q.number, $index)"
                  [disabled]="answered() !== null || busy()"
                  (change)="pick(q, $index)"
                />
                <span class="letter">{{ letter($index) }}</span>
                <span class="label">{{ opt }}</span>
              </label>
            }
            @if (q.allowCustom) {
              <label class="option custom" [class.on]="customOf(q.number).length > 0">
                <span class="letter">{{ letter(q.options.length) }}</span>
                <input
                  type="text"
                  class="custom-input"
                  [placeholder]="t('survey.customPlaceholder')"
                  [ngModel]="customOf(q.number)"
                  (ngModelChange)="setCustom(q.number, $event)"
                  [disabled]="answered() !== null || busy()"
                  [attr.aria-label]="t('survey.customPlaceholder')"
                />
              </label>
            }
          </div>
        </fieldset>
      }
      <footer class="survey-foot">
        @if (answered(); as text) {
          <span class="answered mono" data-testid="survey-answered"
            >{{ t('survey.sent') }} {{ text }}</span
          >
        } @else {
          <span class="preview mono">{{ preview() || t('survey.pickHint') }}</span>
          <button
            type="button"
            class="primary"
            (click)="send()"
            [disabled]="!complete() || busy() || !bridge.canSubmit"
            data-testid="survey-send"
          >
            {{ busy() ? t('survey.sending') : t('survey.send') }}
          </button>
        }
      </footer>
      @if (error(); as message) {
        <div class="survey-error" role="alert">{{ message }}</div>
      }
    </section>
  `,
  styles: `
    .survey {
      border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--border));
      border-radius: var(--radius-panel);
      background: color-mix(in srgb, var(--accent) 6%, var(--surface));
      padding: var(--space-10) var(--space-12);
      display: flex;
      flex-direction: column;
      gap: var(--space-10);
    }
    .survey.done {
      border-color: var(--border);
      background: var(--surface);
    }
    .survey-head {
      display: flex;
      align-items: center;
      gap: var(--space-8);
      font-weight: 600;
    }
    .survey-badge {
      display: inline-flex;
      align-items: center;
      justify-content: center;
      width: 20px;
      height: 20px;
      border-radius: 50%;
      background: var(--accent);
      color: var(--bg);
      font-size: var(--fs-12);
      font-weight: 700;
    }
    .question {
      border: 0;
      padding: 0;
      margin: 0;
      display: flex;
      flex-direction: column;
      gap: var(--space-6);
    }
    .question-text {
      padding: 0;
      font-weight: 600;
      display: flex;
      align-items: baseline;
      gap: var(--space-6);
      flex-wrap: wrap;
    }
    .question-no {
      font-family: var(--font-mono);
      color: var(--accent);
    }
    .question-no::after {
      content: '.';
    }
    .question-kind {
      font-size: var(--fs-11);
      font-weight: 400;
      color: var(--text-muted);
    }
    .options {
      display: grid;
      grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
      gap: var(--space-6);
    }
    .option {
      display: flex;
      align-items: center;
      gap: var(--space-8);
      padding: var(--space-6) var(--space-8);
      border: 1px solid var(--border);
      border-radius: var(--radius-control-sm);
      background: var(--surface-2);
      cursor: pointer;
      min-width: 0;
    }
    .option:hover {
      border-color: var(--border-strong);
    }
    .option.on {
      border-color: var(--accent);
      background: color-mix(in srgb, var(--accent) 14%, var(--surface-2));
    }
    .option input[type='radio'],
    .option input[type='checkbox'] {
      margin: 0;
      accent-color: var(--accent);
    }
    .letter {
      font-family: var(--font-mono);
      font-weight: 700;
      color: var(--accent);
      flex: none;
      width: 1.2em;
    }
    .label {
      min-width: 0;
      overflow-wrap: anywhere;
    }
    .option.custom {
      grid-column: 1 / -1;
    }
    .custom-input {
      flex: 1 1 auto;
      min-width: 0;
      background: transparent;
      border: 0;
      border-bottom: 1px solid var(--border);
      color: var(--text);
      padding: 2px 0;
      font: inherit;
    }
    .custom-input:focus {
      outline: none;
      border-bottom-color: var(--accent);
    }
    .survey-foot {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: var(--space-10);
      flex-wrap: wrap;
    }
    .preview,
    .answered {
      font-size: var(--fs-12);
      color: var(--text-muted);
      overflow-wrap: anywhere;
    }
    .survey-error {
      color: var(--danger);
      font-size: var(--fs-12);
    }
  `,
})
export class SurveyPartComponent {
  private readonly i18n = inject(I18nService);
  readonly bridge = inject(SurveyBridge);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly part = input.required<Part>();
  readonly survey = computed<Survey>(() => surveyOf(this.part()) ?? { questions: [] });
  private readonly callId = computed(() => (this.part() as ToolPart).id);

  readonly answers = signal<Map<number, Answer>>(new Map());
  readonly busy = signal(false);
  readonly error = signal<string | null>(null);
  readonly answered = computed<string | null>(() => {
    void this.answeredVersion();
    try {
      return localStorage.getItem(ANSWERED_PREFIX + this.callId());
    } catch {
      return null;
    }
  });
  private readonly answeredVersion = signal(0);

  readonly letter = optionLetter;

  private answerOf(n: number): Answer {
    return this.answers().get(n) ?? { picked: new Set(), custom: '' };
  }

  isPicked(n: number, index: number): boolean {
    return this.answerOf(n).picked.has(index);
  }

  customOf(n: number): string {
    return this.answerOf(n).custom;
  }

  pick(q: SurveyQuestion, index: number): void {
    this.answers.update((map) => {
      const next = new Map(map);
      const current = this.answerOf(q.number);
      const picked = new Set(q.multi ? current.picked : []);
      if (q.multi && current.picked.has(index)) {
        picked.delete(index);
      } else {
        picked.add(index);
      }
      next.set(q.number, { picked, custom: current.custom });
      return next;
    });
  }

  setCustom(n: number, text: string): void {
    this.answers.update((map) => {
      const next = new Map(map);
      next.set(n, { ...this.answerOf(n), custom: text });
      return next;
    });
  }

  /** Every question answered (an option or the free-text slot). */
  readonly complete = computed(() =>
    this.survey().questions.every((q) => {
      const a = this.answerOf(q.number);
      return a.picked.size > 0 || (q.allowCustom && a.custom.trim().length > 0);
    }),
  );

  readonly preview = computed(() =>
    this.survey()
      .questions.filter((q) => {
        const a = this.answerOf(q.number);
        return a.picked.size > 0 || a.custom.trim().length > 0;
      })
      .map((q) => formatAnswer(q, this.answerOf(q.number)))
      .join(', '),
  );

  async send(): Promise<void> {
    if (!this.complete() || this.busy()) {
      return;
    }
    const text = this.survey()
      .questions.map((q) => formatAnswer(q, this.answerOf(q.number)))
      .join(', ');
    this.busy.set(true);
    this.error.set(null);
    try {
      await this.bridge.submit(text);
      try {
        localStorage.setItem(ANSWERED_PREFIX + this.callId(), text);
      } catch {
        /* keep it in memory */
      }
      this.answeredVersion.update((v) => v + 1);
    } catch (err) {
      this.error.set(err instanceof Error ? err.message : String(err));
    } finally {
      this.busy.set(false);
    }
  }
}
