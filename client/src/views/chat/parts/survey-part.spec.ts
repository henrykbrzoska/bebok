import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { Part } from '../../../core/engine.dtos';
import { SurveyBridge } from '../survey-bridge';
import { SurveyPartComponent, formatAnswer, surveyOf } from './survey-part';

function askUserPart(id = 'call-1'): Part {
  return {
    type: 'tool',
    id,
    name: 'ask_user',
    state: {
      state: 'completed',
      input: {},
      output: 'Questionnaire shown to the user.',
      title: 'ask_user',
      structured: {
        awaitUser: true,
        survey: {
          title: 'New user form',
          questions: [
            {
              number: 1,
              text: 'Which fields?',
              multi: true,
              options: ['name', 'email', 'phone'],
              allowCustom: true,
            },
            {
              number: 2,
              text: 'Auth?',
              multi: false,
              options: ['password', 'magic link'],
              allowCustom: false,
            },
          ],
        },
      } as never,
    },
  } as Part;
}

describe('SurveyPartComponent (ask_user, 1.8)', () => {
  let fixture: ComponentFixture<SurveyPartComponent>;
  let bridge: SurveyBridge;
  let sent: string[];

  beforeEach(() => {
    localStorage.clear();
    sent = [];
    TestBed.configureTestingModule({
      imports: [SurveyPartComponent],
      providers: [provideZonelessChangeDetection()],
    });
    bridge = TestBed.inject(SurveyBridge);
    bridge.attach(async (text) => {
      sent.push(text);
    });
    fixture = TestBed.createComponent(SurveyPartComponent);
    fixture.componentRef.setInput('part', askUserPart());
    fixture.detectChanges();
  });

  it('recognises a completed ask_user call and renders numbered, lettered questions', () => {
    expect(surveyOf(askUserPart())).not.toBeNull();
    expect(surveyOf({ type: 'text', text: 'x' } as Part)).toBeNull();
    const el: HTMLElement = fixture.nativeElement;
    expect(el.querySelectorAll('fieldset.question').length).toBe(2);
    expect(el.querySelector('[data-question="1"] .question-no')?.textContent).toBe('1');
    // Question 1 (multi): checkboxes + the free-text slot lettered D.
    expect(el.querySelectorAll('[data-question="1"] input[type=checkbox]').length).toBe(3);
    expect(el.querySelector('[data-question="1"] .option.custom .letter')?.textContent).toBe('D');
    // Question 2 (single, no custom): radios only.
    expect(el.querySelectorAll('[data-question="2"] input[type=radio]').length).toBe(2);
    expect(el.querySelector('[data-question="2"] .option.custom')).toBeNull();
  });

  it('formats answers as <number><letters> with the custom slot in parentheses', () => {
    const q = { number: 3, text: 'x', multi: true, options: ['a', 'b'], allowCustom: true };
    expect(formatAnswer(q, { picked: new Set([1, 0]), custom: '' })).toBe('3AB');
    expect(formatAnswer(q, { picked: new Set(), custom: 'id, role' })).toBe('3C(id, role)');
    expect(formatAnswer(q, { picked: new Set([0]), custom: 'age' })).toBe('3AC(age)');
  });

  it('sends only once every question is answered, then locks the card', async () => {
    const c = fixture.componentInstance;
    const el: HTMLElement = fixture.nativeElement;
    const send = () => el.querySelector<HTMLButtonElement>('[data-testid=survey-send]')!;
    expect(send().disabled).toBeTrue();

    c.pick(c.survey().questions[0], 1);
    c.pick(c.survey().questions[0], 2);
    fixture.detectChanges();
    expect(send().disabled).toBeTrue();

    c.pick(c.survey().questions[1], 0);
    c.pick(c.survey().questions[1], 1); // radio: replaces
    fixture.detectChanges();
    expect(send().disabled).toBeFalse();
    expect(c.preview()).toBe('1BC, 2B');

    await c.send();
    fixture.detectChanges();
    expect(sent).toEqual(['1BC, 2B']);
    expect(el.querySelector('[data-testid=survey-answered]')?.textContent).toContain('1BC, 2B');
    expect(el.querySelector('[data-testid=survey-send]')).toBeNull();
    expect(localStorage.getItem('bebok.survey.answered.call-1')).toBe('1BC, 2B');
  });
});
