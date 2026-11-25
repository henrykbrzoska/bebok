import {
  Component,
  ElementRef,
  OnDestroy,
  computed,
  effect,
  inject,
  input,
  linkedSignal,
  output,
  signal,
  viewChild,
} from '@angular/core';

import { EngineClient } from '../../core/engine-client.service';
import {
  EngineEvent,
  QuestionAsked,
  QuestionItem,
  parseQuestionAsked,
} from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { I18nService } from '../../i18n/i18n.service';

/** Re-exported for existing imports (specs, chat view). */
export { parseQuestionAsked };
export type { QuestionAsked };

/** A question prompt the agent is waiting on, as shown in the dialog. */
export interface PendingQuestion {
  requestID: string;
  /** Session that raised the question (the sub-agent's child session for `task`). */
  sessionID: string;
  messageIndex: number;
  questions: QuestionItem[];
  askedAt: number;
  busy: boolean;
  error?: string;
}

/** A question the user answered or skipped; rendered back into the transcript. */
export interface AnsweredQuestion {
  requestID: string;
  sessionID: string;
  questions: QuestionItem[];
  /** `null` when the user skipped the prompt. */
  answers: Record<string, string | string[]> | null;
}

/** Per-question form state: selected option labels and free-text answer, by index. */
export interface QuestionDraft {
  selected: string[][];
  text: string[];
}

function emptyDraft(count: number): QuestionDraft {
  return {
    selected: Array.from({ length: count }, () => []),
    text: Array.from({ length: count }, () => ''),
  };
}

let dialogSeq = 0;

/**
 * Question prompt (client question tool).
 *
 * The agent calls the question tool and its turn blocks until the user
 * answers. This is a **modal** dialog (same backdrop as the SSH password
 * prompt): one fieldset per question with its header, radio buttons (one
 * answer) or checkboxes (several), and a free-text field when the question
 * allows it. Submit is enabled once every question has an answer; Skip sends
 * an empty skipped answer. Escape = Skip.
 *
 * The answer goes to `POST /session/{id}/question/{requestID}` - the engine
 * resumes the turn. The dialog then emits `answered` so the chat can render
 * the Q&A back into the transcript as a read-only record.
 */
