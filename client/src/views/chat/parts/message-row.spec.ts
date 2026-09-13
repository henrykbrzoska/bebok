/**
 * F6-1 / F6-1b: grouping of consecutive tool calls in one message, and the
 * three-level collapse (group -> per-call row -> arguments/output).
 *
 * `groupParts` is the pure grouping pass; the component specs cover the
 * rendered group row (a real `<button>` with `aria-expanded`, so native
 * Enter/Space activation applies), the "Expand tool calls by default"
 * preference, and that no level auto-expands a level below it.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { Message, Part } from '../../../core/engine.dtos';
import { UiPrefsStore } from '../../../core/ui-prefs.store';
import { MessageRowComponent, RenderedPart, ToolGroup, groupParts } from './message-row';

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

function status(text: string, kind = 'task.progress'): Part {
  return { type: 'status', kind, text, at: 1 };
}

function task(model: string): Part {
  return {
    type: 'tool',
    id: `task-${Math.random()}`,
    name: 'task',
    state: {
      state: 'completed',
      input: { agent: 'code' },
      output: 'done',
      title: 'task',
      structured: { taskID: 't1', name: 'api', agent: 'code', childSessionID: 'c1', model },
    },
  };
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

  it('F9-7: a status part does not split a run; statuses are emitted right after the group', () => {
    const items = groupParts([
      tool('task'),
      status('api-orders started (code · openai/gpt-5.6-luna)', 'task.started'),
      tool('task_wait'),
      status('api-orders finished in 4m20s', 'task.ended'),
      text('done'),
    ]);
    expect(items.map((i) => i.kind)).toEqual(['group', 'part', 'part', 'part']);
    const group = items[0] as ToolGroup;
    expect(group.rows.map((r) => r.part.type)).toEqual(['tool', 'tool']);
    expect(group.names).toBe('task, task_wait');
    expect((items[1] as RenderedPart).part).toEqual(jasmine.objectContaining({ type: 'status', kind: 'task.started' }));
    expect((items[2] as RenderedPart).part).toEqual(jasmine.objectContaining({ type: 'status', kind: 'task.ended' }));
    expect((items[3] as RenderedPart).part.type).toBe('text');
  });

  it('F9-7: a status part outside any run stays in place, and a lone call stays ungrouped', () => {
    const items = groupParts([status('x'), tool('read'), status('y'), text('z')]);
    expect(items.map((i) => i.kind)).toEqual(['part', 'part', 'part', 'part']);
    expect((items[0] as RenderedPart).part.type).toBe('status');
    expect((items[1] as RenderedPart).part.type).toBe('tool');
    expect((items[1] as RenderedPart).toolIndex).toBe(0);
    expect((items[2] as RenderedPart).part.type).toBe('status');
  });

  it('F9-9: collects child models that differ from the session model', () => {
    const group = groupParts(
      [task('openai/gpt-5.6-mini'), task('openai/gpt-5.6-luna'), task('openai/gpt-5.6-mini')],
      undefined,
      'openai/gpt-5.6-luna',
    )[0] as ToolGroup;
    expect(group.childModels).toEqual(['openai/gpt-5.6-mini']);
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
    return [...root().querySelectorAll<HTMLButtonElement>('.head-toggle')];
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

  it('expands on activation to reveal the individual tool rows - all still collapsed (three-level expand, F6-1b)', async () => {
    fixture.componentRef.setInput('message', message([tool('read'), tool('edit')]));
    await fixture.whenStable();

    // Level 1 -> 2: A real <button> receives native Enter/Space activation as a click.
    groupButton()!.click();
    await fixture.whenStable();

    expect(groupButton()!.getAttribute('aria-expanded')).toBe('true');
    const heads = toolHeads();
    expect(heads.length).toBe(2);
    // Opening the group must not auto-expand any child row (no "first call" exception).
    expect(heads[0].getAttribute('aria-expanded')).toBe('false');
    expect(heads[1].getAttribute('aria-expanded')).toBe('false');
    expect(root().querySelectorAll('.tool-details').length).toBe(0);

    // Level 2 -> 3: each row expands independently of its sibling.
    heads[0].click();
    await fixture.whenStable();
    expect(toolHeads()[0].getAttribute('aria-expanded')).toBe('true');
    expect(toolHeads()[1].getAttribute('aria-expanded')).toBe('false');
    expect(root().querySelectorAll('.tool-details').length).toBe(1);

    groupButton()!.click();
    await fixture.whenStable();
    expect(groupButton()!.getAttribute('aria-expanded')).toBe('false');
  });

  it('does not group a lone tool call, and it starts collapsed like any other call (F6-1b)', async () => {
    fixture.componentRef.setInput('message', message([text('a'), tool('read'), text('b')]));
    await fixture.whenStable();

    expect(groupButton()).toBeNull();
    const parts = root().querySelectorAll('app-tool-part');
    expect(parts.length).toBe(1);
    const head = toolHeads()[0];
    expect(head.getAttribute('aria-expanded')).toBe('false');
    expect(head.classList.contains('collapsed')).toBeTrue();
    expect(root().querySelector('.tool-details')).toBeNull();
  });

  it('starts groups and every tool call expanded when the preference is on', async () => {
    prefs.setExpandToolCallsByDefault(true);
    fixture.componentRef.setInput('message', message([tool('read'), tool('edit'), text('x'), tool('grep')]));
    await fixture.whenStable();

    expect(groupButton()!.getAttribute('aria-expanded')).toBe('true');
    const heads = toolHeads();
    expect(heads.length).toBe(3);
    heads.forEach((head) => expect(head.getAttribute('aria-expanded')).toBe('true'));
    // The lone call outside the group is expanded too - the pref applies everywhere.
    expect(root().querySelectorAll('.tool-details').length).toBe(3);
  });

  it('applies a preference change to already rendered groups', async () => {
    fixture.componentRef.setInput('message', message([tool('read'), tool('edit')]));
    await fixture.whenStable();
    expect(groupButton()!.getAttribute('aria-expanded')).toBe('false');

    prefs.setExpandToolCallsByDefault(true);
    await fixture.whenStable();
    expect(groupButton()!.getAttribute('aria-expanded')).toBe('true');
  });

  it('F9-7: renders the status rows after the group, not inside it', async () => {
    fixture.componentRef.setInput(
      'message',
      message([tool('task'), status('api started', 'task.started'), tool('task_wait')]),
    );
    await fixture.whenStable();

    expect(groupButton()).not.toBeNull();
    const rows = root().querySelectorAll('[data-testid="status-part"]');
    expect(rows.length).toBe(1);
    // The status row is a sibling after the group, not a child of it.
    expect(rows[0].closest('.tool-group')).toBeNull();
    expect(groupButton()!.textContent).toContain('task, task_wait');
  });

  it('F9-9: shows a child model badge in the group header when it differs from the session model', async () => {
    fixture.componentRef.setInput('sessionModel', 'openai/gpt-5.6-luna');
    fixture.componentRef.setInput('message', message([task('openai/gpt-5.6-mini'), tool('task_wait')]));
    await fixture.whenStable();

    const badge = root().querySelector('[data-testid="group-child-model"]');
    expect(badge).not.toBeNull();
    expect(badge!.textContent!.trim()).toBe('gpt-5.6-mini');

    fixture.componentRef.setInput('sessionModel', 'openai/gpt-5.6-mini');
    await fixture.whenStable();
    expect(root().querySelector('[data-testid="group-child-model"]')).toBeNull();
  });

  it('separates the group count and the tool-name summary with a "·"', async () => {
    fixture.componentRef.setInput('message', message([tool('read'), tool('edit')]));
    await fixture.whenStable();

    expect(groupButton()!.querySelector('.group-sep')!.textContent!.trim()).toBe('·');
  });
});
