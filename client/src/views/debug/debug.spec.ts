/**
 * F2-20 / F2-21: the Debug log screen.
 *
 * - a 404 from `/debug/log` (the engine running without `BEBOK_DIAGNOSTIC`,
 *   see WP-AUTH / F0-6) must surface as the friendly explanation, not as a raw
 *   error string;
 * - log entries must map onto the handoff's row model (timestamp, source,
 *   status pill with the right tone, path, "status · timing"), newest first,
 *   and honour the header's filter.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../core/engine-client.service';
import { DebugEntry } from '../../core/engine.dtos';
import { DebugView } from './debug';

const ENTRIES: DebugEntry[] = [
  { ts: 1700000000000, source: 'http', kind: 'error', title: 'GET /session', detail: '400 (0ms)' },
  { ts: 1700000001000, source: 'http', kind: 'response', title: 'POST /session', detail: '200 (8ms)' },
];

describe('DebugView (F2-20/F2-21)', () => {
  let fixture: ComponentFixture<DebugView>;
  let engine: EngineClient;

  beforeEach(() => {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [DebugView],
      providers: [provideZonelessChangeDetection()],
    });
    fixture = TestBed.createComponent(DebugView);
    engine = TestBed.inject(EngineClient);
    spyOn(engine, 'connect').and.returnValue(Promise.resolve({ baseUrl: 'http://x' } as never));
  });

  afterEach(() => {
    fixture.componentInstance.ngOnDestroy();
  });

  it('explains that diagnostic mode is off instead of showing the 404', async () => {
    spyOn(engine, 'debugLog').and.returnValue(
      Promise.reject(new Error('engine GET /debug/log -> 404: not found')),
    );

    await fixture.componentInstance.refresh();

    const view = fixture.componentInstance;
    expect(view.diagnosticOff()).withContext('404 must flip the diagnostic flag').toBeTrue();
    expect(view.error()).withContext('no raw error for a gated endpoint').toBeNull();
    expect(view.rows().length).toBe(0);
  });

  it('still reports other failures verbatim', async () => {
    spyOn(engine, 'debugLog').and.returnValue(
      Promise.reject(new Error('engine GET /debug/log -> 500: boom')),
    );

    await fixture.componentInstance.refresh();

    expect(fixture.componentInstance.diagnosticOff()).toBeFalse();
    expect(fixture.componentInstance.error()).toContain('500');
  });

  it('maps entries onto the row model, newest first', async () => {
    spyOn(engine, 'debugLog').and.returnValue(
      Promise.resolve({ entries: ENTRIES, maxChars: 10000, calls: [] }),
    );

    await fixture.componentInstance.refresh();
    const rows = fixture.componentInstance.rows();

    expect(rows.length).toBe(2);
    expect(rows[0].path).withContext('newest entry first').toBe('POST /session');
    expect(rows[0].source).toBe('HTTP');
    expect(rows[0].pill).toBe('200');
    expect(rows[0].tone).toBe('success');
    expect(rows[0].meta).toBe('200 · 8ms');
    expect(rows[1].tone).withContext('error entries use the danger pill').toBe('danger');
    expect(rows[1].meta).toBe('400 · 0ms');
  });

  it('filters rows by path or status', async () => {
    spyOn(engine, 'debugLog').and.returnValue(
      Promise.resolve({ entries: ENTRIES, maxChars: 10000, calls: [] }),
    );

    const view = fixture.componentInstance;
    await view.refresh();
    view.filter.set('post');

    expect(view.rows().length).toBe(1);
    expect(view.rows()[0].path).toBe('POST /session');
  });

  it('groups the cap note like "10 000"', async () => {
    spyOn(engine, 'debugLog').and.returnValue(
      Promise.resolve({ entries: [], maxChars: 10000, calls: [] }),
    );

    await fixture.componentInstance.refresh();

    expect(fixture.componentInstance.maxCharsLabel()).toContain('10 000');
  });
});
