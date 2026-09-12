/**
 * F7-5: sortable breakdown table.
 *
 * - default sort is the first numeric column, descending;
 * - clicking a header sorts by it, a second click flips the direction and
 *   `aria-sort` follows;
 * - cells render numbers grouped, unknown costs as "—", model ids in
 *   monospace and empty text through the column's fallback key.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { StatsBucket } from '../../core/engine.dtos';
import { StatsColumn, StatsTable, formatNumber } from './stats-table';

function bucket(key: string, input: number, cost: number | null): StatsBucket {
  return {
    key,
    sessions: 1,
    turns: 1,
    llm_calls: 1,
    tool_calls: 0,
    input_tokens: input,
    output_tokens: 0,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    cost,
    cost_unknown_calls: cost === null ? 1 : 0,
  };
}

const ROWS: StatsBucket[] = [
  bucket('openai/gpt-x', 1500, null),
  bucket('anthropic/claude-sonnet', 12_345, 0.25),
  bucket('', 20, 0),
];

const COLUMNS: StatsColumn<StatsBucket>[] = [
  { key: 'key', labelKey: 'stats.colModel', kind: 'mono', emptyKey: 'stats.untitled' },
  { key: 'input_tokens', labelKey: 'stats.colIn', kind: 'number' },
  { key: 'cost', labelKey: 'stats.colCost', kind: 'cost' },
];

describe('StatsTable (F7-5)', () => {
  let fixture: ComponentFixture<StatsTable<StatsBucket>>;

  beforeEach(async () => {
    TestBed.configureTestingModule({
      imports: [StatsTable],
      providers: [provideZonelessChangeDetection()],
    });
    fixture = TestBed.createComponent(StatsTable<StatsBucket>);
    fixture.componentRef.setInput('rows', ROWS);
    fixture.componentRef.setInput('columns', COLUMNS);
    await fixture.whenStable();
  });

  function firstColumnCells(): string[] {
    const cells = fixture.nativeElement.querySelectorAll('tbody tr td:first-child');
    return Array.from(cells as NodeListOf<HTMLElement>).map((td) => td.textContent!.trim());
  }

  function header(index: number): HTMLElement {
    return fixture.nativeElement.querySelectorAll('thead th')[index] as HTMLElement;
  }

  it('sorts by the first numeric column, descending, by default', () => {
    expect(fixture.componentInstance.sort()).toEqual({ key: 'input_tokens', dir: 'desc' });
    expect(firstColumnCells()).toEqual([
      'anthropic/claude-sonnet',
      'openai/gpt-x',
      'Untitled session',
    ]);
    expect(header(1).getAttribute('aria-sort')).toBe('descending');
    expect(header(0).getAttribute('aria-sort')).toBeNull();
  });

  it('sorts by a clicked header and flips on the second click', async () => {
    (header(0).querySelector('button') as HTMLButtonElement).click();
    await fixture.whenStable();
    expect(fixture.componentInstance.sort()).toEqual({ key: 'key', dir: 'asc' });
    expect(header(0).getAttribute('aria-sort')).toBe('ascending');
    // Empty text sinks to the bottom regardless of direction.
    expect(firstColumnCells()).toEqual([
      'anthropic/claude-sonnet',
      'openai/gpt-x',
      'Untitled session',
    ]);

    (header(0).querySelector('button') as HTMLButtonElement).click();
    await fixture.whenStable();
    expect(fixture.componentInstance.sort()).toEqual({ key: 'key', dir: 'desc' });
    expect(firstColumnCells()).toEqual([
      'openai/gpt-x',
      'anthropic/claude-sonnet',
      'Untitled session',
    ]);
  });

  it('sorts costs with unknown ones last', async () => {
    (header(2).querySelector('button') as HTMLButtonElement).click();
    await fixture.whenStable();
    expect(fixture.componentInstance.sort()).toEqual({ key: 'cost', dir: 'desc' });
    expect(firstColumnCells()).toEqual([
      'anthropic/claude-sonnet',
      'Untitled session',
      'openai/gpt-x',
    ]);
  });

  it('formats cells by kind', () => {
    const table = fixture.componentInstance;
    const [, model] = COLUMNS;
    expect(table.cell(ROWS[1], model)).toBe('12\u202f345');
    expect(table.cell(ROWS[0], COLUMNS[2])).toBe('—');
    expect(table.cell(ROWS[1], COLUMNS[2])).toBe('$0.2500');
    expect(table.cell(ROWS[2], COLUMNS[2]))
      .withContext('a genuine zero is a price')
      .toBe('$0.0000');
    expect(table.cell(ROWS[2], COLUMNS[0])).toBe('Untitled session');

    const monoCell = fixture.nativeElement.querySelector('tbody tr td.mono') as HTMLElement;
    expect(monoCell).withContext('model ids render in the monospace cell class').not.toBeNull();
    const numCells = fixture.nativeElement.querySelectorAll('tbody tr td.num');
    expect(numCells.length).toBe(ROWS.length * 2);
  });

  it('shows the empty row when there is nothing to list', async () => {
    fixture.componentRef.setInput('rows', []);
    await fixture.whenStable();
    const empty = fixture.nativeElement.querySelector('td.empty') as HTMLElement;
    expect(empty).not.toBeNull();
    expect(empty.getAttribute('colspan')).toBe(String(COLUMNS.length));
  });

  it('formatNumber groups thousands with a narrow no-break space', () => {
    expect(formatNumber(0)).toBe('0');
    expect(formatNumber(999)).toBe('999');
    expect(formatNumber(1000)).toBe('1\u202f000');
    expect(formatNumber(1_234_567.4)).toBe('1\u202f234\u202f567');
  });
});
