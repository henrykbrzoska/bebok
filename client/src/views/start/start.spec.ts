/**
 * F0-1: the client always attempts to connect at startup.
 *
 * - a failing `fetch` must end in an explicit `error` phase with a visible
 *   Retry button and an editable address field (no silent "idle");
 * - a successful connection must end in `live` without ever showing the old
 *   idle/disconnected screen.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { AgentInfo, ProjectEntry, SessionMeta } from '../../core/engine.dtos';
import { EngineClient } from '../../core/engine-client.service';
import { EventsStore } from '../../core/events.store';
import { OpenSessionsStore } from '../../core/open-sessions.store';
import { ProjectsStore } from '../../core/projects.store';
import { NewSessionDialogStore } from '../../ui/new-session-dialog/new-session-dialog.store';
import { ProjectSessionsStore } from '../../ui/shell/project-sessions.store';
import { StartView } from './start';

describe('StartView (F0-1 auto-connect)', () => {
  let fixture: ComponentFixture<StartView>;
  let originalFetch: typeof fetch;

  function setup(): void {
    TestBed.configureTestingModule({
      imports: [StartView],
      providers: [provideZonelessChangeDetection(), provideRouter([])],
    });
    fixture = TestBed.createComponent(StartView);
    // The SSE stream is irrelevant here - keep it from opening a real socket.
    const events = TestBed.inject(EventsStore);
    spyOn(events, 'start');
    spyOn(events, 'restart');
  }

  beforeEach(() => {
    originalFetch = window.fetch;
    localStorage.clear();
    TestBed.resetTestingModule();
  });

  afterEach(() => {
    window.fetch = originalFetch;
  });

  it('shows an explicit error state with a Retry button when the engine is unreachable', async () => {
    window.fetch = jasmine
      .createSpy('fetch')
      .and.returnValue(Promise.reject(new TypeError('Failed to fetch')));
    setup();

    await fixture.componentInstance.ngOnInit();
    await fixture.whenStable();
    fixture.detectChanges();

    const component = fixture.componentInstance;
    expect(component.phase()).toBe('error');
    expect(component.connected()).toBeFalse();
    expect(component.statusTone()).toBe('danger');
    // The attempted address is part of the message the user sees.
    expect(component.statusLabel()).toContain('127.0.0.1:8787');

    const html = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(html).toContain('Retry');
    const input = (fixture.nativeElement as HTMLElement).querySelector('input');
    expect(input).withContext('address field must be editable after a failure').not.toBeNull();
  });

  it('reaches the live state without a flash of idle when the engine answers', async () => {
    const seen: string[] = [];
    window.fetch = jasmine
      .createSpy('fetch')
      .and.returnValue(Promise.resolve(new Response('{"sessions":[]}', { status: 200 })));
    setup();

    const component = fixture.componentInstance;
    seen.push(component.phase());
    await component.ngOnInit();
    await fixture.whenStable();
    seen.push(component.phase());

    expect(component.phase()).toBe('live');
    expect(component.error()).toBeNull();
    expect(component.statusTone()).toBe('success');
    // 'connecting' is the only intermediate phase - never a silent idle state.
    expect(seen[0]).toBe('connecting');
  });
});

/**
 * WP-GIT / F6-16: "New session" opens the dialog (no direct `createSession`);
 * deleting a worktree session offers - never forces - worktree removal, which
 * only runs through the separate removal endpoint after the delete succeeded.
 */
