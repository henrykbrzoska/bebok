/**
 * Git panel specs.
 *
 * Phase 1 (read-only): file groups from `GitStatus.files` and the engine reads
 * (status / branches / log) that fill the drawer section.
 * Phase 4b (writes): the lane graph, the commit button guard and the
 * confirm gate in front of destructive operations.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { By } from '@angular/platform-browser';

import { EngineClient } from '../../../core/engine-client.service';
import {
  GitBranchesResponse,
  GitFileStatus,
  GitLogResponse,
  GitStatus,
  ProjectEntry,
  SessionMeta,
} from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { ProjectsStore } from '../../../core/projects.store';
import { ChatSessionStore } from '../../../views/chat/chat-session.store';
import { GitPanel, computeLanes, groupGitFiles, laneSegments } from './git-panel';

const META: SessionMeta = {
  id: 's1',
  directory: '/p',
  agent: 'code',
  created_at: 0,
  updated_at: 0,
  usage: { input_tokens: 0, output_tokens: 0 },
} as SessionMeta;

const PROJECT = {
  id: 'p1',
  name: 'p',
  path: '/p',
  added_at: 0,
  last_opened_at: null,
  pinned: false,
} as ProjectEntry;

function file(over: Partial<GitFileStatus>): GitFileStatus {
  return {
    path: 'a.ts',
    x: ' ',
    y: ' ',
    staged: false,
    unstaged: false,
    untracked: false,
    renamed_from: null,
    ...over,
  };
}

const STAGED = file({ path: 'src/staged.ts', x: 'M', staged: true });
const UNSTAGED = file({ path: 'src/dirty.ts', y: 'M', unstaged: true });
const UNTRACKED = file({ path: 'notes.md', x: '?', y: '?', untracked: true });
const BOTH = file({ path: 'both.ts', x: 'M', y: 'M', staged: true, unstaged: true });

const STATUS: GitStatus = { branch: 'main', ahead: 1, behind: 2, files: [STAGED, UNSTAGED, UNTRACKED, BOTH] };
const BRANCHES: GitBranchesResponse = {
  current: 'main',
  branches: [
    { name: 'main', current: true, upstream: 'origin/main', ahead: 1, behind: 0, last_commit: 'abc1234', last_ts: 0 },
    { name: 'feat', current: false, upstream: null, ahead: 0, behind: 0, last_commit: 'def5678', last_ts: 0 },
  ],
};
const LOG: GitLogResponse = {
  entries: [
    { hash: 'abcdef0123456789', parents: [], author: 'Dev', email: 'd@x', ts: 0, subject: 'feat: add git panel', body: '', refs: ['HEAD -> main'] },
  ],
  has_more: false,
};

describe('groupGitFiles', () => {
  it('splits rows into staged / unstaged / untracked, keeping engine order', () => {
    const groups = groupGitFiles([STAGED, UNSTAGED, UNTRACKED, BOTH]);
    expect(groups.map((g) => [g.key, g.rows.map((r) => r.path)])).toEqual([
      ['staged', ['src/staged.ts', 'both.ts']],
      ['unstaged', ['src/dirty.ts', 'both.ts']],
      ['untracked', ['notes.md']],
    ]);
  });

  it('drops empty groups', () => {
    expect(groupGitFiles([UNTRACKED]).map((g) => g.key)).toEqual(['untracked']);
    expect(groupGitFiles([])).toEqual([]);
  });
});

describe('computeLanes', () => {
  it('keeps a linear history in one lane', () => {
    const rows = computeLanes([['b'], ['c'], []], ['a', 'b', 'c']);
    expect(rows.map((r) => r.lane)).toEqual([0, 0, 0]);
    expect(rows.map((r) => r.width)).toEqual([1, 1, 1]);
    expect(rows[0].parentLanes).toEqual([0]);
  });

  it('opens a second lane at a fork and closes it again at the join', () => {
    //   a (merge of b + d) -> b -> c ; d tip in its own lane
    //   history (newest first): a(b,d), d, b(a-part), c
    const parents = [['b', 'd'], ['base'], ['c'], ['base']];
    const hashes = ['a', 'd', 'b', 'c'];
    const rows = computeLanes(parents, hashes);

    // The merge commit sits in lane 0 and fans out to a second lane.
    expect(rows[0].lane).toBe(0);
    expect(rows[0].parentLanes.length).toBe(2);
    expect(rows[0].width).toBe(2);
    // `d` takes the freshly opened lane 1.
    expect(rows[1].lane).toBe(1);
    // `b` continues in lane 0.
    expect(rows[2].lane).toBe(0);
    // The graph narrows back once both lanes converge on `base`.
    expect(rows[3].width).toBeLessThanOrEqual(2);
  });

  it('draws a node rail plus one curve per merge parent', () => {
    const rows = computeLanes([['b', 'd'], [], []], ['a', 'b', 'd']);
    const segments = laneSegments(rows[0]);
    // Incoming rail above the node + straight rail down lane 0 + curve to lane 1.
    expect(segments.length).toBeGreaterThanOrEqual(2);
    expect(segments.some((s) => s.d.includes('C'))).toBe(true);
    expect(segments.every((s) => s.d.startsWith('M'))).toBe(true);
  });

  it('handles an empty log', () => {
    expect(computeLanes([], [])).toEqual([]);
  });
});

describe('GitPanel (read-only)', () => {
  let fixture: ComponentFixture<GitPanel>;
  let engine: {
    connected: jasmine.Spy;
    gitStatus: jasmine.Spy;
    gitBranches: jasmine.Spy;
    gitLog: jasmine.Spy;
    gitShow: jasmine.Spy;
    gitStashList: jasmine.Spy;
    gitConflicts: jasmine.Spy;
    gitRemotes: jasmine.Spy;
    gitTools: jasmine.Spy;
    gitReset: jasmine.Spy;
    gitCommit: jasmine.Spy;
  };
  let sessions: { meta: unknown; directory: unknown; running: unknown };
  let projects: { findByPath: jasmine.Spy; refresh: jasmine.Spy };

  beforeEach(async () => {
    engine = {
      connected: jasmine.createSpy('connected').and.returnValue(true),
      gitStatus: jasmine.createSpy('gitStatus').and.resolveTo(STATUS),
      gitBranches: jasmine.createSpy('gitBranches').and.resolveTo(BRANCHES),
      gitLog: jasmine.createSpy('gitLog').and.resolveTo(LOG),
      gitShow: jasmine
        .createSpy('gitShow')
        .and.resolveTo({ hash: 'abcdef0123456789', subject: 'feat: add git panel', diff: 'diff --git a/x b/x', files: [] }),
      gitStashList: jasmine.createSpy('gitStashList').and.resolveTo({ stashes: [] }),
      gitConflicts: jasmine.createSpy('gitConflicts').and.resolveTo({ conflicts: [] }),
      gitRemotes: jasmine
        .createSpy('gitRemotes')
        .and.resolveTo({ remotes: [{ name: 'origin', url: 'https://example.com/x.git' }] }),
      gitTools: jasmine
        .createSpy('gitTools')
        .and.resolveTo({ tools: { git: true, gh: false, glab: false } }),
      gitReset: jasmine.createSpy('gitReset').and.resolveTo({ ok: true }),
      gitCommit: jasmine.createSpy('gitCommit').and.resolveTo({ hash: 'ffee1122334455' }),
    };
    sessions = { meta: signal(META), directory: signal('/p'), running: signal(false) };
    projects = {
      findByPath: jasmine.createSpy('findByPath').and.callFake((p: string | null) => (p === '/p' ? PROJECT : null)),
      refresh: jasmine.createSpy('refresh').and.resolveTo(undefined),
    };

    await TestBed.configureTestingModule({
      imports: [GitPanel],
      providers: [
        provideZonelessChangeDetection(),
        { provide: EngineClient, useValue: engine },
        { provide: EventsStore, useValue: { onEvent: () => () => undefined } },
        { provide: ChatSessionStore, useValue: sessions },
        { provide: ProjectsStore, useValue: projects },
      ],
    }).compileComponents();

    fixture = TestBed.createComponent(GitPanel);
    fixture.detectChanges();
    await settle();
  });

  /** Zoneless: an awaited fetch lands on a microtask, not on app stability. */
  async function settle(): Promise<void> {
    await fixture.whenStable();
    await Promise.resolve();
    fixture.detectChanges();
    await Promise.resolve();
    fixture.detectChanges();
  }

  it('reads status, branches and log for the session project', () => {
    expect(engine.gitStatus).toHaveBeenCalledWith('p1');
    expect(engine.gitBranches).toHaveBeenCalledWith('p1');
    expect(engine.gitLog).toHaveBeenCalledWith('p1', 50);
    // `both.ts` is half-staged, so it appears in both the staged and unstaged lists.
    expect(fixture.debugElement.queryAll(By.css('[data-testid="git-file-row"]')).length).toBe(5);
    expect(fixture.debugElement.queryAll(By.css('[data-testid="git-branch-row"]')).length).toBe(2);
    expect(fixture.debugElement.queryAll(By.css('[data-testid="git-log-row"]')).length).toBe(1);
  });

  it('maps a worktree session directory back to its owning project', async () => {
    projects.findByPath.calls.reset();
    (sessions.directory as unknown as { set(v: string): void }).set('/p/.bebok/worktrees/feat');
    await settle();
    expect(projects.findByPath).toHaveBeenCalledWith('/p');
    expect(engine.gitStatus).toHaveBeenCalledWith('p1');
  });

  it('opens the commit diff in the overlay on a log row click', async () => {
    const row = fixture.debugElement.query(By.css('[data-testid="git-log-row"]'));
    row.nativeElement.click();
    await settle();
    expect(engine.gitShow).toHaveBeenCalledWith('p1', 'abcdef0123456789');
    expect(fixture.debugElement.query(By.css('[data-testid="git-show"]'))).not.toBeNull();
  });

  it('disables the commit button while the message is empty', async () => {
    const button = (): HTMLButtonElement =>
      fixture.debugElement.query(By.css('[data-testid="git-commit-button"]')).nativeElement;
    // No message and nothing staged in the textarea -> disabled.
    expect(button().disabled).toBeTrue();

    const textarea = fixture.debugElement.query(By.css('[data-testid="git-commit-message"]'))
      .nativeElement as HTMLTextAreaElement;
    textarea.value = 'feat: something';
    textarea.dispatchEvent(new Event('input'));
    await settle();
    // Something is staged (`src/staged.ts`, `both.ts`), so it enables.
    expect(button().disabled).toBeFalse();

    textarea.value = '   ';
    textarea.dispatchEvent(new Event('input'));
    await settle();
    expect(button().disabled).toBeTrue();
  });

  it('blocks reset --hard until the confirm checkbox is ticked', async () => {
    const mode = fixture.debugElement.query(By.css('[data-testid="git-reset-mode"]'))
      .nativeElement as HTMLSelectElement;
    mode.value = 'hard';
    mode.dispatchEvent(new Event('change'));
    await settle();

    const runButton = (): HTMLButtonElement =>
      fixture.debugElement.query(By.css('[data-testid="git-reset-run"]')).nativeElement;
    expect(runButton().disabled).toBeTrue();

    const confirm = fixture.debugElement.query(By.css('[data-testid="git-reset-confirm"]'))
      .nativeElement as HTMLInputElement;
    expect(confirm).not.toBeNull();
    confirm.checked = true;
    confirm.dispatchEvent(new Event('change'));
    await settle();
    expect(runButton().disabled).toBeFalse();

    runButton().click();
    await settle();
    expect(engine.gitReset).toHaveBeenCalledWith('p1', 'hard', 'HEAD', true);
  });
});
