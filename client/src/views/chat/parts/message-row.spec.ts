/**
 * F6-1: grouping of consecutive tool calls in one message.
 *
 * `groupParts` is the pure grouping pass; the component specs cover the
 * rendered group row (a real `<button>` with `aria-expanded`, so native
 * Enter/Space activation applies) and the "Expand tool calls by default"
 * preference.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { Message, Part } from '../../../core/engine.dtos';
import { UiPrefsStore } from '../../../core/ui-prefs.store';
import { MessageRowComponent, ToolGroup, groupParts } from './message-row';

function tool(name: string, state: 'completed' | 'error' | 'running' = 'completed'): Part {
  switch (state) {
    case 'error':
      return { type: 'tool', id: `${name}-${Math.random()}`, name, state: { state, input: {}, error: 'boom' } };
    case 'running':
      return { type: 'tool', id: `${name}-${Math.random()}`, name, state: { state, input: {}, started_at: 1 } };
    default:
      return {
        type: 'tool',
        id: `${name}-${Math.random()}`,
        name,
        state: { state: 'completed', input: { path: 'src/a.ts' }, output: 'ok', title: name },
      };
  }
}

function text(value: string): Part {
  return { type: 'text', text: value };
}

function thinking(value: string): Part {
  return { type: 'thinking', text: value };
}

describe('groupParts (F6-1)', () => {
  it('leaves a message without tool parts untouched', () => {
    const items = groupParts([text('a'), thinking('b')]);
    expect(items.map((i) => i.kind)).toEqual(['part', 'part']);
    expect(items.every((i) => i.kind === 'part' && i.toolIndex === -1)).toBeTrue();
  });

  it('renders a lone tool call ungrouped with toolIndex 0', () => {
    const items = groupParts([text('a'), tool('read'), text('b')]);
    expect(items.map((i) => i.kind)).toEqual(['part', 'part', 'part']);
    expect(items[1]).toEqual(jasmine.objectContaining({ kind: 'part', toolIndex: 0 }));
  });

  it('folds exactly two consecutive tool calls into one group', () => {
    const items = groupParts([tool('read'), tool('edit')]);
    expect(items.length).toBe(1);
    const group = items[0] as ToolGroup;
    expect(group.kind).toBe('group');
    expect(group.key).toBe(0);
    expect(group.rows.map((r) => r.toolIndex)).toEqual([0, 1]);
    expect(group.names).toBe('read, edit');
    expect(group.state).toBe('completed');
  });

  it('keeps per-message tool ordinals across groups, lone calls and other parts', () => {
    const items = groupParts([
      text('intro'),
      tool('read'),
      tool('read'),
      tool('grep'),
      thinking('hmm'),
      tool('edit'),
      text('done'),
      tool('bash'),
      tool('bash'),
    ]);
    expect(items.map((i) => i.kind)).toEqual(['part', 'group', 'part', 'part', 'part', 'group']);

    const first = items[1] as ToolGroup;
    expect(first.key).toBe(1);
    expect(first.rows.map((r) => r.toolIndex)).toEqual([0, 1, 2]);
    expect(first.names).toBe('read ×2, grep');

    // The lone call after the thinking part continues the ordinal (collapsed by default).
    expect(items[3]).toEqual(jasmine.objectContaining({ kind: 'part', toolIndex: 3 }));

    const last = items[5] as ToolGroup;
    expect(last.key).toBe(7);
    expect(last.rows.map((r) => r.toolIndex)).toEqual([4, 5]);
    expect(last.names).toBe('bash ×2');
  });

  it('reports the worst state of the run and truncates long name lists', () => {
    const running = groupParts([tool('a'), tool('b', 'running')])[0] as ToolGroup;
    expect(running.state).toBe('running');

    const failed = groupParts([tool('a', 'running'), tool('b', 'error'), tool('c')])[0] as ToolGroup;
    expect(failed.state).toBe('error');

    const many = groupParts([tool('a'), tool('b'), tool('c'), tool('d'), tool('e')])[0] as ToolGroup;
    expect(many.names).toBe('a, b, c, d, …');
  });
});

describe('MessageRowComponent tool groups (F6-1)', () => {
  let fixture: ComponentFixture<MessageRowComponent>;
  let prefs: UiPrefsStore;

  function message(parts: Part[]): Message {
    return { id: 'assistant-1', role: 'assistant', parts };
  }

  function root(): HTMLElement {
    return fixture.nativeElement as HTMLElement;
  }

  function groupButton(): HTMLButtonElement | null {
    return root().querySelector<HTMLButtonElement>('.group-head');
  }

  function toolHeads(): HTMLButtonElement[] {
    return [...root().querySelectorAll<HTMLButtonElement>('.tool-head')];
  }

  beforeEach(() => {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [MessageRowComponent],
      providers: [provideZonelessChangeDetection(), provideRouter([])],
    });
    prefs = TestBed.inject(UiPrefsStore);
    prefs.setExpandToolCallsByDefault(false);
    fixture = TestBed.createComponent(MessageRowComponent);
  });

  afterEach(() => {
    prefs.setExpandToolCallsByDefault(false);
  });

  it('renders a run of tool calls as one collapsed summary button', async () => {
    fixture.componentRef.setInput('message', message([text('x'), tool('read'), tool('edit'), tool('read')]));
    await fixture.whenStable();

    const button = groupButton();
    expect(button).not.toBeNull();
    expect(button!.tagName).toBe('BUTTON');
    expect(button!.getAttribute('type')).toBe('button');
    expect(button!.getAttribute('aria-expanded')).toBe('false');
    expect(button!.textContent).toContain('3 tool calls');
    expect(button!.textContent).toContain('read ×2, edit');
    expect(root().querySelectorAll('app-tool-part').length).toBe(0);
  });

  it('expands on activation to reveal the individual tool rows, first one open', async () => {
    fixture.componentRef.setInput('message', message([tool('read'), tool('edit')]));
    await fixture.whenStable();

    // A real <button> receives native Enter/Space activation as a click.
    groupButton()!.click();
    await fixture.whenStable();

    expect(groupButton()!.getAttribute('aria-expanded')).toBe('true');
    const heads = toolHeads();
    expect(heads.length).toBe(2);
    expect(heads[0].getAttribute('aria-expanded')).toBe('true');
    expect(heads[1].getAttribute('aria-expanded')).toBe('false');

    groupButton()!.click();
    await fixture.whenStable();
    expect(groupButton()!.getAttribute('aria-expanded')).toBe('false');
  });

  it('does not group a lone tool call', async () => {
    fixture.componentRef.setInput('message', message([text('a'), tool('read'), text('b')]));
    await fixture.whenStable();

    expect(groupButton()).toBeNull();
    expect(root().querySelectorAll('app-tool-part').length).toBe(1);
  });

  it('starts groups and every tool call expanded when the preference is on', async () => {
    prefs.setExpandToolCallsByDefault(true);
    fixture.componentRef.setInput('message', message([tool('read'), tool('edit'), text('x'), tool('grep')]));
    await fixture.whenStable();

    expect(groupButton()!.getAttribute('aria-expanded')).toBe('true');
    const heads = toolHeads();
    expect(heads.length).toBe(3);
    heads.forEach((head) => expect(head.getAttribute('aria-expanded')).toBe('true'));
  });

  it('applies a preference change to already rendered groups', async () => {
    fixture.componentRef.setInput('message', message([tool('read'), tool('edit')]));
    await fixture.whenStable();
    expect(groupButton()!.getAttribute('aria-expanded')).toBe('false');

    prefs.setExpandToolCallsByDefault(true);
    await fixture.whenStable();
    expect(groupButton()!.getAttribute('aria-expanded')).toBe('true');
  });
});
