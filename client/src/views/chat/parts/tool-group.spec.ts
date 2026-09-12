/**
 * F6-1 / F6-1c: `ToolGroupComponent` and `summarizeToolRun`, the shared
 * collapsible "N tool calls · read ×2, edit" summary row used both for a run
 * of tool calls inside one message (`message-row.ts`) and for a merged run
 * of tool-only messages (`tool-run-row.ts`).
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { Part } from '../../../core/engine.dtos';
import { GroupRow, RenderedPart, ToolGroupComponent, summarizeToolRun } from './tool-group';

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

function row(part: Part, toolIndex = 0): RenderedPart {
  return { kind: 'part', part, toolIndex };
}

describe('summarizeToolRun (F6-1 / F6-1c)', () => {
  it('is completed with an empty name summary for no rows', () => {
    expect(summarizeToolRun([])).toEqual({ state: 'completed', names: '' });
  });

  it('counts repeated names in first-appearance order', () => {
    const summary = summarizeToolRun([row(tool('read')), row(tool('read')), row(tool('grep'))]);
    expect(summary.names).toBe('read ×2, grep');
    expect(summary.state).toBe('completed');
  });

  it('reports the worst state: error beats running beats completed', () => {
    expect(summarizeToolRun([row(tool('a')), row(tool('b', 'running'))]).state).toBe('running');
    expect(
      summarizeToolRun([row(tool('a', 'running')), row(tool('b', 'error')), row(tool('c'))]).state,
    ).toBe('error');
  });

  it('truncates the name list beyond 4 entries with an ellipsis', () => {
    const summary = summarizeToolRun(
      ['a', 'b', 'c', 'd', 'e'].map((n) => row(tool(n))),
    );
    expect(summary.names).toBe('a, b, c, d, …');
  });
});

describe('ToolGroupComponent (F6-1 / F6-1c)', () => {
  let fixture: ComponentFixture<ToolGroupComponent>;

  function root(): HTMLElement {
    return fixture.nativeElement as HTMLElement;
  }

  beforeEach(() => {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [ToolGroupComponent],
      providers: [provideZonelessChangeDetection(), provideRouter([])],
    });
    fixture = TestBed.createComponent(ToolGroupComponent);
  });

  function setInputs(rows: GroupRow[], open: boolean): void {
    fixture.componentRef.setInput('rows', rows);
    fixture.componentRef.setInput('state', 'completed');
    fixture.componentRef.setInput('names', 'read ×2, edit');
    fixture.componentRef.setInput('count', 3);
    fixture.componentRef.setInput('open', open);
  }

  it('renders the collapsed summary button with the given count/names, closed', async () => {
    setInputs([row(tool('read')), row(tool('edit'))], false);
    await fixture.whenStable();

    const button = root().querySelector<HTMLButtonElement>('.group-head');
    expect(button).not.toBeNull();
    expect(button!.getAttribute('type')).toBe('button');
    expect(button!.getAttribute('aria-expanded')).toBe('false');
    expect(button!.textContent).toContain('3 tool calls');
    expect(button!.textContent).toContain('read ×2, edit');
    expect(root().querySelector('.group-body')).toBeNull();
  });

  it('emits (toggle) on click without owning the open state itself', async () => {
    setInputs([row(tool('read'))], false);
    await fixture.whenStable();
    let toggled = 0;
    fixture.componentInstance.toggle.subscribe(() => toggled++);

    root().querySelector<HTMLButtonElement>('.group-head')!.click();
    await fixture.whenStable();

    expect(toggled).toBe(1);
    // The component is stateless: `open` still reflects the (unchanged) input.
    expect(root().querySelector('.group-head')!.getAttribute('aria-expanded')).toBe('false');
  });

  it('renders each RenderedPart row via app-part-renderer when open', async () => {
    setInputs([row(tool('read')), row(tool('edit'))], true);
    await fixture.whenStable();

    expect(root().querySelectorAll('app-part-renderer').length).toBe(2);
    expect(root().querySelectorAll('.turn-strip').length).toBe(0);
  });

  it('renders a TurnStrip row as a small per-turn usage label, not a part', async () => {
    const rows: GroupRow[] = [
      { kind: 'turn', tokensIn: 12, tokensOut: 340 },
      row(tool('read')),
      { kind: 'turn', tokensIn: 5, tokensOut: 20 },
      row(tool('edit')),
    ];
    setInputs(rows, true);
    await fixture.whenStable();

    const strips = root().querySelectorAll('.turn-strip');
    expect(strips.length).toBe(2);
    expect(strips[0].textContent).toContain('12');
    expect(strips[0].textContent).toContain('340');
    expect(strips[0].classList.contains('first')).toBeTrue();
    expect(strips[1].classList.contains('first')).toBeFalse();
    expect(root().querySelectorAll('app-part-renderer').length).toBe(2);
  });
});
