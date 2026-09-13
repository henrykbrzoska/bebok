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
import { ToolSafetyStore } from '../../core/tool-safety.store';
import { NewSessionDialogStore } from '../new-session-dialog/new-session-dialog.store';
import { ProjectSessionsStore } from '../shell/project-sessions.store';
import { Sidebar, buildSessionTree } from './sidebar';

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
  let uncategorizedCount: ReturnType<typeof signal<number>>;

  beforeEach(() => {
    uncategorizedCount = signal(0);
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
        { provide: ToolSafetyStore, useValue: { uncategorizedCount } },
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

  // -- F9-12: nested sub-agent rows --------------------------------------------

  describe('F9-12 sub-agent nesting', () => {
    const t0 = Date.now();
    const parent = session({ id: 'parent-0001', title: 'Add orders', updated_at: t0 - 1000, created_at: t0 - 9000 });
    const childA = session({ id: 'child-a', alias: 'api-orders', parent: ['parent-0001', 3], updated_at: t0 - 500, created_at: t0 - 8000 });
    const childB = session({ id: 'child-b', alias: 'docs-orders', parent: ['parent-0001', 3], updated_at: t0 - 100, created_at: t0 - 7000 });
    const grandchild = session({ id: 'grand-1', alias: 'nx-inspect', parent: ['child-a', 1], updated_at: t0 - 50, created_at: t0 - 6000 });
    const fork = session({ id: 'fork-0001', title: 'Add orders', parent: ['parent-0001', 2], updated_at: t0 - 10, created_at: t0 - 5000 });
    const orphan = session({ id: 'orphan-01', alias: 'lost-child', parent: ['gone-000', 0], updated_at: t0 - 5, created_at: t0 - 4000 });

    it('buildSessionTree nests task children (spawn order) but not forks/compactions', () => {
      const tree = buildSessionTree([orphan, fork, grandchild, childB, childA, parent]);
      expect(tree.roots.map((s) => s.id)).toEqual(['orphan-01', 'fork-0001', 'parent-0001']);
      const rows = tree.rows(parent);
      expect(rows.map((r) => `${r.depth}:${r.session.id}`)).toEqual([
        '0:parent-0001',
        '1:child-a',
        '2:grand-1',
        '1:child-b',
      ]);
      expect(rows[1].subAgent).toBeTrue();
      expect(rows[1].parent?.id).toBe('parent-0001');
      expect(tree.rows(fork)[0].subAgent).toBeFalse();
      expect(tree.rows(orphan)[0].subAgent).toBeTrue(); // glyph even without its parent
      expect(tree.rows(orphan)[0].depth).toBe(0);
    });

    it('renders children indented under the parent with the glyph and a tooltip', () => {
      projectSessions.sessions.set([fork, grandchild, childB, childA, parent]);
      fixture.detectChanges();
      const host = fixture.nativeElement as HTMLElement;
      const rows = Array.from(host.querySelectorAll('.session-row')) as HTMLElement[];
      expect(rows.map((r) => r.getAttribute('data-depth'))).toEqual(['0', '0', '1', '2', '1']);
      const child = rows[2];
      expect(child.getAttribute('data-subagent')).toBe('true');
      expect(child.classList).toContain('subagent');
      expect(child.querySelector('.subagent-glyph svg')).toBeTruthy();
      expect(child.getAttribute('title')).toContain('Sub-agent of Add orders');
      expect(parseFloat(child.style.paddingLeft)).toBeGreaterThan(parseFloat(rows[1].style.paddingLeft));
      expect(rows[1].querySelector('.subagent-glyph')).toBeNull(); // the fork is a normal row
    });

    it('a child whose parent is filtered out still shows the glyph at depth 0', () => {
      projectSessions.sessions.set([childA, parent]);
      fixture.componentInstance.search.set('api-orders');
      fixture.detectChanges();
      const rows = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.session-row')) as HTMLElement[];
      expect(rows.length).toBe(1);
      expect(rows[0].getAttribute('data-depth')).toBe('0');
      expect(rows[0].getAttribute('data-subagent')).toBe('true');
      expect(rows[0].getAttribute('title')).toContain('Sub-agent of parent-0');
    });
  });

  // -- F9-13: icon rail ----------------------------------------------------------

  describe('F9-13 icon rail', () => {
    it('renders five inline SVG icons with tooltips and aria-labels in the expanded sidebar', () => {
      const host = fixture.nativeElement as HTMLElement;
      const buttons = Array.from(host.querySelectorAll('.nav-rail .nav-btn')) as HTMLElement[];
      expect(buttons.length).toBe(5);
      for (const b of buttons) {
        expect(b.querySelector('svg')).toBeTruthy();
        expect(b.getAttribute('aria-label')).toBeTruthy();
        expect(b.getAttribute('data-tip')).toBe(b.getAttribute('aria-label'));
      }
      expect(host.querySelector('.nav-rail .nav-row')).toBeNull();
      expect(host.querySelector('.nav-rail .nav-label')).toBeNull();
    });

    it('collapsed: the same icons stacked with native titles (no 2-letter glyphs)', () => {
      fixture.componentInstance.shell.toggleSidebar();
      fixture.detectChanges();
      const host = fixture.nativeElement as HTMLElement;
      const buttons = Array.from(host.querySelectorAll('.rail .rail-btn')) as HTMLElement[];
      expect(buttons.length).toBe(5);
      for (const b of buttons) {
        expect(b.querySelector('svg')).toBeTruthy();
        expect(b.getAttribute('title')).toBeTruthy();
        expect(b.textContent?.trim()).toBe('');
      }
    });

    it('shows a badge dot on Settings only while tools are uncategorized', () => {
      const host = fixture.nativeElement as HTMLElement;
      expect(host.querySelector('[data-testid="nav-settings"] .nav-badge')).toBeNull();
      uncategorizedCount.set(3);
      fixture.detectChanges();
      const badge = host.querySelector('[data-testid="nav-settings"] .nav-badge') as HTMLElement;
      expect(badge).toBeTruthy();
      expect(badge.getAttribute('title')).toContain('3');
      expect(host.querySelector('[data-testid="nav-stats"] .nav-badge')).toBeNull();
    });
  });
});
