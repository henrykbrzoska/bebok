/**
 * WP-GIT / F6-16: the sidebar's "+ New" opens the New-session dialog instead
 * of creating a session directly, and worktree sessions carry a branch badge.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { AgentInfo, SessionMeta } from '../../core/engine.dtos';
import { EngineClient } from '../../core/engine-client.service';
import { EventsStore } from '../../core/events.store';
import { ProjectsStore } from '../../core/projects.store';
import { NewSessionDialogStore } from '../new-session-dialog/new-session-dialog.store';
import { ProjectSessionsStore } from '../shell/project-sessions.store';
import { Sidebar } from './sidebar';

const DIR = '/work/alpha';

function session(overrides: Partial<SessionMeta>): SessionMeta {
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

describe('Sidebar (F6-16 new-session dialog + branch badge)', () => {
  let fixture: ComponentFixture<Sidebar>;
  let dialog: NewSessionDialogStore;
  let projectSessions: {
    directory: ReturnType<typeof signal<string | null>>;
    sessions: ReturnType<typeof signal<SessionMeta[]>>;
    agents: ReturnType<typeof signal<AgentInfo[]>>;
    refresh: jasmine.Spy;
  };
  let engine: { createSession: jasmine.Spy; connected: () => boolean };

  beforeEach(() => {
    localStorage.clear();
    projectSessions = {
      directory: signal<string | null>(DIR),
      sessions: signal<SessionMeta[]>([]),
      agents: signal<AgentInfo[]>([{ name: 'code', builtin: true }, { name: 'plan', builtin: true }]),
      refresh: jasmine.createSpy('refresh').and.resolveTo(undefined),
    };
    engine = { createSession: jasmine.createSpy('createSession'), connected: () => false };

    TestBed.configureTestingModule({
      imports: [Sidebar],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        { provide: ProjectSessionsStore, useValue: projectSessions },
        { provide: EngineClient, useValue: engine },
        { provide: ProjectsStore, useValue: { findByPath: () => null } },
      ],
    });
    const events = TestBed.inject(EventsStore);
    spyOn(events, 'start');
    dialog = TestBed.inject(NewSessionDialogStore);
    fixture = TestBed.createComponent(Sidebar);
    fixture.detectChanges();
  });

  it('"+ New" opens the dialog for the current directory with the preselected agent, never createSession directly', () => {
    const component = fixture.componentInstance;
    component.selectedAgent.set('plan');
    component.newSession();
    expect(dialog.request()).toEqual({ directory: DIR, agent: 'plan' });
    expect(engine.createSession).not.toHaveBeenCalled();
  });

  it('does nothing without a directory', () => {
    projectSessions.directory.set(null);
    fixture.componentInstance.newSession();
    expect(dialog.request()).toBeNull();
  });

  it('renders a branch badge only for worktree sessions', () => {
    projectSessions.sessions.set([
      session({ id: 'plain-0001', title: 'Plain' }),
      session({ id: 'wt-000001', title: 'Worktree', worktree_branch: 'bebok/session-abc123' }),
    ]);
    fixture.detectChanges();
    const rows = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.session-row'));
    expect(rows.length).toBe(2);
    const badges = (fixture.nativeElement as HTMLElement).querySelectorAll('app-branch-badge');
    expect(badges.length).toBe(1);
    expect(badges[0].textContent).toContain('bebok/session-abc123');
    expect(fixture.componentInstance.worktreeBranch(session({}))).toBeNull();
    expect(fixture.componentInstance.worktreeBranch(session({ worktree_branch: '  ' }))).toBeNull();
  });
});
