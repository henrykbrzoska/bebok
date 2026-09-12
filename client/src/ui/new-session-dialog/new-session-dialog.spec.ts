/**
 * WP-GIT / F6-16: the "New session" dialog round-trips agent, model and the
 * git-worktree option into `EngineClient.createSession` (mocked).
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router } from '@angular/router';

import { AgentInfo, ProjectEntry, ProjectGitInfo } from '../../core/engine.dtos';
import { EngineClient } from '../../core/engine-client.service';
import { ProjectsStore } from '../../core/projects.store';
import { ProjectSessionsStore } from '../shell/project-sessions.store';
import { NewSessionDialog, defaultBranchName, isValidBranch } from './new-session-dialog';
import { NewSessionDialogStore } from './new-session-dialog.store';

const DIR = '/work/alpha';
const ALPHA: ProjectEntry = {
  id: 'alpha', name: 'Alpha', path: DIR, added_at: 1, last_opened_at: 1, pinned: false,
};
const AGENTS: AgentInfo[] = [
  { name: 'code', builtin: true, model: 'anthropic/claude-sonnet' },
  { name: 'plan', builtin: true },
];
const GIT: ProjectGitInfo = {
  project_id: 'alpha', path: DIR, worktrees_dir: `${DIR}/.bebok/worktrees`,
  is_repo: true, root: DIR, branch: 'main', remote_url: 'git@github.com:acme/alpha.git', is_github: true, dirty_count: 2,
};
const CONFIG = {
  providers: [
    { name: 'anthropic', kind: 'anthropic', models: ['claude-sonnet'], has_key: true },
    { name: 'openai', kind: 'openai', models: ['gpt'], has_key: false },
  ],
};

describe('NewSessionDialog (F6-16)', () => {
  let fixture: ComponentFixture<NewSessionDialog>;
  let store: NewSessionDialogStore;
  let engine: {
    createSession: jasmine.Spy;
    projectGit: jasmine.Spy;
    getConfig: jasmine.Spy;
  };
  let router: { navigate: jasmine.Spy };
  let projectSessions: { agents: ReturnType<typeof signal<AgentInfo[]>>; refresh: jasmine.Spy };
  let registry: ProjectEntry[];

  async function settle(): Promise<void> {
    await fixture.whenStable();
    fixture.detectChanges();
    // Let the Promise.all in `prepare()` resolve and its signals apply.
    await new Promise((resolve) => setTimeout(resolve, 0));
    fixture.detectChanges();
  }

  beforeEach(() => {
    registry = [ALPHA];
    engine = {
      createSession: jasmine.createSpy('createSession').and.resolveTo({ sessionID: 'sess-1' }),
      projectGit: jasmine.createSpy('projectGit').and.resolveTo(GIT),
      getConfig: jasmine.createSpy('getConfig').and.resolveTo(CONFIG),
    };
    router = { navigate: jasmine.createSpy('navigate').and.resolveTo(true) };
    projectSessions = { agents: signal(AGENTS), refresh: jasmine.createSpy('refresh').and.resolveTo(undefined) };

    TestBed.configureTestingModule({
      imports: [NewSessionDialog],
      providers: [
        provideZonelessChangeDetection(),
        { provide: EngineClient, useValue: engine },
        { provide: Router, useValue: router },
        { provide: ProjectSessionsStore, useValue: projectSessions },
        {
          provide: ProjectsStore,
          useValue: {
            findByPath: (path: string | null) => registry.find((p) => p.path === path) ?? null,
          },
        },
      ],
    });
    store = TestBed.inject(NewSessionDialogStore);
    fixture = TestBed.createComponent(NewSessionDialog);
    fixture.detectChanges();
  });

  it('stays hidden until a caller opens it for a directory', () => {
    expect((fixture.nativeElement as HTMLElement).querySelector('.card')).toBeNull();
    store.openFor(DIR, { agent: 'plan' });
    fixture.detectChanges();
    expect((fixture.nativeElement as HTMLElement).querySelector('.card')).not.toBeNull();
    expect(fixture.componentInstance.selectedAgent()).toBe('plan');
  });

  it('loads keyed-provider models and the git state for a registered project', async () => {
    store.openFor(DIR);
    await settle();
    const component = fixture.componentInstance;
    expect(engine.getConfig).toHaveBeenCalledWith(DIR);
    expect(engine.projectGit).toHaveBeenCalledWith('alpha');
    expect(component.models()).toEqual(['anthropic/claude-sonnet']);
    expect(component.worktreeAvailable()).toBeTrue();
    // Base branch defaults to the project's current branch.
    expect(component.base()).toBe('main');
    expect(component.branch()).toMatch(/^bebok\/session-[a-z0-9]{6}$/);
  });

  it('creates a plain session (no worktree, agent default model) and opens the chat', async () => {
    store.openFor(DIR, { agent: 'code' });
    await settle();
    await fixture.componentInstance.create();
    expect(engine.createSession).toHaveBeenCalledWith(DIR, 'code', undefined, undefined);
    expect(projectSessions.refresh).toHaveBeenCalled();
    expect(router.navigate).toHaveBeenCalledWith(['/chat', 'sess-1']);
    expect(store.open()).toBeFalse();
  });

  it('round-trips the worktree option (branch + base) and the chosen model', async () => {
    store.openFor(DIR);
    await settle();
    const component = fixture.componentInstance;
    component.selectedModel.set('anthropic/claude-sonnet');
    component.useWorktree.set(true);
    component.branch.set('bebok/feature-x');
    component.base.set('develop');
    fixture.detectChanges();
    expect(component.worktreeSpec()).toEqual({ branch: 'bebok/feature-x', base: 'develop' });

    await component.create();
    expect(engine.createSession).toHaveBeenCalledWith(
      DIR,
      'code',
      'anthropic/claude-sonnet',
      { branch: 'bebok/feature-x', base: 'develop' },
    );
    expect(router.navigate).toHaveBeenCalledWith(['/chat', 'sess-1']);
  });

  it('omits an empty base and refuses to create with an invalid branch name', async () => {
    store.openFor(DIR);
    await settle();
    const component = fixture.componentInstance;
    component.useWorktree.set(true);
    component.base.set('   ');
    component.branch.set('bebok/ok');
    expect(component.worktreeSpec()).toEqual({ branch: 'bebok/ok' });

    component.branch.set('../escape');
    fixture.detectChanges();
    expect(component.branchValid()).toBeFalse();
    expect(component.canCreate()).toBeFalse();
    await component.create();
    expect(engine.createSession).not.toHaveBeenCalled();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('Branch names may only contain');
  });

  it('disables the worktree option for a non-repo project and an unregistered directory', async () => {
    engine.projectGit.and.resolveTo({ ...GIT, is_repo: false, branch: null, root: null, remote_url: null, is_github: false, dirty_count: null });
    store.openFor(DIR);
    await settle();
    const component = fixture.componentInstance;
    expect(component.worktreeAvailable()).toBeFalse();
    expect(component.worktreeHint()).toContain('Not a git repository');
    const checkbox = (fixture.nativeElement as HTMLElement).querySelector<HTMLInputElement>('input[type=checkbox]');
    expect(checkbox?.disabled).toBeTrue();

    store.close();
    registry = [];
    engine.projectGit.calls.reset();
    store.openFor('/work/unregistered');
    await settle();
    expect(engine.projectGit).not.toHaveBeenCalled();
    expect(component.worktreeAvailable()).toBeFalse();
    expect(component.worktreeHint()).toContain('Register this directory');
  });

  it('shows the engine error inside the dialog and keeps it open', async () => {
    engine.createSession.and.rejectWith(new Error('git failed: branch exists'));
    store.openFor(DIR);
    await settle();
    await fixture.componentInstance.create();
    fixture.detectChanges();
    expect(fixture.componentInstance.error()).toContain('branch exists');
    expect(store.open()).toBeTrue();
    expect(router.navigate).not.toHaveBeenCalled();
  });

  it('closes on Escape and on the backdrop', () => {
    store.openFor(DIR);
    fixture.detectChanges();
    fixture.componentInstance.onDocumentKeydown(new KeyboardEvent('keydown', { key: 'Escape' }));
    expect(store.open()).toBeFalse();
    store.openFor(DIR);
    fixture.detectChanges();
    (fixture.nativeElement as HTMLElement).querySelector<HTMLElement>('.backdrop')?.click();
    expect(store.open()).toBeFalse();
  });
});

describe('branch helpers (F6-16)', () => {
  it('accepts safe names and rejects traversal / option-like names', () => {
    for (const ok of ['bebok/session-1a2b3c', 'feature', 'a.b-c_d', 'x/y/z']) {
      expect(isValidBranch(ok)).withContext(ok).toBeTrue();
    }
    for (const bad of ['', '-x', '.hidden', 'a/../b', 'a//b', 'a/', '/a', 'a b', 'a\\b', 'a.lock', 'a/.git', 'x@{1}']) {
      expect(isValidBranch(bad)).withContext(bad).toBeFalse();
    }
  });

  it('generates a valid, prefixed default branch name', () => {
    const name = defaultBranchName();
    expect(name).toMatch(/^bebok\/session-[a-z0-9]{6}$/);
    expect(isValidBranch(name)).toBeTrue();
  });
});