@Component({
  selector: 'app-question-dialog',
  template: `
    @if (current(); as item) {
      <div class="q-backdrop" data-testid="question-popup">
        <div
          class="q"
          role="dialog"
          aria-modal="true"
          [attr.aria-labelledby]="titleId"
          [attr.aria-describedby]="bodyId"
          (keydown)="onKeydown($event)"
          #dialog
        >
          <div class="q-head">
            <span class="dot" aria-hidden="true"></span>
            <h2 class="q-title" [id]="titleId">{{ t('question.title') }}</h2>
            @if (queue() > 1) {
              <span class="queue">+{{ queue() - 1 }}</span>
            }
          </div>

          <div class="q-body" [id]="bodyId">
            @for (q of item.questions; track $index; let qi = $index) {
              <fieldset class="q-item" data-testid="question-item">
                @if (q.header) {
                  <legend class="q-header">{{ q.header }}</legend>
                }
                <p class="q-text">{{ q.question }}</p>
                @if (q.multiple && q.options.length > 0) {
                  <p class="q-hint">{{ t('question.multiHint') }}</p>
                }

                @if (q.options.length > 0) {
                  <div class="q-options">
                    @for (opt of q.options; track $index) {
                      <label class="q-option">
                        @if (q.multiple) {
                          <input
                            type="checkbox"
                            data-testid="question-option"
                            [value]="opt.label"
                            [checked]="isSelected(qi, opt.label)"
                            [disabled]="item.busy"
                            (change)="toggleOption(qi, opt.label, $any($event.target).checked)"
                          />
                        } @else {
                          <input
                            type="radio"
                            data-testid="question-option"
                            [name]="groupName(qi)"
                            [value]="opt.label"
                            [checked]="isSelected(qi, opt.label)"
                            [disabled]="item.busy"
                            (change)="toggleOption(qi, opt.label, true)"
                          />
                        }
                        <span class="q-opt-label">{{ opt.label }}</span>
                        @if (opt.description) {
                          <span class="q-opt-desc">{{ opt.description }}</span>
                        }
                      </label>
                    }
                  </div>
                }

                @if (q.allowFreeText) {
                  <input
                    type="text"
                    class="q-text-input"
                    autocomplete="off"
                    spellcheck="false"
                    data-testid="question-text"
                    [attr.aria-label]="q.question"
                    [placeholder]="t('question.otherPlaceholder')"
                    [value]="draft().text[qi] ?? ''"
                    [disabled]="item.busy"
                    (input)="setText(qi, $any($event.target).value)"
                    (keydown.enter)="$event.preventDefault(); submit()"
                  />
                }
              </fieldset>
            }
          </div>

          @if (item.error) {
            <p class="q-error" role="alert">{{ item.error }}</p>
          }

          <div class="q-actions">
            <button
              type="button"
              class="submit"
              data-testid="question-submit"
              [disabled]="item.busy || !canSubmit()"
              (click)="submit()"
            >{{ t('question.submit') }}</button>
            <button
              type="button"
              class="skip"
              data-testid="question-skip"
              [disabled]="item.busy"
              (click)="skip()"
            >{{ t('question.skip') }}</button>
          </div>
        </div>
      </div>
    }
  `,
  styles: `
    :host {
      display: block;
    }

    .q-backdrop {
      position: fixed;
      inset: 0;
      z-index: 50;
      display: flex;
      align-items: flex-start;
      justify-content: center;
      padding: 10vh var(--space-16) var(--space-16);
      background: color-mix(in srgb, var(--bg) 72%, transparent);
    }

    .q {
      width: min(560px, 100%);
      max-height: 80vh;
      overflow: auto;
      border: 1px solid var(--warning);
      border-radius: var(--radius-panel);
      background: var(--surface);
      padding: var(--space-14) var(--space-16);
      display: flex;
      flex-direction: column;
      gap: var(--space-10);
    }

    .q-head {
      display: flex;
      align-items: center;
      gap: var(--space-8);
    }

    .dot {
      width: 7px;
      height: 7px;
      border-radius: 50%;
      background: var(--warning);
      flex: none;
    }

    .q-title {
      margin: 0;
      font-size: var(--fs-13-5);
      font-weight: 600;
      color: var(--text);
    }

    .queue {
      margin-left: auto;
      font-size: var(--fs-11);
      color: var(--text-faint);
    }

    .q-body {
      display: flex;
      flex-direction: column;
      gap: var(--space-12);
    }

    .q-item {
      margin: 0;
      padding: 0;
      border: 0;
      display: flex;
      flex-direction: column;
      gap: var(--space-6);
      min-width: 0;
    }

    .q-header {
      font-size: var(--fs-11);
      text-transform: uppercase;
      letter-spacing: var(--label-tracking);
      color: var(--text-faint);
      padding: 0;
    }

    .q-text {
      margin: 0;
      font-size: var(--fs-13);
      line-height: 1.55;
      color: var(--text);
    }

    .q-hint {
      margin: 0;
      font-size: var(--fs-11-5);
      color: var(--text-faint);
    }

    .q-options {
      display: flex;
      flex-direction: column;
      gap: var(--space-4);
    }

    .q-option {
      display: flex;
      align-items: baseline;
      flex-wrap: wrap;
      gap: var(--space-6);
      font-size: var(--fs-13);
      color: var(--text);
      cursor: pointer;
    }

    .q-opt-desc {
      width: 100%;
      padding-left: 22px;
      font-size: var(--fs-11-5);
      color: var(--text-faint);
    }

    .q-text-input {
      font: inherit;
      font-size: var(--fs-13);
      padding: 6px 10px;
      border: 1px solid var(--border);
      border-radius: var(--radius-control-sm);
      background: var(--bg);
      color: var(--text);
    }

    .q-error {
      margin: 0;
      font-size: var(--fs-12);
      color: var(--danger);
    }

    .q-actions {
      display: flex;
      align-items: center;
      gap: var(--space-8);
    }

    .q-actions button {
      font-size: var(--fs-12-5);
      padding: 5px 14px;
      border-radius: var(--radius-control-sm);
    }

    .submit {
      background: var(--success);
      border: 1px solid var(--success);
      color: var(--bg);
      font-weight: 600;
    }

    .submit:hover:not(:disabled) {
      filter: brightness(1.08);
    }

    .skip {
      margin-left: auto;
      background: transparent;
      border: 1px solid var(--border-strong);
      color: var(--text-muted);
    }

    .skip:hover:not(:disabled) {
      color: var(--text);
      border-color: var(--text-muted);
    }
  `,
})
export class QuestionDialog implements OnDestroy {
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly activeSessionID = input<string>();
  /** Working directory of the open chat; sub-agent questions carry the child session id. */
  readonly directory = input<string>();