describe('StartView (F6-16 new-session dialog + worktree removal offer)', () => {
  let fixture: ComponentFixture<StartView>;
  let dialog: NewSessionDialogStore;
  let engine: {
    createSession: jasmine.Spy;
    deleteSession: jasmine.Spy;
    removeWorktree: jasmine.Spy;
    connect: jasmine.Spy;
    ping: jasmine.Spy;
    isTauri: () => boolean;
    connected: () => boolean;
    remoteDefaults: () => { baseUrl: string };
    readLastDirectory: () => string | null;
  };
  let projectSessions: {
    directory: ReturnType<typeof signal<string | null>>;
    sessions: ReturnType<typeof signal<SessionMeta[]>>;
    agents: ReturnType<typeof signal<AgentInfo[]>>;
    loading: ReturnType<typeof signal<boolean>>;
    refresh: jasmine.Spy;
    forget: jasmine.Spy;
    select: jasmine.Spy;
  };

  const DIR = '/work/alpha';
  const WT_DIR = '/work/alpha/.bebok/worktrees/bebok/session-abc123';
  const ALPHA: ProjectEntry = {
    id: 'alpha', name: 'Alpha', path: DIR, added_at: 1, last_opened_at: 1, pinned: false,
  };

  function meta(overrides: Partial<SessionMeta>): SessionMeta {
    const now = Date.now();
    return {
      id: 'session-0000',
      directory: DIR,
      agent: 'code',
      created_at: now,
      updated_at: now,
      usage: { input_tokens: 0, output_tokens: 0 },
      ...overrides,
    } as SessionMeta;
  }

  beforeEach(() => {
    localStorage.clear();
    TestBed.resetTestingModule();
    engine = {
      createSession: jasmine.createSpy('createSession'),
      deleteSession: jasmine.createSpy('deleteSession'),
      removeWorktree: jasmine.createSpy('removeWorktree').and.resolveTo({ removed: true, path: WT_DIR }),
      connect: jasmine.createSpy('connect').and.resolveTo({ kind: 'http', baseUrl: 'http://127.0.0.1:8787' }),
      ping: jasmine.createSpy('ping').and.resolveTo(undefined),
      isTauri: () => false,
      connected: () => true,
      remoteDefaults: () => ({ baseUrl: 'http://127.0.0.1:8787' }),
      readLastDirectory: () => DIR,
    };
    projectSessions = {
      directory: signal<string | null>(DIR),
      sessions: signal<SessionMeta[]>([]),
      agents: signal<AgentInfo[]>([{ name: 'code', builtin: true }]),
      loading: signal(false),
      refresh: jasmine.createSpy('refresh').and.resolveTo(undefined),
      forget: jasmine.createSpy('forget'),
      select: jasmine.createSpy('select').and.resolveTo(undefined),
    };
    TestBed.configureTestingModule({
      imports: [StartView],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        { provide: EngineClient, useValue: engine },
        { provide: ProjectSessionsStore, useValue: projectSessions },
        { provide: OpenSessionsStore, useValue: { close: jasmine.createSpy('close') } },
        {
          provide: ProjectsStore,
          useValue: {
            recent: signal<ProjectEntry[]>([ALPHA]),
            error: signal<string | null>(null),
            refresh: jasmine.createSpy('refresh').and.resolveTo(undefined),
            findByPath: (path: string | null) => (path === DIR ? ALPHA : null),
          },
        },
      ],
    });
    const events = TestBed.inject(EventsStore);
    spyOn(events, 'start');
    dialog = TestBed.inject(NewSessionDialogStore);
    fixture = TestBed.createComponent(StartView);
    // ngOnInit's auto-connect runs against the mocked engine (connect/ping
    // resolve); the fields below make the session list render right away.
    fixture.componentInstance.directory.set(DIR);
    fixture.componentInstance.connected.set(true);
    fixture.componentInstance.connecting.set(false);
    fixture.detectChanges();
  });

  it('"New session" opens the dialog with the preselected agent instead of creating directly', () => {
    const component = fixture.componentInstance;
    component.selectedAgent.set('code');
    component.startSession();
    expect(dialog.request()).toEqual({ directory: DIR, agent: 'code' });
    expect(engine.createSession).not.toHaveBeenCalled();
  });

  it('shows the branch badge and the removal checkbox only for worktree sessions', () => {
    projectSessions.sessions.set([
      meta({ id: 'plain-0001', title: 'Plain' }),
      meta({ id: 'wt-000001', title: 'Worktree', directory: WT_DIR, worktree_branch: 'bebok/session-abc123' }),
    ]);
    fixture.detectChanges();
    const host = fixture.nativeElement as HTMLElement;
    expect(host.querySelectorAll('app-branch-badge').length).toBe(1);

    const component = fixture.componentInstance;
    component.requestDeleteSession(projectSessions.sessions()[0], new Event('click'));
    fixture.detectChanges();
    expect(host.querySelector('.worktree-check')).toBeNull();

    component.requestDeleteSession(projectSessions.sessions()[1], new Event('click'));
    fixture.detectChanges();
    expect(host.querySelector('.worktree-check')).not.toBeNull();
    // Off by default: removal is offered, never forced.
    expect(component.removeWorktreeToo()).toBeFalse();
    component.cancelDelete();
  });

  it('deletes a worktree session without touching the worktree when the box is unticked', async () => {
    const wt = meta({ id: 'wt-000001', directory: WT_DIR, worktree_branch: 'bebok/session-abc123' });
    engine.deleteSession.and.resolveTo({
      sessionID: wt.id, directory: WT_DIR, deleted: true,
      is_worktree: true, worktree_path: WT_DIR, worktree_branch: 'bebok/session-abc123', project_root: DIR,
    });
    const component = fixture.componentInstance;
    component.requestDeleteSession(wt, new Event('click'));
    await component.confirmDeleteSession(wt, new Event('click'));
    expect(engine.deleteSession).toHaveBeenCalledWith(wt.id);
    expect(engine.removeWorktree).not.toHaveBeenCalled();
    expect(projectSessions.forget).toHaveBeenCalledWith(wt.id);
  });

  it('removes the worktree through the dedicated endpoint only when explicitly ticked, after the delete', async () => {
    const wt = meta({ id: 'wt-000001', directory: WT_DIR, worktree_branch: 'bebok/session-abc123' });
    const order: string[] = [];
    engine.deleteSession.and.callFake(async () => {
      order.push('delete');
      return {
        sessionID: wt.id, directory: WT_DIR, deleted: true,
        is_worktree: true, worktree_path: WT_DIR, worktree_branch: 'bebok/session-abc123', project_root: DIR,
      };
    });
    engine.removeWorktree.and.callFake(async () => {
      order.push('remove');
      return { removed: true, path: WT_DIR };
    });
    const component = fixture.componentInstance;
    component.requestDeleteSession(wt, new Event('click'));
    component.removeWorktreeToo.set(true);
    await component.confirmDeleteSession(wt, new Event('click'));
    expect(engine.removeWorktree).toHaveBeenCalledWith('alpha', WT_DIR);
    expect(order).toEqual(['delete', 'remove']);
    expect(component.notice()).toContain('removed');
    expect(component.error()).toBeNull();
    // The checkbox resets for the next delete.
    expect(component.removeWorktreeToo()).toBeFalse();
  });

  it('never calls the removal endpoint when the session delete fails', async () => {
    const wt = meta({ id: 'wt-000001', directory: WT_DIR, worktree_branch: 'bebok/session-abc123' });
    engine.deleteSession.and.rejectWith(new Error('409 session busy'));
    const component = fixture.componentInstance;
    component.requestDeleteSession(wt, new Event('click'));
    component.removeWorktreeToo.set(true);
    await component.confirmDeleteSession(wt, new Event('click'));
    expect(engine.removeWorktree).not.toHaveBeenCalled();
    expect(component.error()).toContain('busy');
  });

  it('reports a failed worktree removal without hiding that the session is gone', async () => {
    const wt = meta({ id: 'wt-000001', directory: WT_DIR, worktree_branch: 'bebok/session-abc123' });
    engine.deleteSession.and.resolveTo({
      sessionID: wt.id, directory: WT_DIR, deleted: true,
      is_worktree: true, worktree_path: WT_DIR, worktree_branch: 'bebok/session-abc123', project_root: DIR,
    });
    engine.removeWorktree.and.rejectWith(new Error('git failed: locked'));
    const component = fixture.componentInstance;
    component.requestDeleteSession(wt, new Event('click'));
    component.removeWorktreeToo.set(true);
    await component.confirmDeleteSession(wt, new Event('click'));
    expect(projectSessions.forget).toHaveBeenCalledWith(wt.id);
    expect(component.error()).toContain('locked');
  });
});
