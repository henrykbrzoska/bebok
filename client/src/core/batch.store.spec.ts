/**
 * Batch store: folds `batch.*` SSE events into the session's batch list.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { BatchStore, batchIsTerminal } from './batch.store';
import { EngineClient } from './engine-client.service';
import { BatchRecord, EngineEvent } from './engine.dtos';
import { EventsStore } from './events.store';

function record(id: string, extra: Partial<BatchRecord> = {}): BatchRecord {
  return {
    id,
    directory: '/p',
    session_id: 's1',
    provider: 'openrouter',
    model: 'openrouter/deepseek/deepseek-chat:batch',
    state: 'Submitted',
    job_id: 'job-1',
    items: [],
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    error: null,
    ...extra,
  };
}

function ev(type: string, properties: unknown): EngineEvent {
  return { type, directory: '/p', sessionID: 's1', properties: properties as EngineEvent['properties'] };
}

describe('BatchStore', () => {
  let store: BatchStore;
  let engine: { sessionBatches: jasmine.Spy; getBatch: jasmine.Spy };

  beforeEach(() => {
    engine = {
      sessionBatches: jasmine.createSpy('sessionBatches').and.resolveTo([]),
      getBatch: jasmine.createSpy('getBatch'),
    };
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        BatchStore,
        { provide: EngineClient, useValue: engine },
        { provide: EventsStore, useValue: { subscribe: () => () => undefined } },
      ],
    });
    store = TestBed.inject(BatchStore);
  });

  it('batchIsTerminal covers the three terminal states', () => {
    expect(batchIsTerminal('Completed')).toBeTrue();
    expect(batchIsTerminal('Failed')).toBeTrue();
    expect(batchIsTerminal('Cancelled')).toBeTrue();
    expect(batchIsTerminal('Polling')).toBeFalse();
    expect(batchIsTerminal('Submitted')).toBeFalse();
  });

  it('batch.submitted seeds progress and reloads', async () => {
    store.load('s1', '/p');
    store.ingest(ev('batch.submitted', { batchID: 'b1', count: 4 }));
    expect(store.batchProgress('b1')()).toEqual({ done: 0, total: 4 });
    await Promise.resolve();
    expect(engine.sessionBatches).toHaveBeenCalled();
  });

  it('batch.progress updates the done count', () => {
    store.load('s1', '/p');
    store.ingest(ev('batch.progress', { batchID: 'b1', done: 3, total: 4 }));
    expect(store.batchProgress('b1')()).toEqual({ done: 3, total: 4 });
  });

  it('batch.completed reloads the single record and clears progress', async () => {
    engine.getBatch.and.resolveTo(record('b1', { state: 'Completed' }));
    store.load('s1', '/p');
    store.ingest(ev('batch.progress', { batchID: 'b1', done: 1, total: 2 }));
    store.ingest(ev('batch.completed', { batchID: 'b1', ok: 2, failed: 0 }));
    await Promise.resolve();
    await Promise.resolve();
    expect(engine.getBatch).toHaveBeenCalledWith('b1', '/p');
    expect(store.batchProgress('b1')()).toBeNull();
    expect(store.batches().some((b) => b.id === 'b1')).toBeTrue();
  });

  it('ignores events of other sessions', () => {
    store.load('s1', '/p');
    store.ingest({ type: 'batch.progress', directory: '/p', sessionID: 's2', properties: { batchID: 'b9', done: 1, total: 1 } } as EngineEvent);
    expect(store.batchProgress('b9')()).toBeNull();
  });
});
