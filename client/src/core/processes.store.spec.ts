/**
 * F9-14: the process registry store folds `process.output` / `process.exited`
 * into the listed rows, keeps a capped output buffer per process and detects
 * a local URL when the engine did not.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { EngineClient } from './engine-client.service';
import { EngineEvent, ProcessInfo } from './engine.dtos';
import { EventsStore } from './events.store';
import { OUTPUT_RING_BYTES, ProcessesStore, detectUrl, formatUptime } from './processes.store';

function row(id: string, extra: Partial<ProcessInfo> = {}): ProcessInfo {
  return {
    id,
    session_id: 's1',
    command: `npm run dev-${id}`,
    cwd: '/p',
    pid: 100,
    started_at: 1_000,
    status: 'running',
    exit_code: null,
    ended_at: null,
    log_path: `/p/.bebok/run/${id}.log`,
    agent: 'main',
    ...extra,
  };
}

function output(id: string, chunk: string, at = 5_000): EngineEvent {
  return { type: 'process.output', directory: '/p', sessionID: 's1', properties: { id, sessionID: 's1', chunk, at } };
}

function exited(id: string, code: number | null): EngineEvent {
  return { type: 'process.exited', directory: '/p', sessionID: 's1', properties: { id, sessionID: 's1', code } };
}

describe('ProcessesStore (F9-14)', () => {
  let store: ProcessesStore;
  let listener: ((event: EngineEvent) => void) | null;
  let engine: { sessionProcesses: jasmine.Spy; killProcess: jasmine.Spy };

  beforeEach(() => {
    jasmine.clock().install();
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
    engine = {
      sessionProcesses: jasmine.createSpy('sessionProcesses').and.resolveTo([row('a'), row('b', { status: 'exited', exit_code: 0 })]),
      killProcess: jasmine.createSpy('killProcess').and.callFake((id: string) =>
        Promise.resolve(row(id, { status: 'exited', exit_code: -1, ended_at: 9_000 })),
      ),
    };
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        { provide: EventsStore, useValue: events },
        { provide: EngineClient, useValue: engine },
      ],
    });
    store = TestBed.inject(ProcessesStore);
  });

  afterEach(() => {
    jasmine.clock().uninstall();
  });

  it('loads the list of a session (running rows first)', async () => {
    await store.load('s1');
    expect(engine.sessionProcesses).toHaveBeenCalledWith('s1');
    expect(store.sessionId()).toBe('s1');
    expect(store.processes().map((p) => p.id)).toEqual(['a', 'b']);
    expect(store.running().map((p) => p.id)).toEqual(['a']);
    expect(store.loading()).toBeFalse();
    expect(store.error()).toBeNull();
  });

  it('surfaces a load failure and keeps the session id', async () => {
    engine.sessionProcesses.and.rejectWith(new Error('engine GET -> 500'));
    await store.load('s1');
    expect(store.error()).toContain('500');
    expect(store.processes()).toEqual([]);
  });

  it('patches status and exit code on process.exited, then reloads', async () => {
    await store.load('s1');
    expect(store.byId('a')?.status).toBe('running');

    listener!(exited('a', 3));
    const a = store.byId('a')!;
    expect(a.status).toBe('exited');
    expect(a.exit_code).toBe(3);
    expect(a.ended_at).toEqual(jasmine.any(Number));
    expect(store.running().length).toBe(0);

    engine.sessionProcesses.calls.reset();
    jasmine.clock().tick(1_000);
    expect(engine.sessionProcesses).toHaveBeenCalledWith('s1');
  });

  it('appends process.output to a per-process buffer and emits chunks', async () => {
    await store.load('s1');
    const seen: string[] = [];
    const sub = store.chunks$.subscribe((c) => seen.push(`${c.id}:${c.chunk}`));

    listener!(output('a', 'hello '));
    listener!(output('a', 'world\n'));
    listener!(output('b', 'other'));

    expect(store.output('a')()).toBe('hello world\n');
    expect(store.output('b')()).toBe('other');
    expect(store.output('zzz')()).toBe('');
    expect(seen).toEqual(['a:hello ', 'a:world\n', 'b:other']);
    sub.unsubscribe();
  });

  it('caps the buffer at OUTPUT_RING_BYTES, dropping the oldest output', async () => {
    await store.load('s1');
    const line = 'x'.repeat(1023) + '\n';
    const lines = Math.ceil(OUTPUT_RING_BYTES / line.length) + 5;
    for (let i = 0; i < lines; i++) {
      listener!(output('a', line));
    }
    listener!(output('a', 'TAIL'));
    const buffer = store.output('a')();
    expect(buffer.length).toBeLessThanOrEqual(OUTPUT_RING_BYTES);
    expect(buffer.endsWith('TAIL')).toBeTrue();
    // Cut on a line boundary: the buffer starts with a full line, not a fragment.
    expect(buffer.startsWith('x')).toBeTrue();
    expect(buffer.indexOf('\n')).toBe(1023);
  });

  it('seed() replaces the buffer with the log snapshot', async () => {
    await store.load('s1');
    listener!(output('a', 'streamed'));
    store.seed('a', 'from-file\n');
    expect(store.output('a')()).toBe('from-file\n');
    listener!(output('a', 'more'));
    expect(store.output('a')()).toBe('from-file\nmore');
  });

  it('detects the url/port from output when the engine did not', async () => {
    await store.load('s1');
    expect(store.byId('a')?.url).toBeUndefined();
    listener!(output('a', '  ➜  Local:   http://localhost:4200/\n'));
    expect(store.byId('a')?.port).toBe(4200);
    expect(store.byId('a')?.url).toBe('http://localhost:4200/');
    // Engine-supplied values win and are never overwritten by later output.
    listener!(output('a', 'http://localhost:9999'));
    expect(store.byId('a')?.port).toBe(4200);
  });

  it('schedules a reload when output arrives for an unknown process', async () => {
    await store.load('s1');
    engine.sessionProcesses.calls.reset();
    listener!(output('new-one', 'boot'));
    expect(engine.sessionProcesses).not.toHaveBeenCalled();
    jasmine.clock().tick(1_000);
    expect(engine.sessionProcesses).toHaveBeenCalledTimes(1);
  });

  it('kill() calls the engine and replaces the row', async () => {
    await store.load('s1');
    await store.kill('a');
    expect(engine.killProcess).toHaveBeenCalledWith('a');
    expect(store.byId('a')?.status).toBe('exited');
    expect(store.byId('a')?.exit_code).toBe(-1);
  });

  it('select() carries the log tab handoff', () => {
    expect(store.selected()).toBeNull();
    store.select('a');
    expect(store.selected()).toBe('a');
    store.select(null);
    expect(store.selected()).toBeNull();
  });
});

describe('detectUrl (F9-14)', () => {
  it('finds localhost / 127.0.0.1 urls with paths', () => {
    expect(detectUrl('Server listening at http://localhost:3000')).toEqual({ url: 'http://localhost:3000', port: 3000 });
    expect(detectUrl('open http://127.0.0.1:8080/app/index.html now')).toEqual({
      url: 'http://127.0.0.1:8080/app/index.html',
      port: 8080,
    });
    expect(detectUrl('at https://localhost:8443/.')).toEqual({ url: 'https://localhost:8443/', port: 8443 });
  });

  it('falls back to a "port <n>" mention', () => {
    expect(detectUrl('Listening on port 5173')).toEqual({ url: 'http://localhost:5173', port: 5173 });
    expect(detectUrl('PORT=8000')).toEqual({ url: 'http://localhost:8000', port: 8000 });
  });

  it('returns null when nothing looks like a local server', () => {
    expect(detectUrl('')).toBeNull();
    expect(detectUrl('compiled successfully')).toBeNull();
    expect(detectUrl('see https://example.com/docs')).toBeNull();
    expect(detectUrl('port 99999')).toBeNull();
  });
});

describe('formatUptime (F9-14)', () => {
  it('formats seconds, minutes, hours and days', () => {
    expect(formatUptime(0, 12_000)).toBe('12s');
    expect(formatUptime(0, 185_000)).toBe('3m 05s');
    expect(formatUptime(0, 2 * 3_600_000 + 3 * 60_000)).toBe('2h 03m');
    expect(formatUptime(0, 28 * 3_600_000)).toBe('1d 4h');
  });

  it('never goes negative', () => {
    expect(formatUptime(10_000, 0)).toBe('0s');
  });
});
