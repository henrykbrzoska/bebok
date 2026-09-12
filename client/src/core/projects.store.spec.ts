/**
 * F5-3/F5-4: the one-time last-directory migration, `recent`'s sort+cap, and
 * the directory-picker platform strategy.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { DirectoryPicker } from './directory-picker.service';
import { EngineClient } from './engine-client.service';
import { ProjectEntry } from './engine.dtos';
import { ProjectsStore } from './projects.store';

function entry(
  id: string,
  lastOpened: number | null,
  name = id,
  extra: Partial<Pick<ProjectEntry, 'group' | 'pinned'>> = {},
): ProjectEntry {
  return {
    id,
    name,
    path: `/tmp/${id}`,
    added_at: 1,
    last_opened_at: lastOpened,
    pinned: false,
    ...extra,
  };
}

describe('ProjectsStore (F5-3)', () => {
  let engine: jasmine.SpyObj<EngineClient>;

  function setup(projects: ProjectEntry[], lastDirectory: string | null): ProjectsStore {
    engine = jasmine.createSpyObj<EngineClient>(
      'EngineClient',
      ['listProjects', 'addProject', 'readLastDirectory', 'connected'],
    );
    engine.listProjects.and.returnValue(Promise.resolve(projects));
    engine.addProject.and.callFake((path: string) =>
      Promise.resolve(entry('migrated', null, path)),
    );
    engine.readLastDirectory.and.returnValue(lastDirectory);
    (engine.connected as unknown as jasmine.Spy).and.returnValue(true);

    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        { provide: EngineClient, useValue: engine },
      ],
    });
    return TestBed.inject(ProjectsStore);
  }

  it('migrates the remembered last directory into an empty registry', async () => {
    const store = setup([], '/tmp/remembered');
    await store.refresh();
    expect(engine.addProject).toHaveBeenCalledWith('/tmp/remembered');
  });

  it('does not migrate when the registry already has entries', async () => {
    const store = setup([entry('a', 10)], '/tmp/remembered');
    await store.refresh();
    expect(engine.addProject).not.toHaveBeenCalled();
  });

  it('sorts recent by last_opened_at and caps it at five', async () => {
    const many = [
      entry('never', null),
      entry('p1', 1),
      entry('p2', 2),
      entry('p3', 3),
      entry('p4', 4),
      entry('p5', 5),
      entry('p6', 6),
    ];
    const store = setup(many, null);
    await store.refresh();
    expect(store.recent().map((p) => p.id)).toEqual(['p6', 'p5', 'p4', 'p3', 'p2']);
  });

  it('groups() sorts named groups alphabetically, keeps pinned/recent order within a group, and puts "Ungrouped" last', async () => {
    // Already in the registry's own pinned -> recent -> name order (as the
    // server would return it) - groups() must not re-sort within a bucket.
    const projects = [
      entry('pinned', null, 'pinned', { group: 'Zeta', pinned: true }),
      entry('newer', 20, 'newer', { group: 'Zeta' }),
      entry('older', 10, 'older', { group: 'Zeta' }),
      entry('never', null, 'never', { group: 'Zeta' }),
      entry('alpha-item', 5, 'alpha-item', { group: 'alpha' }),
      entry('no-group-a', 1, 'no-group-a'),
      entry('no-group-b', null, 'no-group-b', { group: '   ' }),
    ];
    const store = setup(projects, null);
    await store.refresh();

    const groups = store.groups();
    expect(groups.map((g) => g.name)).toEqual(['alpha', 'Zeta', null]);

    const zeta = groups.find((g) => g.name === 'Zeta')!;
    // Same pinned -> recent -> name order the registry itself returns, just filtered into the bucket.
    expect(zeta.projects.map((p) => p.id)).toEqual(['pinned', 'newer', 'older', 'never']);

    const ungrouped = groups.find((g) => g.name === null)!;
    expect(ungrouped.projects.map((p) => p.id).sort()).toEqual(['no-group-a', 'no-group-b']);
  });

  it('distinctGroupNames() lists only named groups, alphabetically', async () => {
    const store = setup(
      [
        entry('a', null, 'a', { group: 'Backend' }),
        entry('b', null, 'b', { group: 'frontend' }),
        entry('c', null, 'c'),
      ],
      null,
    );
    await store.refresh();
    expect(store.distinctGroupNames()).toEqual(['Backend', 'frontend']);
  });
});

describe('DirectoryPicker (F5-4)', () => {
  function setup(platform: string): DirectoryPicker {
    const engine = {
      platform,
      pickDirectory: jasmine
        .createSpy('pickDirectory')
        .and.returnValue(Promise.resolve('/native/path')),
    };
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        { provide: EngineClient, useValue: engine },
      ],
    });
    return TestBed.inject(DirectoryPicker);
  }

  it('delegates to the native dialog on Tauri', async () => {
    const picker = setup('tauri');
    await expectAsync(picker.pick('title')).toBeResolvedTo('/native/path');
    expect(picker.request()).toBeNull();
  });

  it('falls back to the in-app picker when the Tauri dialog is unavailable', async () => {
    const picker = setup('tauri');
    const engine = TestBed.inject(EngineClient) as unknown as { pickDirectory: jasmine.Spy };
    engine.pickDirectory.and.returnValue(Promise.reject(new Error('dialog capability unavailable')));
    const pending = picker.pick('title');
    await Promise.resolve();
    expect(picker.request()?.title).toBe('title');
    picker.resolve('  C:\\work  ');
    await expectAsync(pending).toBeResolvedTo('C:\\work');
  });

  it('resolves with the path the in-app browser confirms elsewhere', async () => {
    const picker = setup('http');
    const pending = picker.pick('title');
    expect(picker.request()?.title).toBe('title');
    picker.resolve('/browsed/path');
    await expectAsync(pending).toBeResolvedTo('/browsed/path');
    expect(picker.request()).toBeNull();
  });

  it('resolves with null when the browser modal is cancelled', async () => {
    const picker = setup('http');
    const pending = picker.pick('title');
    picker.resolve(null);
    await expectAsync(pending).toBeResolvedTo(null);
  });
});
