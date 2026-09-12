/**
 * WP-DELEGATION (F8-2): the progress store folds `task.started` /
 * `task.progress` / `task.ended` events into one live map and can match a
 * `task` tool call's arguments to the child the engine spawned for it.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { EngineEvent } from './engine.dtos';
import { EventsStore } from './events.store';
import {
  TaskProgressStore,
  descriptionOf,
  normalizeChildName,
} from './task-progress.store';

function started(taskID: string, extra: Record<string, unknown> = {}): EngineEvent {
  return {
    type: 'task.started',
    directory: '/p',
    sessionID: 'parent-1',
    properties: {
      taskID,
      childSessionID: `child-${taskID}`,
      name: `name-${taskID}`,
      agent: 'code',
      description: `prompt for ${taskID}`,
      startedAt: 1_000,
      status: 'running',
      background: true,
      ...extra,
    },
  };
}

describe('TaskProgressStore (WP-DELEGATION)', () => {
  let store: TaskProgressStore;
  let listener: ((event: EngineEvent) => void) | null;

  beforeEach(() => {
    listener = null;
    const events = {
      reconnectVersion: signal(0),
      onEvent: jasmine.createSpy('onEvent').and.callFake((fn: (event: EngineEvent) => void) => {
        listener = fn;
        return () => {
          listener = null;
        };
      }),
    };
    TestBed.configureTestingModule({
      providers: [provideZonelessChangeDetection(), { provide: EventsStore, useValue: events }],
    });
    store = TestBed.inject(TaskProgressStore);
  });

  it('registers a child on task.started and updates it on task.progress', () => {
    listener!(started('t1'));
    expect(store.entries().length).toBe(1);
    expect(store.byTaskID('t1')?.status).toBe('running');
    expect(store.byTaskID('t1')?.background).toBeTrue();

    listener!({
      type: 'task.progress',
      directory: '/p',
      sessionID: 'parent-1',
      properties: {
        taskID: 't1',
        childSessionID: 'child-t1',
        name: 'name-t1',
        agent: 'code',
        status: 'running',
        progress: { lastTool: 'read_file', lastToolState: 'completed', summary: 'Reading', toolCalls: 1, steps: 1 },
        tokens: { input: 300, output: 40 },
        at: 2_000,
      },
    });
    const e = store.byTaskID('t1')!;
    expect(e.progress?.lastTool).toBe('read_file');
    expect(e.progress?.summary).toBe('Reading');
    expect(e.tokens).toEqual({ input: 300, output: 40 });
    expect(store.liveForSession('parent-1').length).toBe(1);
    expect(store.liveForSession('other').length).toBe(0);
  });

  it('marks queued children and flips them to running on the first progress event', () => {
    listener!(started('q', { status: 'queued' }));
    expect(store.byTaskID('q')?.status).toBe('queued');
    listener!({
      type: 'task.progress',
      directory: '/p',
      sessionID: 'parent-1',
      properties: { taskID: 'q', status: 'running', progress: { summary: '', toolCalls: 0, steps: 0 }, tokens: {} },
    });
    expect(store.byTaskID('q')?.status).toBe('running');
  });

  it('keeps a finished entry with its final status (done / failed / aborted)', () => {
    listener!(started('a'));
    listener!(started('b'));
    listener!(started('c'));
    listener!({ type: 'task.ended', directory: '/p', sessionID: 'parent-1', properties: { taskID: 'a', status: 'completed', tokens: { input: 1, output: 2 } } });
    listener!({ type: 'task.ended', directory: '/p', sessionID: 'parent-1', properties: { taskID: 'b', status: 'error' } });
    listener!({ type: 'task.aborted', directory: '/p', sessionID: 'parent-1', properties: { taskID: 'c' } });
    expect(store.byTaskID('a')?.status).toBe('done');
    expect(store.byTaskID('a')?.tokens).toEqual({ input: 1, output: 2 });
    expect(store.byTaskID('b')?.status).toBe('failed');
    expect(store.byTaskID('c')?.status).toBe('aborted');
    expect(store.liveForSession('parent-1').length).toBe(0);
  });

  it('matches a task call by its normalised name or by the prompt prefix', () => {
    listener!(started('n', { name: 'api-tests', description: descriptionOf('Add unit tests for the api app controller') }));
    // 81 chars: the engine keeps only the first 80 as the task description, so
    // the tool call's full prompt must match on that prefix.
    const long = 'Update README.md with a features section and nothing else, keep the style, thanks';
    expect(long.length).toBeGreaterThan(80);
    listener!(started('m', { name: 'code-2', description: descriptionOf(long) }));

    expect(store.matchTaskCall({ name: 'API Tests', prompt: 'whatever' }, 'parent-1')?.taskID).toBe('n');
    expect(store.matchTaskCall({ prompt: '  Add unit tests for the api app controller  ' })?.taskID).toBe('n');
    expect(store.matchTaskCall({ prompt: long })?.taskID).toBe('m');
    expect(store.matchTaskCall({ prompt: 'unrelated' })).toBeNull();
    expect(store.matchTaskCall({ name: 'api-tests' }, 'other-session')).toBeNull();
    expect(store.matchTaskCall(undefined)).toBeNull();
  });

  it('normalises names like the engine does', () => {
    expect(normalizeChildName('Auth Flow Audit!')).toBe('auth-flow-audit');
    expect(normalizeChildName('--x--')).toBe('x');
    expect(normalizeChildName('a'.repeat(40)).length).toBe(32);
    expect(descriptionOf('x'.repeat(100)).length).toBe(80);
  });
});
