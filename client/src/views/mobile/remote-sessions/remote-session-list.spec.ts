/**
 * WP-M6 (F10-24): the remote session list - no target / empty / populated /
 * cached-offline states.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../../core/engine-client.service';
import { EngineTargetStore } from '../../../core/engine-target.store';
import { ProjectEntry, SessionMeta } from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { MemoryCacheStore, OFFLINE_CACHE_STORE, OfflineCache } from '../../../core/remote/offline-cache';
import { RemoteSessionList, relativeTime } from './remote-session-list';

const PROJECTS: ProjectEntry[] = [
  { id: 'p1', name: 'Alpha', path: 'C:/alpha', added_at: 0, last_opened_at: null, pinned: false },
  { id: 'p2', name: 'Beta', path: 'C:/beta', added_at: 0, last_opened_at: null, pinned: false },
];

function meta(id: string, directory: string, updated: number, extra: Partial<SessionMeta> = {}): SessionMeta {
  return {
    id,
    directory,
    agent: 'code',
    created_at: updated,
    updated_at: updated,
    usage: { input_tokens: 0, output_tokens: 0 },
    ...extra,
  };
}

describe('RemoteSessionList (WP-M6 / F10-24)', () => {
  let fixture: ComponentFixture<RemoteSessionList>;
  let engine: {
    connected: ReturnType<typeof signal<boolean>>;
    listProjects: jasmine.Spy;
    listSessions: jasmine.Spy;
  };
  let events: { onEvent: jasmine.Spy; reconnectVersion: ReturnType<typeof signal<number>> };
  let targets: EngineTargetStore;
  let cacheStore: MemoryCacheStore;

  beforeEach(() => {
    localStorage.clear();
    engine = {
      connected: signal(true),
      listProjects: jasmine.createSpy('listProjects').and.resolveTo(PROJECTS),
      listSessions: jasmine.createSpy('listSessions').and.callFake(async (dir: string) =>
        dir === 'C:/alpha'
          ? [meta('a1', 'C:/alpha', 100, { title: 'Fix login', running: true }), meta('a2', 'C:/alpha', 50)]
          : [meta('b1', 'C:/beta', 75, { alias: 'api-orders' }), meta('b-child', 'C:/beta', 76, { parent: ['b1', 3] })],
      ),
    };
    events = {
      onEvent: jasmine.createSpy('onEvent').and.returnValue(() => undefined),
      reconnectVersion: signal(0),
    };
    cacheStore = new MemoryCacheStore();
    TestBed.configureTestingModule({
      imports: [RemoteSessionList],
      providers: [
        provideZonelessChangeDetection(),
        { provide: EngineClient, useValue: engine },
        { provide: EventsStore, useValue: events },
        { provide: OFFLINE_CACHE_STORE, useValue: cacheStore },
      ],
    });
    targets = TestBed.inject(EngineTargetStore);
  });

  afterEach(() => {
    fixture?.destroy();
    localStorage.clear();
  });

  async function settle(): Promise<void> {
    await new Promise((resolve) => setTimeout(resolve, 20));
    fixture.detectChanges();
  }

  function create(): void {
    fixture = TestBed.createComponent(RemoteSessionList);
    fixture.detectChanges();
  }

  function rows(): HTMLElement[] {
    return Array.from(
      (fixture.nativeElement as HTMLElement).querySelectorAll('button.session[data-testid^="remote-session-"]'),
    );
  }

  it('no active target: nothing fetched, empty state', async () => {
    create();
    await settle();
    expect(engine.listProjects).not.toHaveBeenCalled();
    expect((fixture.nativeElement as HTMLElement).querySelector('[data-testid="remote-list-empty"]')).not.toBeNull();
  });

  it('populated: union of GET /session per project, newest first, children hidden', async () => {
    targets.upsert({ id: 'desktop:1', kind: 'desktop', label: 'd', baseUrl: 'http://1', token: 't' });
    targets.setActive('desktop:1');
    create();
    await settle();
    expect(engine.listSessions.calls.allArgs()).toEqual([['C:/alpha'], ['C:/beta']]);
    const list = rows();
    expect(list.map((r) => r.getAttribute('data-testid'))).toEqual([
      'remote-session-a1',
      'remote-session-b1',
      'remote-session-a2',
    ]);
    expect(list[0].textContent).toContain('Fix login');
    expect(list[0].textContent).toContain('Alpha');
    expect(list[0].classList).toContain('running');
    expect(list[1].textContent).toContain('api-orders');
    // The list was cached for offline use.
    expect(await cacheStore.keys('sessions:desktop:1')).toEqual(['sessions:desktop:1']);
  });

  it('project chips filter the rows', async () => {
    targets.upsert({ id: 'desktop:1', kind: 'desktop', label: 'd', baseUrl: 'http://1', token: 't' });
    targets.setActive('desktop:1');
    create();
    await settle();
    (fixture.nativeElement as HTMLElement)
      .querySelector<HTMLButtonElement>('[data-testid="remote-filter-p2"]')!
      .click();
    fixture.detectChanges();
    expect(rows().map((r) => r.getAttribute('data-testid'))).toEqual(['remote-session-b1']);
  });

  it('empty: a desktop without sessions', async () => {
    engine.listSessions.and.resolveTo([]);
    targets.upsert({ id: 'desktop:1', kind: 'desktop', label: 'd', baseUrl: 'http://1', token: 't' });
    targets.setActive('desktop:1');
    create();
    await settle();
    expect(rows().length).toBe(0);
    expect((fixture.nativeElement as HTMLElement).querySelector('[data-testid="remote-list-empty"]')).not.toBeNull();
  });

  it('offline: falls back to the cached list and says so', async () => {
    targets.upsert({ id: 'desktop:1', kind: 'desktop', label: 'd', baseUrl: 'http://1', token: 't' });
    targets.setActive('desktop:1');
    await TestBed.inject(OfflineCache).putSessions('desktop:1', [meta('cached-1', 'C:/alpha', 10, { title: 'Old' })]);
    engine.listProjects.and.rejectWith(new TypeError('Failed to fetch'));
    create();
    await settle();
    await settle();
    expect(rows().map((r) => r.getAttribute('data-testid'))).toEqual(['remote-session-cached-1']);
    expect((fixture.nativeElement as HTMLElement).querySelector('[data-testid="remote-list-cached"]')).not.toBeNull();
    expect(fixture.componentInstance.state()).toBe('cached');
  });

  it('emits the tapped session id', async () => {
    targets.upsert({ id: 'desktop:1', kind: 'desktop', label: 'd', baseUrl: 'http://1', token: 't' });
    targets.setActive('desktop:1');
    create();
    await settle();
    const opened: string[] = [];
    fixture.componentInstance.open.subscribe((id) => opened.push(id));
    rows()[0].click();
    expect(opened).toEqual(['a1']);
  });

  it('relativeTime buckets', () => {
    const now = 10_000_000;
    expect(relativeTime(now - 30_000, now)).toBe('now');
    expect(relativeTime(now - 5 * 60_000, now)).toBe('5m');
    expect(relativeTime(now - 3 * 3_600_000, now)).toBe('3h');
    expect(relativeTime(now - 2 * 86_400_000, now)).toBe('2d');
  });
});
