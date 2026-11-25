/**
 * Question prompt: the modal renders the asked questions, submit posts the
 * selected answers, Escape skips (empty, `skipped: true`), and the dialog
 * reports `blocking()` while a question is open - the chat composer's send lock.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../core/engine-client.service';
import { EngineEvent } from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { QuestionDialog } from './question-dialog';
import { QuestionRecord } from './question-record';
import type { AnsweredQuestion } from './question-dialog';

describe('QuestionDialog', () => {
  let fixture: ComponentFixture<QuestionDialog>;
  let listener: ((event: EngineEvent) => void) | undefined;
  let answerQuestion: jasmine.Spy;

  const questions = [
    {
      header: 'Colour',
      question: 'Pick a colour',
      options: [{ label: 'Red' }, { label: 'Blue', description: 'the cool one' }],
      multiple: false,
      allowFreeText: false,
    },
    {
      header: 'Extras',
      question: 'Which extras?',
      options: [{ label: 'Docs' }, { label: 'Tests' }],
      multiple: true,
      allowFreeText: true,
    },
  ];

  function ask(requestID = 'r1'): void {
    listener?.({
      type: 'question.asked',
      directory: '/project',
      sessionID: 's1',
      properties: { requestID, messageIndex: 2, questions },
    });
  }

  function root(): HTMLElement {
    return fixture.nativeElement as HTMLElement;
  }

  beforeEach(async () => {
    listener = undefined;
    answerQuestion = jasmine.createSpy('answerQuestion').and.resolveTo(undefined);
    await TestBed.configureTestingModule({
      imports: [QuestionDialog],
      providers: [
        provideZonelessChangeDetection(),
        { provide: EngineClient, useValue: { answerQuestion } },
        {
          provide: EventsStore,
          useValue: {
            onEvent: (cb: (event: EngineEvent) => void) => {
              listener = cb;
              return () => {
                listener = undefined;
              };
            },
          },
        },
      ],
    }).compileComponents();

    fixture = TestBed.createComponent(QuestionDialog);
    fixture.componentRef.setInput('activeSessionID', 's1');
    fixture.componentRef.setInput('directory', '/project');
    fixture.detectChanges();
  });

  it('renders the headers, options and free-text field of each question', () => {
    ask();
    fixture.detectChanges();

    const text = root().textContent ?? '';
    expect(text).toContain('Colour');
    expect(text).toContain('Pick a colour');
    expect(text).toContain('the cool one');
    expect(root().querySelectorAll('input[type="radio"]').length).toBe(2);
    expect(root().querySelectorAll('input[type="checkbox"]').length).toBe(2);
    expect(root().querySelector('[data-testid="question-text"]')).not.toBeNull();
    expect(root().querySelector('[role="dialog"][aria-modal="true"]')).not.toBeNull();
  });

  it('keeps the composer blocked until the question is answered', async () => {
    const dialog = fixture.componentInstance;
    expect(dialog.blocking()).toBeFalse();

    ask();
    fixture.detectChanges();
    expect(dialog.blocking()).toBeTrue();

    (root().querySelector('input[type="radio"][value="Blue"]') as HTMLInputElement).click();
    (root().querySelector('input[type="checkbox"][value="Docs"]') as HTMLInputElement).click();
    fixture.detectChanges();
    (root().querySelector('[data-testid="question-submit"]') as HTMLButtonElement).click();
    await fixture.whenStable();

    expect(dialog.blocking()).toBeFalse();
  });

  it('posts the selected answers on submit', async () => {
    ask();
    fixture.detectChanges();

    (root().querySelector('input[type="radio"][value="Blue"]') as HTMLInputElement).click();
    const docs = root().querySelector('input[type="checkbox"][value="Docs"]') as HTMLInputElement;
    docs.click();
    fixture.detectChanges();
    (root().querySelector('[data-testid="question-submit"]') as HTMLButtonElement).click();
    await fixture.whenStable();

    expect(answerQuestion).toHaveBeenCalledWith('s1', 'r1', [
      { questionIndex: 0, selected: ['Blue'] },
      { questionIndex: 1, selected: ['Docs'] },
    ], false);
  });

  it('keeps Submit disabled until every question has an answer', () => {
    ask();
    fixture.detectChanges();
    const submit = root().querySelector('[data-testid="question-submit"]') as HTMLButtonElement;
    expect(submit.disabled).toBeTrue();

    (root().querySelector('input[type="radio"][value="Red"]') as HTMLInputElement).click();
    fixture.detectChanges();
    expect(submit.disabled).toBeTrue();

    (root().querySelector('[data-testid="question-text"]') as HTMLInputElement).value = 'and more';
    (root().querySelector('[data-testid="question-text"]') as HTMLInputElement).dispatchEvent(new Event('input'));
    fixture.detectChanges();
    expect(submit.disabled).toBeFalse();
  });

  it('skips the prompt on Escape', async () => {
    const answered = jasmine.createSpy('answered');
    fixture.componentInstance.answered.subscribe(answered);
    ask();
    fixture.detectChanges();

    root().querySelector('.q')!.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }),
    );
    await fixture.whenStable();

    expect(answerQuestion).toHaveBeenCalledWith('s1', 'r1', [], true);
    expect(fixture.componentInstance.blocking()).toBeFalse();
    const record = answered.calls.mostRecent().args[0] as AnsweredQuestion;
    expect(record.answers).toBeNull();
    expect(record.questions.length).toBe(2);
  });

  it('ignores question events for other sessions', () => {
    listener?.({
      type: 'question.asked',
      directory: '/elsewhere',
      sessionID: 'other',
      properties: { requestID: 'x', messageIndex: 0, questions },
    });
    fixture.detectChanges();
    expect(fixture.componentInstance.blocking()).toBeFalse();
  });
});

describe('QuestionRecord', () => {
  it('marks the picked options and shows free text read-only', async () => {
    TestBed.resetTestingModule();
    await TestBed.configureTestingModule({
      imports: [QuestionRecord],
      providers: [provideZonelessChangeDetection()],
    }).compileComponents();
    const fixture = TestBed.createComponent(QuestionRecord);
    fixture.componentRef.setInput('record', {
      requestID: 'r1',
      sessionID: 's1',
      questions: [
        {
          header: '',
          question: 'Pick',
          options: [{ label: 'Red' }, { label: 'Blue' }],
          multiple: false,
          allowFreeText: true,
        },
      ],
      answers: [{ questionIndex: 0, selected: ['Blue'], text: 'navy' }],
    } satisfies AnsweredQuestion);
    fixture.detectChanges();

    const picked = fixture.nativeElement.querySelectorAll('li.picked') as NodeListOf<HTMLElement>;
    expect(picked.length).toBe(1);
    expect(picked[0].textContent).toContain('Blue');
    expect(fixture.nativeElement.querySelector('[data-testid="question-record-text"]').textContent)
      .toContain('navy');
  });
});
