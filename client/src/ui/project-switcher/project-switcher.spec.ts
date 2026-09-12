import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { DirectoryPicker } from '../../core/directory-picker.service';
import { ProjectEntry } from '../../core/engine.dtos';
import { ProjectGroup } from '../../core/projects.store';
import { ProjectsStore } from '../../core/projects.store';
import { ProjectSessionsStore } from '../shell/project-sessions.store';
import { ShellStore } from '../shell/shell.store';
import { ProjectSwitcher } from './project-switcher';

const ALPHA: ProjectEntry = {
  id: 'alpha', name: 'Alpha', path: 'C:\\work\\alpha', added_at: 1, last_opened_at: 1, pinned: false,
};
const BETA: ProjectEntry = {
  id: 'beta', name: 'Beta', path: 'C:\\work\\beta', added_at: 2, last_opened_at: null, pinned: true, group: 'Backend',
};
const GAMMA: ProjectEntry = {
  id: 'gamma', name: 'Gamma', path: 'C:\\work\\gamma', added_at: 3, last_opened_at: null, pinned: false, group: 'Frontend',
};
const ALL = [ALPHA, BETA, GAMMA];

/** Mirrors `ProjectsStore.groups()`'s ordering contract for the mock. */
const GROUPS: ProjectGroup[] = [
  { name: 'Backend', projects: [BETA] },
  { name: 'Frontend', projects: [GAMMA] },
  { name: null, projects: [ALPHA] },
];

describe('ProjectSwitcher (F5-6/F6-7)', () => {
  let fixture: ComponentFixture<ProjectSwitcher>;
  let shell: { projectSwitcherOpen: ReturnType<typeof signal<boolean>>; closeProjectSwitcher: jasmine.Spy };
  let projects: {
    projects: ReturnType<typeof signal<ProjectEntry[]>>;
    groups: ReturnType<typeof signal<ProjectGroup[]>>;
    distinctGroupNames: jasmine.Spy;
    refresh: jasmine.Spy;
    findByPath: jasmine.Spy;
    open: jasmine.Spy;
    togglePinned: jasmine.Spy;
    remove: jasmine.Spy;
    add: jasmine.Spy;
    moveToGroup: jasmine.Spy;
  };
  let projectSessions: { directory: ReturnType<typeof signal<string | null>>; select: jasmine.Spy };

  beforeEach(() => {
    shell = { projectSwitcherOpen: signal(true), closeProjectSwitcher: jasmine.createSpy('closeProjectSwitcher') };
    projects = {
      projects: signal(ALL),
      groups: signal(GROUPS),
      distinctGroupNames: jasmine.createSpy('distinctGroupNames').and.returnValue(['Backend', 'Frontend']),
      refresh: jasmine.createSpy('refresh').and.resolveTo(undefined),
      findByPath: jasmine.createSpy('findByPath').and.callFake((path: string | null) =>
        ALL.find((entry) => entry.path === path) ?? null),
      open: jasmine.createSpy('open').and.callFake((id: string) =>
        Promise.resolve(ALL.find((entry) => entry.id === id)?.path ?? null)),
      togglePinned: jasmine.createSpy('togglePinned').and.resolveTo(undefined),
      remove: jasmine.createSpy('remove').and.resolveTo(undefined),
      add: jasmine.createSpy('add').and.resolveTo(null),
      moveToGroup: jasmine.createSpy('moveToGroup').and.resolveTo(undefined),
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

  it('renders collapsible group sections, with the ungrouped bucket last', () => {
    const headers = (fixture.nativeElement as HTMLElement).querySelectorAll('.group-header');
    const headerText = Array.from(headers).map((h) => h.textContent?.trim());
    expect(headerText[0]).toContain('Backend');
    expect(headerText[1]).toContain('Frontend');
    expect(headerText[2]).toContain('Ungrouped');

    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('Alpha');
    expect(text).toContain('Beta');
    expect(text).toContain('Gamma');
  });

  it('collapses and expands a group', () => {
    const component = fixture.componentInstance;
    expect(component.isCollapsed('Backend')).toBe(false);

    component.toggleGroup('Backend');
    fixture.detectChanges();
    expect(component.isCollapsed('Backend')).toBe(true);
    expect((fixture.nativeElement as HTMLElement).textContent ?? '').not.toContain('Beta');

    component.toggleGroup('Backend');
    fixture.detectChanges();
    expect(component.isCollapsed('Backend')).toBe(false);
    expect((fixture.nativeElement as HTMLElement).textContent ?? '').toContain('Beta');
  });

  it('moves a project to a new group on confirm, and skips the round-trip when nothing changed', async () => {
    const component = fixture.componentInstance;

    component.startMoveGroup(ALPHA);
    expect(component.editingGroupId()).toBe(ALPHA.id);
    expect(component.groupDraft()).toBe('');

    component.groupDraft.set('Design');
    await component.confirmMoveGroup(ALPHA);
    expect(projects.moveToGroup).toHaveBeenCalledWith(ALPHA.id, 'Design');
    expect(component.editingGroupId()).toBeNull();

    projects.moveToGroup.calls.reset();
    component.startMoveGroup(BETA);
    component.groupDraft.set(BETA.group as string);
    await component.confirmMoveGroup(BETA);
    expect(projects.moveToGroup).not.toHaveBeenCalled();
  });

  it('cancels the move-to-group edit without calling the engine', () => {
    const component = fixture.componentInstance;
    component.startMoveGroup(ALPHA);
    component.groupDraft.set('Design');
    component.cancelMoveGroup();
    expect(component.editingGroupId()).toBeNull();
    expect(projects.moveToGroup).not.toHaveBeenCalled();
  });
});