  /** Emitted once the user answered or skipped a prompt. */
  readonly answered = output<AnsweredQuestion>();

  readonly asks = signal<PendingQuestion[]>([]);
  readonly current = computed(() => this.asks()[0] ?? null);
  readonly queue = computed(() => this.asks().length);
  /** True while a question is open: the chat composer must not send. */
  readonly blocking = computed(() => this.asks().length > 0);

  /** Form state of the prompt on screen; resets whenever a new prompt takes its place. */
  readonly draft = linkedSignal<string, QuestionDraft>({
    source: () => this.current()?.requestID ?? '',
    computation: () => emptyDraft(this.current()?.questions.length ?? 0),
  });

  /** True when every question has an answer (a selection or non-empty text). */
  readonly canSubmit = computed(() => {
    const item = this.current();
    if (!item) {
      return false;
    }
    const draft = this.draft();
    return item.questions.every(
      (_, index) =>
        (draft.selected[index]?.length ?? 0) > 0 || (draft.text[index] ?? '').trim().length > 0,
    );
  });

  readonly titleId = `question-title-${++dialogSeq}`;
  readonly bodyId = `question-body-${dialogSeq}`;

  private readonly dialog = viewChild<ElementRef<HTMLElement>>('dialog');

  private readonly unsubscribe: () => void;
  private readonly resolvedIds = new Set<string>();

  constructor() {
    this.unsubscribe = this.events.onEvent((event: EngineEvent) => this.handle(event));
    // Switching directory must not show questions from the previous one.
    effect(() => {
      this.directory();
      this.asks.set([]);
      this.resolvedIds.clear();
    });
    // Resync: if a `question.asked` event was missed (reconnect, late mount),
    // the turn would block forever with no dialog. Ask the engine for pending
    // prompts whenever the active session changes.
    effect(() => {
      const sessionID = this.activeSessionID();
      if (!sessionID) {
        return;
      }
      void this.resync(sessionID);
    });
    // Focus the first answer control of a new prompt so keyboard users can start at once.
    effect(() => {
      const id = this.current()?.requestID;
      if (!id) {
        return;
      }
      queueMicrotask(() => {
        const root = this.dialog()?.nativeElement;
        const first = root?.querySelector<HTMLElement>('input:not([disabled])');
        (first ?? root?.querySelector<HTMLElement>('button'))?.focus();
      });
    });
  }

  ngOnDestroy(): void {
    this.unsubscribe();
  }

  groupName(questionIndex: number): string {
    return `${this.titleId}-q${questionIndex}`;
  }

  isSelected(questionIndex: number, label: string): boolean {
    return this.draft().selected[questionIndex]?.includes(label) ?? false;
  }

  /** Radio: replace the selection. Checkbox: add or remove the label. */
  toggleOption(questionIndex: number, label: string, checked: boolean): void {
    const item = this.current();
    const multiple = item?.questions[questionIndex]?.multiple ?? false;
    this.draft.update((draft) => {
      const selected = draft.selected.map((list) => [...list]);
      const current = selected[questionIndex] ?? [];
      if (!multiple) {
        selected[questionIndex] = checked ? [label] : [];
      } else if (checked) {
        selected[questionIndex] = current.includes(label) ? current : [...current, label];
      } else {
        selected[questionIndex] = current.filter((value) => value !== label);
      }
      return { ...draft, selected };
    });
  }

  setText(questionIndex: number, value: string): void {
    this.draft.update((draft) => {
      const text = [...draft.text];
      text[questionIndex] = value;
      return { ...draft, text };
    });
  }

  /** Focus trap + Escape (= Skip), keeping the prompt keyboard-complete. */
  onKeydown(event: KeyboardEvent): void {
    if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      this.skip();
      return;
    }
    if (event.key !== 'Tab') {
      return;
    }
    const root = this.dialog()?.nativeElement;
    if (!root) {
      return;
    }
    const focusable = [
      ...root.querySelectorAll<HTMLElement>('input:not([disabled]), button:not([disabled])'),
    ].filter((el) => el.offsetParent !== null);
    if (focusable.length === 0) {
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const active = document.activeElement as HTMLElement | null;
    if (event.shiftKey && (active === first || !root.contains(active))) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  }

