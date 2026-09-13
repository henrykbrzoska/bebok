/**
 * F9-14: the Terminal screen lists the active session's background processes
 * above the PTY tab strip and opens a read-only log tab on a row click.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, provideRouter } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { EngineEvent, ProcessInfo, SessionMeta } from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { ProcessesStore } from '../../core/processes.store';
import { ChatSessionStore } from '../chat/chat-session.store';
import { TerminalView } from './terminal';

const META: SessionMeta = {
  id: 's1',
  directory: '/p',
  agent: 'code',
  created_at: 0,
  updated_at: 0,
  usage: { input_tokens: 0, output_tokens: 0 },
} as SessionMeta;

function row(id: string, extra: Partial<ProcessInfo> = {}): ProcessInfo {
  return {
    id,
    session_id: 's1',
    command: `npm run dev --workspace ${id}`,
    cwd: '/p',
    pid: 4242,
    started_at: Date.now() - 65_000,
    status: 'running',
    exit_code: null,
    ended_at: null,
    log_path: `/p/.bebok/run/${id}.log`,
    agent: 'main',
    ...extra,
  };
}

describe('TerminalView processes section (F9-14)', () => {
  let fixture: ComponentFixture<TerminalView>;
  let engine: {
    connected: ReturnType<typeof signal<boolean>>;
    connect: jasmine.Spy;
    readLastDirectory: jasmine.Spy;
    listPtys: jasmine.Spy;
    listSessions: jasmine.Spy;
    sessionProcesses: jasmine.Spy;
    processLog: jasmine.Spy;
    killProcess: jasmine.Spy;
  };
  let listener: ((event: EngineEvent) => void) | null;
  let session: ChatSessionStore;
  let store: ProcessesStore;

  beforeEach(() => {
    listener = null;
    engine = {
      connected: signal(true),
      connect: jasmine.createSpy('connect').and.resolveTo({ baseUrl: 'http://e' }),
      readLastDirectory: jasmine.createSpy('readLastDirectory').and.returnValue('/p'),
      listPtys: jasmine.createSpy('listPtys').and.resolveTo([]),
      listSessions: jasmine.createSpy('listSessions').and.resolveTo([]),
      sessionProcesses: jasmine.createSpy('sessionProcesses').and.resolveTo([
        row('a', { url: 'http://localhost:4200', port: 4200, agent: 'api-orders' }),
        row('b', { status: 'exited', exit_code: 1, ended_at: Date.now() - 1_000, command: 'cargo test' }),
      ]),
      processLog: jasmine.createSpy('processLog').and.resolveTo({ id: 'a', log: 'ready\n', size: 6 }),
      killProcess: jasmine.createSpy('killProcess').and.callFake((id: string) =>
        Promise.resolve(row(id, { status: 'exited', exit_code: -1, ended_at: Date.now() })),
      ),
    };
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
      imports: [TerminalView],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        { provide: EngineClient, useValue: engine },
        { provide: EventsStore, useValue: events },
        {
          provide: ActivatedRoute,
          useValue: { snapshot: { queryParamMap: new Map([['directory', '/p']]) } },
        },
      ],
    });
    session = TestBed.inject(ChatSessionStore);
    session.meta.set(META);
    store = TestBed.inject(ProcessesStore);
    fixture = TestBed.createComponent(TerminalView);
    fixture.detectChanges();
  });

  afterEach(() => {
    fixture.destroy();
    session.meta.set(null);
    store.clear();
    store.select(null);
  });

  async function settle(): Promise<void> {
    await fixture.whenStable();
    await Promise.resolve();
    await Promise.resolve();
    fixture.detectChanges();
  }

  function el(): HTMLElement {
    return fixture.nativeElement as HTMLElement;
  }

  function rows(): HTMLElement[] {
    return Array.from(el().querySelectorAll('[data-testid^="process-"]'));
  }

  it('lists the active session processes with status dot, agent, port and uptime', async () => {
    await settle();
    expect(engine.sessionProcesses).toHaveBeenCalledWith('s1');
    const list = rows();
    expect(list.length).toBe(2);

    const running = list[0];
    expect(running.textContent).toContain('npm run dev --workspace a');
    expect(running.textContent).toContain('api-orders');
    expect(running.textContent).toContain('localhost:4200');
    expect(running.textContent).toContain('1m 0');
    expect(running.querySelector('.dot')!.classList).toContain('tone-running');
    expect(running.getAttribute('title')).toContain('npm run dev --workspace a');

    const exited = list[1];
    expect(exited.textContent).toContain('cargo test');
    expect(exited.textContent).toContain('exit code 1');
    expect(exited.querySelector('.dot')!.classList).toContain('tone-failed');
    // The processes section sits above the PTY tab strip.
    const section = el().querySelector('[data-testid="processes"]')!;
    const strip = el().querySelector('.tab-strip')!;
    expect(section.compareDocumentPosition(strip) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it('shows the empty hint when the session has no processes', async () => {
    await settle();
    engine.sessionProcesses.and.resolveTo([]);
    await fixture.componentInstance.refreshProcesses();
    await settle();
    expect(rows().length).toBe(0);
    expect(el().textContent).toContain('No background processes.');
  });

  it('patches a row on process.exited', async () => {
    await settle();
    listener!({
      type: 'process.exited',
      directory: '/p',
      sessionID: 's1',
      properties: { id: 'a', sessionID: 's1', code: 0 },
    });
    await settle();
    const first = rows().find((r) => r.textContent!.includes('workspace a'))!;
    expect(first.querySelector('.dot')!.classList).toContain('tone-exited');
    expect(first.textContent).toContain('exit code 0');
  });

  it('opens a read-only log tab on a row click and seeds it from the log endpoint', async () => {
    await settle();
    rows()[0].click();
    await settle();
    expect(fixture.componentInstance.activeLogId()).toBe('a');
    expect(fixture.componentInstance.activePtyId()).toBeNull();
    expect(store.selected()).toBe('a');
    expect(engine.processLog).toHaveBeenCalledWith('a', 64 * 1024);
    expect(el().querySelector('[data-testid="log-tab-a"]')).not.toBeNull();
    expect(el().querySelector('[data-testid="log-tab-a"]')!.classList).toContain('active');
    const logTab = el().querySelector('app-process-log-tab')!;
    expect(logTab).not.toBeNull();
    const stop = logTab.querySelector('button.stop') as HTMLButtonElement;
    expect(stop.disabled).toBeFalse();
    expect(logTab.textContent).toContain('Open URL');
  });

  it('follows a selection made in the drawer (store.select)', async () => {
    await settle();
    store.select('b');
    await settle();
    expect(fixture.componentInstance.activeLogId()).toBe('b');
    expect(fixture.componentInstance.logTabs()).toEqual(['b']);
    // An exited process cannot be stopped.
    const stop = el().querySelector('app-process-log-tab button.stop') as HTMLButtonElement;
    expect(stop.disabled).toBeTrue();
  });

  it('closing the log tab clears the selection', async () => {
    await settle();
    fixture.componentInstance.openLog('a');
    await settle();
    fixture.componentInstance.closeLog('a');
    await settle();
    expect(fixture.componentInstance.activeLogId()).toBeNull();
    expect(fixture.componentInstance.logTabs()).toEqual([]);
    expect(store.selected()).toBeNull();
  });
});
