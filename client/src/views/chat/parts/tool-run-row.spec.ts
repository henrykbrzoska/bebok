/**
 * F6-1c: merging a run of consecutive tool-only assistant turns into one
 * `<app-tool-run-row>` - one header, one combined tool-group summary, one
 * usage line with the summed tokens, and per-turn token labels once expanded.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { Message, Part } from '../../../core/engine.dtos';
import { UiPrefsStore } from '../../../core/ui-prefs.store';
import { ToolRunRowComponent, buildToolRun } from './tool-run-row';

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
        state: { state: 'completed', input: {}, output: 'ok', title: name },
      };
  }
}

function thinking(text = 'hmm'): Part {
  return { type: 'thinking', text };
}

function usage(input_tokens: number, output_tokens: number, cost: number | null = null): Part {
  return { type: 'usage', input_tokens, output_tokens, ...(cost !== null ? { cost } : {}) };
}

function turn(id: string, parts: Part[], meta?: Message['meta']): Message {
  return { id, role: 'assistant', parts, meta };
}

describe('buildToolRun (F6-1c)', () => {
  it('sums tokens/cost across every merged turn', () => {
    const run = buildToolRun([
      turn('a', [tool('read'), usage(10, 20, 0.01)]),
      turn('b', [tool('edit'), usage(5, 7, 0.002)]),
    ]);
    expect(run.tokensIn).toBe(15);
    expect(run.tokensOut).toBe(27);
    expect(run.cost).toBeCloseTo(0.012);
    expect(run.key).toBe('a');
  });

  it('is null cost when no turn reports one', () => {
    const run = buildToolRun([turn('a', [tool('read'), usage(1, 1)]), turn('b', [tool('edit'), usage(1, 1)])]);
    expect(run.cost).toBeNull();
  });

  it('counts tool calls and summarizes names across all turns, not just one', () => {
    const run = buildToolRun([
      turn('a', [tool('read'), tool('read')]),
      turn('b', [tool('bash')]),
    ]);
    expect(run.count).toBe(3);
    expect(run.names).toBe('read ×2, bash');
    expect(run.state).toBe('completed');
  });

  it('reports the worst state across all merged turns', () => {
    const run = buildToolRun([
      turn('a', [tool('read')]),
      turn('b', [tool('edit', 'error')]),
    ]);
    expect(run.state).toBe('error');
  });

  it('emits one TurnStrip per turn, first, ahead of that turn\'s own rows, and drops usage parts from the rows', () => {
    const run = buildToolRun([
      turn('a', [tool('read'), usage(10, 20)]),
      turn('b', [thinking(), tool('edit'), usage(5, 7)]),
    ]);
    // turn strip, read -> turn strip, thinking, edit
    expect(run.rows.map((r) => r.kind)).toEqual(['turn', 'part', 'turn', 'part', 'part']);
    expect(run.rows[0]).toEqual({ kind: 'turn', tokensIn: 10, tokensOut: 20 });
    expect(run.rows[2]).toEqual({ kind: 'turn', tokensIn: 5, tokensOut: 7 });
    expect(run.rows.some((r) => r.kind === 'part' && r.part.type === 'usage')).toBeFalse();
  });

  it('numbers tool calls continuously across the whole run', () => {
    const run = buildToolRun([turn('a', [tool('read'), tool('grep')]), turn('b', [tool('edit')])]);
    const toolRows = run.rows.filter((r) => r.kind === 'part' && r.part.type === 'tool');
    expect(toolRows.map((r) => (r as { toolIndex: number }).toolIndex)).toEqual([0, 1, 2]);
  });
});

describe('ToolRunRowComponent (F6-1c)', () => {
  let fixture: ComponentFixture<ToolRunRowComponent>;
  let prefs: UiPrefsStore;

  function root(): HTMLElement {
    return fixture.nativeElement as HTMLElement;
  }

  beforeEach(() => {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [ToolRunRowComponent],
      providers: [provideZonelessChangeDetection(), provideRouter([])],
    });
    prefs = TestBed.inject(UiPrefsStore);
    prefs.setExpandToolCallsByDefault(false);
    fixture = TestBed.createComponent(ToolRunRowComponent);
  });

  afterEach(() => {
    prefs.setExpandToolCallsByDefault(false);
  });

  it('shows one header from the first turn\'s agent/model/timestamp', async () => {
    fixture.componentRef.setInput('messages', [
      turn('a', [tool('read')], { agent: 'code', model: 'sonnet', created_at: 1_700_000_000_000 }),
      turn('b', [tool('edit')], { agent: 'other', model: 'other-model' }),
    ]);
    await fixture.whenStable();

    expect(root().querySelectorAll('.label-row').length).toBe(1);
    expect(root().querySelector('.model')!.textContent).toContain('code');
    expect(root().querySelector('.model')!.textContent).toContain('sonnet');
    expect(root().querySelector('.time')).not.toBeNull();
  });

  it('renders exactly one collapsed group summing every turn\'s tool calls', async () => {
    fixture.componentRef.setInput('messages', [
      turn('a', [tool('read'), usage(10, 20)]),
      turn('b', [tool('edit'), tool('bash'), usage(5, 7)]),
    ]);
    await fixture.whenStable();

    expect(root().querySelectorAll('app-tool-group').length).toBe(1);
    const groupButton = root().querySelector<HTMLButtonElement>('.group-head');
    expect(groupButton!.textContent).toContain('3 tool calls');
    expect(groupButton!.getAttribute('aria-expanded')).toBe('false');
  });

  it('shows one usage line with the summed tokens', async () => {
    fixture.componentRef.setInput('messages', [
      turn('a', [tool('read'), usage(10, 20)]),
      turn('b', [tool('edit'), usage(5, 7)]),
    ]);
    await fixture.whenStable();

    const usageLines = root().querySelectorAll('.usage');
    expect(usageLines.length).toBe(1);
    expect(usageLines[0].textContent).toContain('15');
    expect(usageLines[0].textContent).toContain('27');
  });

  it('expanding the group reveals per-call rows in order with per-turn labels, still collapsed individually', async () => {
    fixture.componentRef.setInput('messages', [
      turn('a', [tool('read'), usage(10, 20)]),
      turn('b', [tool('edit'), usage(5, 7)]),
    ]);
    await fixture.whenStable();

    root().querySelector<HTMLButtonElement>('.group-head')!.click();
    await fixture.whenStable();

    expect(root().querySelector<HTMLButtonElement>('.group-head')!.getAttribute('aria-expanded')).toBe('true');
    const toolHeads = root().querySelectorAll<HTMLButtonElement>('.tool-head');
    expect(toolHeads.length).toBe(2);
    toolHeads.forEach((head) => expect(head.getAttribute('aria-expanded')).toBe('false'));
    const strips = root().querySelectorAll('.turn-strip');
    expect(strips.length).toBe(2);
    expect(strips[0].textContent).toContain('10');
    expect(strips[1].textContent).toContain('5');
  });

  it('starts expanded when the "expand tool calls by default" preference is on', async () => {
    prefs.setExpandToolCallsByDefault(true);
    fixture.componentRef.setInput('messages', [turn('a', [tool('read')]), turn('b', [tool('edit')])]);
    await fixture.whenStable();

    expect(root().querySelector('.group-head')!.getAttribute('aria-expanded')).toBe('true');
  });

  it('keeps a manual expand/collapse override stable while `messages` grows (streaming into the same run)', async () => {
    fixture.componentRef.setInput('messages', [turn('a', [tool('read')]), turn('b', [tool('edit')])]);
    await fixture.whenStable();

    root().querySelector<HTMLButtonElement>('.group-head')!.click();
    await fixture.whenStable();
    expect(root().querySelector('.group-head')!.getAttribute('aria-expanded')).toBe('true');

    // A new turn streams into the same run: the identity (first message id)
    // is unchanged, so the user's "opened" choice must survive.
    fixture.componentRef.setInput('messages', [
      turn('a', [tool('read')]),
      turn('b', [tool('edit')]),
      turn('c', [tool('bash')]),
    ]);
    await fixture.whenStable();
    expect(root().querySelector('.group-head')!.getAttribute('aria-expanded')).toBe('true');
  });

  it('resets the override when a different run takes the same component (new first-message id)', async () => {
    fixture.componentRef.setInput('messages', [turn('a', [tool('read')]), turn('b', [tool('edit')])]);
    await fixture.whenStable();
    root().querySelector<HTMLButtonElement>('.group-head')!.click();
    await fixture.whenStable();
    expect(root().querySelector('.group-head')!.getAttribute('aria-expanded')).toBe('true');

    fixture.componentRef.setInput('messages', [turn('x', [tool('read')]), turn('y', [tool('edit')])]);
    await fixture.whenStable();
    expect(root().querySelector('.group-head')!.getAttribute('aria-expanded')).toBe('false');
  });
});