  submit(): void {
    if (!this.canSubmit()) {
      return;
    }
    void this.resolve(false);
  }

  skip(): void {
    void this.resolve(true);
  }

  private buildAnswers(item: PendingQuestion): Record<string, string | string[]> {
    const draft = this.draft();
    const answers: Record<string, string | string[]> = {};
    item.questions.forEach((q, index) => {
      const selected = [...(draft.selected[index] ?? [])];
      const text = (draft.text[index] ?? '').trim();
      // Engine contract: `{ <id>: string | string[] }`. A free-text answer
      // replaces the selection; a multi-select stays an array.
      if (text) {
        answers[q.id] = text;
      } else if (q.multiple) {
        answers[q.id] = selected;
      } else {
        answers[q.id] = selected[0] ?? '';
      }
    });
    return answers;
  }

  private async resolve(skipped: boolean): Promise<void> {
    const item = this.current();
    if (!item || item.busy) {
      return;
    }
    const answers = skipped ? null : this.buildAnswers(item);
    this.asks.update((list) =>
      list.map((a) => (a.requestID === item.requestID ? { ...a, busy: true, error: undefined } : a)),
    );
    try {
      await this.engine.answerQuestion(item.sessionID, item.requestID, answers ?? {}, skipped);
      this.resolvedIds.add(item.requestID);
      this.asks.update((list) => list.filter((a) => a.requestID !== item.requestID));
      this.answered.emit({
        requestID: item.requestID,
        sessionID: item.sessionID,
        questions: item.questions,
        answers,
      });
    } catch (err) {
      console.error('question answer failed', err);
      // Skip is a local dismissal: the user explicitly asked to move on, so
      // the dialog must close even when the POST failed (e.g. the engine
      // restarted and the pending question is gone — the turn is dead anyway
      // and keeping the dialog would block the composer forever). The same
      // applies to a submit that hits a question the engine no longer knows.
      const message = err instanceof Error ? err.message : String(err);
      const gone = message.includes('-> 404') || message.includes('no pending question');
      if (skipped || gone) {
        this.resolvedIds.add(item.requestID);
        this.asks.update((list) => list.filter((a) => a.requestID !== item.requestID));
        this.answered.emit({
          requestID: item.requestID,
          sessionID: item.sessionID,
          questions: item.questions,
          answers,
        });
        return;
      }
      this.asks.update((list) =>
        list.map((a) =>
          a.requestID === item.requestID
            ? { ...a, busy: false, error: this.t('question.failed') }
            : a,
        ),
      );
    }
  }

  private async resync(sessionID: string): Promise<void> {
    let pending: QuestionAsked[];
    try {
      pending = await this.engine.pendingQuestions(sessionID);
    } catch {
      return;
    }
    this.asks.update((list) => {
      const known = new Set(list.map((a) => a.requestID));
      const fresh = pending
        .filter((p) => !this.resolvedIds.has(p.requestID) && !known.has(p.requestID))
        .map((p) => ({
          requestID: p.requestID,
          sessionID,
          messageIndex: p.messageIndex,
          questions: p.questions,
          askedAt: Date.now(),
          busy: false,
        }));
      return fresh.length > 0 ? [...list, ...fresh] : list;
    });
  }

  private handle(event: EngineEvent): void {
    const active = this.activeSessionID();
    const dir = this.directory();
    const matches =
      (!!active && event.sessionID === active) || (!!dir && event.directory === dir);
    if (!matches) {
      return;
    }
    if (event.type === 'question.asked') {
      const asked = parseQuestionAsked(event.properties);
      if (!asked || this.resolvedIds.has(asked.requestID)) {
        return;
      }
      this.asks.update((list) => {
        if (list.some((a) => a.requestID === asked.requestID)) {
          return list;
        }
        return [
          ...list,
          {
            requestID: asked.requestID,
            sessionID: event.sessionID,
            messageIndex: asked.messageIndex,
            questions: asked.questions,
            askedAt: Date.now(),
            busy: false,
          },
        ];
      });
    } else if (event.type === 'question.resolved') {
      const requestID = event.properties?.['requestID'];
      if (typeof requestID === 'string') {
        this.resolvedIds.add(requestID);
        this.asks.update((list) => list.filter((a) => a.requestID !== requestID));
      }
    }
  }
}
