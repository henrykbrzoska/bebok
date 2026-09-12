import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { DirectoryPicker } from '../../core/directory-picker.service';
import { ProjectEntry } from '../../core/engine.dtos';
import { ProjectsStore } from '../../core/projects.store';
import { ProjectSessionsStore } from '../shell/project-sessions.store';
import { ShellStore } from '../shell/shell.store';
import { ProjectSwitcher } from './project-switcher';

const ALPHA: ProjectEntry = {
  id: 'alpha', name: 'Alpha', path: 'C:\\work\\alpha', added_at: 1, last_opened_at: 1, pinned: false,
};
const BETA: ProjectEntry = {
  id: 'beta', name: 'Beta', path: 'C:\\work\\beta', added_at: 2, last_opened_at: null, pinned: true,
};

describe('ProjectSwitcher (F5-6)', () => {
  let fixture: ComponentFixture<ProjectSwitcher>;
  let shell: { projectSwitcherOpen: ReturnType<typeof signal<boolean>>; closeProjectSwitcher: jasmine.Spy };
  let projects: {
    projects: ReturnType<typeof signal<ProjectEntry[]>>;
    refresh: jasmine.Spy;
    findByPath: jasmine.Spy;
    open: jasmine.Spy;
    togglePinned: jasmine.Spy;
    remove: jasmine.Spy;
    add: jasmine.Spy;
  };
  let projectSessions: { directory: ReturnType<typeof signal<string | null>>; select: jasmine.Spy };

  beforeEach(() => {
    shell = { projectSwitcherOpen: signal(true), closeProjectSwitcher: jasmine.createSpy('closeProjectSwitcher') };
    projects = {
      projects: signal([ALPHA, BETA]),
      refresh: jasmine.createSpy('refresh').and.resolveTo(undefined),
      findByPath: jasmine.createSpy('findByPath').and.callFake((path: string | null) =>
        [ALPHA, BETA].find((entry) => entry.path === path) ?? null),
      open: jasmine.createSpy('open').and.callFake((id: string) =>
        Promise.resolve(id === BETA.id ? BETA.path : ALPHA.path)),
      togglePinned: jasmine.createSpy('togglePinned').and.resolveTo(undefined),
      remove: jasmine.createSpy('remove').and.resolveTo(undefined),
      add: jasmine.createSpy('add').and.resolveTo(null),
    };
    projectSessions = { directory: signal(ALPHA.path), select: jasmine.createSpy('select').and.resolveTo(undefined) };

    TestBed.configureTestingModule({
      imports: [ProjectSwitcher],
      providers: [
        provideZonelessChangeDetection(),
        { provide: ShellStore, useValue: shell },
        { provide: ProjectsStore, useValue: projects },
        { provide: ProjectSessionsStore, useValue: projectSessions },
        { provide: DirectoryPicker, useValue: { pick: jasmine.createSpy('pick') } },
      ],
    });
    fixture = TestBed.createComponent(ProjectSwitcher);
    fixture.detectChanges();
  });

  it('renders registered projects and their paths', () => {
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('Alpha');
    expect(text).toContain('C:\\work\\beta');
  });

  it('persists a pin toggle for the selected row', async () => {
    await fixture.componentInstance.togglePinned(ALPHA);
    expect(projects.togglePinned).toHaveBeenCalledWith(ALPHA.id);
  });

  it('removes only after the user confirms', async () => {
    spyOn(window, 'confirm').and.returnValue(false);
    await fixture.componentInstance.remove(BETA);
    expect(projects.remove).not.toHaveBeenCalled();

    (window.confirm as jasmine.Spy).and.returnValue(true);
    await fixture.componentInstance.remove(BETA);
    expect(projects.remove).toHaveBeenCalledWith(BETA.id);
  });

  it('switches the project directory and refreshes the sidebar-scoped sessions', async () => {
    await fixture.componentInstance.select(BETA);

    expect(projects.open).toHaveBeenCalledWith(BETA.id);
    expect(projectSessions.select).toHaveBeenCalledWith(BETA.path, true);
    expect(shell.closeProjectSwitcher).toHaveBeenCalled();
  });
});
